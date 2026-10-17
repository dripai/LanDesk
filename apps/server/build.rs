use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let icons = std::path::Path::new("../../assets/icons");
    println!("cargo:rerun-if-changed={}", icons.display());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile_for_everything(
            icons.join("LanDesk.rc"),
            embed_resource::ParamsIncludeDirs([icons]),
        )
        .manifest_required()
        .expect("无法编译 LanDesk 应用图标资源");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    // The capture bindings only add the full-Xcode Swift library directory.
    // Resolve the actual installed toolchain, including Command Line Tools.
    let output = Command::new("xcrun")
        .args(["--find", "swiftc"])
        .output()
        .expect("需要 Apple Command Line Tools 的 Swift 编译器");
    assert!(output.status.success(), "无法定位 Swift 编译器");
    let compiler = PathBuf::from(
        String::from_utf8(output.stdout)
            .expect("Swift 路径不是 UTF-8")
            .trim(),
    );
    let directory = compiler
        .parent()
        .expect("Swift 编译器路径无效")
        .parent()
        .expect("Swift 工具链路径无效")
        .join("lib/swift/macosx");
    assert!(
        directory.join("libswiftCompatibility56.a").is_file(),
        "Swift 链接库缺失: {}",
        directory.display()
    );
    println!("cargo:rustc-link-search=native={}", directory.display());
}
