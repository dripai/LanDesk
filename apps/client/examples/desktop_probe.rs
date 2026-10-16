//! Explicit protocol smoke probe. Password comes from stdin, never command arguments.
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use http_body_util::BodyExt;
use http_body_util::Empty;
use hyper::Request;
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use landesk_client::adapters::gateway;
use landesk_client::adapters::tunnel;
use landesk_core::domain::connection::ConnectionProfile;
use std::io::Read;
use std::sync::mpsc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use zeroize::Zeroizing;

async fn get(
    address: std::net::SocketAddr,
    path: &str,
    cookie: Option<&str>,
) -> Result<(hyper::StatusCode, hyper::HeaderMap, String)> {
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(TcpStream::connect(address).await?))
            .await?;
    let driver = tokio::spawn(connection);
    let mut request = Request::builder()
        .uri(path)
        .header("host", address.to_string());
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let response = sender
        .send_request(request.body(Empty::<Bytes>::new())?)
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = String::from_utf8(response.into_body().collect().await?.to_bytes().to_vec())?;
    driver.abort();
    Ok((status, headers, body))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 2,
        "usage: desktop_probe HOST PORT (password on stdin)"
    );
    let mut password = Zeroizing::new(String::new());
    std::io::stdin().read_to_string(&mut password)?;
    let password = Zeroizing::new(password.trim_end_matches(['\r', '\n']).to_owned());
    let profile = ConnectionProfile {
        host: args[0].clone(),
        ssh_port: args[1].parse()?,
        ..Default::default()
    };
    let routes = gateway::Routes::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let endpoint = gateway::Endpoint::new(address)?;
    let (stop, stopped) = oneshot::channel();
    let gateway = tokio::spawn(gateway::run(listener, routes.clone(), stopped));
    let (cancel, cancelled) = oneshot::channel();
    let (events, receiver) = mpsc::channel();
    let settings = profile.clone();
    let tunnel_routes = routes.clone();
    let task = tokio::spawn(tunnel::run(
        settings,
        password,
        events,
        cancelled,
        tunnel_routes,
    ));
    timeout(Duration::from_secs(15), async {
        loop {
            if matches!(
                receiver.try_recv(),
                Ok(landesk_core::domain::connection::Event::Ready)
            ) {
                break;
            }
            ensure!(!task.is_finished(), "SSH subsystem failed before ready");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let url = url::Url::parse(&endpoint.viewer_url(&routes, &profile)?)?;
        let (status, headers, _) = get(
            address,
            &format!(
                "{}?{}",
                url.path(),
                url.query().context("missing launch credential")?
            ),
            None,
        )
        .await?;
        ensure!(status == 303, "launch credential rejected");
        let cookie = headers["set-cookie"]
            .to_str()?
            .split(';')
            .next()
            .context("missing session cookie")?;
        let path = url.path();
        let (status, _, html) = get(address, path, Some(cookie)).await?;
        ensure!(
            status == 200 && html.contains("id=\"connect-form\""),
            "desktop page unavailable"
        );
        ensure!(
            get(address, &format!("{path}app.js"), Some(cookie))
                .await?
                .0
                == 200,
            "desktop assets unavailable"
        );
        ensure!(
            get(address, path, None).await?.0 == 401,
            "unauthenticated browser access accepted"
        );
        let mut request = format!("ws://{address}{path}ws").into_client_request()?;
        request
            .headers_mut()
            .insert("origin", format!("http://{address}").parse()?);
        request.headers_mut().insert("cookie", cookie.parse()?);
        let (mut socket, _) = connect_async(request).await?;
        socket.close(None).await?;
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    let _ = cancel.send(());
    timeout(Duration::from_secs(5), task).await???;
    ensure!(
        routes.read().unwrap().is_empty(),
        "route survived disconnect"
    );
    let _ = stop.send(());
    gateway.await??;
    println!(
        "PASS: SSH subsystem, memory backend, browser credential exchange, assets, WebSocket, disconnect cleanup"
    );
    Ok(())
}
