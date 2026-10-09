use std::{path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        virtual_display();
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

fn virtual_display() {
    use sha2::{Digest, Sha256};
    assert_eq!(
        std::env::var("CARGO_CFG_TARGET_ARCH").as_deref(),
        Ok("x86_64"),
        "内置虚拟显示驱动的发布包为 Windows x64"
    );
    let output = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    for (name, hash) in [
        (
            "MttVDD.inf",
            "550d211fe481e74dfe3f9d724ed78be48b3a9113405965d683d9373e8d672f5d",
        ),
        (
            "MttVDD.dll",
            "c9ca837f57a98fbd43bc416a7f535a95843626e7759eaf85cf0cd7ce334dbb05",
        ),
        (
            "mttvdd.cat",
            "08a0093fc9b2e32b287a6f8a77ca4de0a31830d29fc33d2b13a918dc859468f6",
        ),
    ] {
        let path = PathBuf::from("vendor/virtual-display").join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = std::fs::read(&path)
            .expect("缺少虚拟显示驱动；先运行 python scripts/prepare_virtual_display.py");
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            hash,
            "虚拟显示驱动文件校验失败：{name}"
        );
        std::fs::write(output.join(name), bytes).expect("无法准备内置虚拟显示驱动");
    }
}
