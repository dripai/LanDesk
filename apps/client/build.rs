fn main() {
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
}
