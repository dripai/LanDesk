//! Developer launcher for testing the same terminal core without the Windows UI.
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use landesk_client::adapters::gateway;
use landesk_client::adapters::tunnel;
use landesk_core::domain::connection::Authentication;
use landesk_core::domain::connection::ConnectionMode;
use landesk_core::domain::connection::ConnectionProfile;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use tokio::sync::oneshot;
use zeroize::Zeroizing;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 5,
        "usage: terminal_gateway HOST USER PORT KEY_PATH KNOWN_HOSTS\nUse - for KEY_PATH to authenticate with LANDESK_SSH_SECRET. Press Enter to stop."
    );
    let settings = ConnectionProfile {
        name: format!("{}@{}", args[1], args[0]),
        host: args[0].clone(),
        user: args[1].clone(),
        ssh_port: args[2].parse()?,
        mode: ConnectionMode::Terminal,
        authentication: if args[3] == "-" {
            Authentication::Password
        } else {
            Authentication::PrivateKey {
                path: PathBuf::from(&args[3]),
            }
        },
        ..Default::default()
    };
    settings.validate()?;
    let secret = Zeroizing::new(std::env::var("LANDESK_SSH_SECRET").unwrap_or_default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("无法启动本机网页入口")?;
    let endpoint = gateway::Endpoint::new(listener.local_addr()?)?;
    let routes = gateway::Routes::default();
    let (stop, stopped) = oneshot::channel();
    let gateway = tokio::spawn(gateway::run(listener, routes.clone(), stopped));
    let (cancel, cancelled) = oneshot::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let _ = cancel.send(());
    });
    let (events, receiver) = mpsc::channel();
    let task = tunnel::run(
        settings.clone(),
        secret,
        PathBuf::from(&args[4]),
        events,
        cancelled,
        routes.clone(),
    );
    tokio::pin!(task);
    let mut tick = tokio::time::interval(Duration::from_millis(30));
    let result = loop {
        tokio::select! {
            result = &mut task => break result,
            _ = tick.tick() => while let Ok(event) = receiver.try_recv() {
                match event { landesk_core::domain::connection::Event::Ready => println!("{}", endpoint.viewer_url(&routes, &settings)?), landesk_core::domain::connection::Event::Status(status) => eprintln!("{status}"), _ => {} }
            }
        }
    };
    let _ = stop.send(());
    gateway.await??;
    eprintln!("{:?}", result?);
    Ok(())
}
