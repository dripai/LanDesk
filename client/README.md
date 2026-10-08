# LanDeskClient

Windows 10 / 11 x64 客户端，用 GPUI Kit 0.7.1（GPUI / gpui-component）管理 SSH 加密隧道，远程桌面使用默认浏览器。托盘基于 tray-icon 0.21.2；最小化与恢复使用 Win32 窗口 API。

1. 从本仓库 Actions 的 **Windows client** 成功运行中下载 `LanDeskClient-windows-x64`，解压后运行 `LanDeskClient.exe`。
2. 填写 Mac 地址、用户名、SSH 端口和密码。Mac 应先开启“远程登录”，并运行已获屏幕录制和辅助功能权限的 LanDesk。
3. 首次连接自动记录 Mac 的 SSH 主机密钥，不显示指纹确认。后续密钥变化会中止连接，防止误连其他主机；不会自动覆盖原记录。
4. 浏览器打开后输入 Mac LanDesk 窗口里的六位连接码。
5. 默认最小化或关闭客户端窗口时留在托盘，隧道继续运行。单击托盘图标恢复窗口，右键可断开或退出。关闭浏览器只结束远控会话；客户端“断开”或托盘“退出”才停止 SSH 隧道。

地址、用户名、端口、自动打开浏览器和托盘偏好保存在 `%APPDATA%\LanDeskClient\settings.json`；信任过的主机保存在同目录 `known_hosts`。密码框下的“记住密码”在连接或保存设置时生效。勾选后密码写入当前用户的 Windows 凭据管理器，绑定 Mac 地址、SSH 端口和用户名；下次密码框可留空。取消勾选并保存或连接后会删除已保存密码。密码不写入 settings.json，连接时清空输入框。当前使用 SSH 密码认证，不支持密钥、跳板机或 SSH 主机证书。

本地地址固定为 `127.0.0.1:17890`，Mac 目标同样为 `127.0.0.1:17890`，以匹配浏览器服务的来源检查。端口占用会明确报错，请先关闭旧的 `.cmd` 连接窗口。连接失败后不会自动重试密码。

构建需要 Rust stable MSVC、Visual Studio C++ 工具链、Windows SDK 和 CMake。执行：

```sh
cargo test --manifest-path client/Cargo.toml --locked --lib
cargo clippy --manifest-path client/Cargo.toml --locked --all-targets -- -D warnings
cargo build --manifest-path client/Cargo.toml --locked --release
```

Actions 的构建通过仅证明 Windows 编译及测试成功。托盘、GPU 渲染、Windows 中文输入和真实 Mac SSH 连接仍需要 Windows 实机验收。开发构建未做代码签名。

官方资料：[GPUI Kit 安装与平台要求](https://gpui-kit.com/docs/installation/)、[russh 0.64.1](https://docs.rs/russh/0.64.1/russh/)、[tray-icon 0.21.2](https://docs.rs/tray-icon/0.21.2/tray_icon/)、[Win32 ShowWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-showwindow)。

首次自动记录密钥采用首次使用信任（TOFU），无法验证第一次连接的主机身份。请首次连接自己的 Mac 地址。凭据使用 [Windows CredWriteW](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credwritew)；配置保存失败会回滚凭据修改，回滚失败会明确报错。
