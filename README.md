# LanDesk

- **LanDeskServer**：被控端，运行在 Mac 或 Windows 的已登录桌面。
- **LanDeskClient**：Windows 控制端，管理多个服务器，各自使用浏览器标签操作。

两端内置加密连接与认证，无需安装 OpenSSH、开启系统远程登录或使用系统账号密码。当前是 IP/主机名直连，尚未实现设备 ID 发现、NAT 穿透和中继。

## 使用

1. 被控电脑运行 `LanDeskServer.app`（Mac）或 `LanDeskServer.exe`（Windows），设置访问密码（至少 10 个字符），点击“保存并启动”。默认连接端口 **17891**。首次未设密码时不监听网络；之后启动自动加载配置并监听。
2. Mac 自行授予屏幕录制、辅助功能权限并重开。Windows 如显示防火墙提示，允许预期网络访问；LanDesk 不修改系统 SSH 配置或防火墙规则。
3. Windows 打开 [LanDeskClient](client/README.md)，添加服务器的 IP/主机名、连接端口和 **LanDesk 访问密码**，点击“连接”后自动打开网页。不填写系统用户名或密码。两端需要一起更新；旧版本不能连接新版服务端。
4. 每台服务器使用 `http://127.0.0.1:17890/s/<地址哈希>/` 独立标签。网页自动识别 Mac/Windows 能力，没有六位连接码。首次自动记录设备密钥，后续密钥变化拒绝连接。
5. 点击画面操作。连接 Mac 时 Ctrl 快捷键映射 Command；连接 Windows 时保留 Ctrl。支持方向键长按、中文与全角标点，松开或失焦释放按键。
6. 浮动工具条可拖动、收起，提供文字输入、剪贴板、文件面板、采集分辨率、全屏和断开。目录面板可调整宽度。
7. 文件范围限运行服务端的当前用户目录。上传普通文件，同名拒绝覆盖；单文件最多 512 MiB，64 KiB 分块。暂不支持目录上传、删除、下载和断点续传。
8. Ctrl+V 或粘贴按钮发送文字或单张图片；目标应用需要支持图片。取回剪贴板仍只支持文字，不自动同步。
9. 默认按屏幕实际像素采集、等比例显示；可选择采集宽度，不修改系统显示分辨率。当前仅支持一个显示器。
10. 关闭服务端窗口停止服务，最小化保留连接。同一服务端仅允许一个控制会话；不同电脑可同时连接。关闭网页或长时间不操作不再自动断开连接；重新打开原网页或点击“重新连接”即可恢复远控。网络中断后网页请求会重建加密连接；在客户端主动断开或退出后，需要先在客户端重新连接。

## 连接与配置

服务端监听 IPv4/IPv6，端口可设为 1024–65535，保留客户端网页入口 17890，默认 17891。两端可在同机运行。跨公网直连要求服务器地址、端口可达；路由器映射和网络防火墙仍由用户配置。

密码留空保存表示保留原密码。修改端口或密码需要先断开远控；保存成功关闭旧加密连接。新端口占用或保存失败时保留正在运行的配置。损坏配置明确报错，不生成替代身份。已存在的系统 SSH 服务和配置不会被改动。

服务端配置位于当前用户配置目录 `LanDeskServer/server.json`：Mac 为 `~/Library/Application Support/LanDeskServer/server.json`，Windows 为 `%APPDATA%\LanDeskServer\server.json`。包含 Ed25519 私钥与带随机盐的 Argon2id 密码校验值，不保存明文访问密码。Mac 文件权限为 0600；Windows 使用当前用户配置目录的访问权限。请勿公开此文件；删除它会重置身份，已有客户端将拒绝变化后的设备密钥。

采用项目锁定的 [russh 0.64.1](https://docs.rs/russh/0.64.1/russh/server/index.html) 内嵌服务端，复用 SSH 协议的加密和密码认证。应用不开放命令行、SFTP、系统账号登录或通用 TCP 转发；只允许认证后的 LanDesk HTTP/WebSocket 通道，服务端不另开明文 HTTP 监听。认证限制为每连接最多 3 次，失败延迟 2 秒，同时最多 2 个密码校验任务、32 条传输连接；未认证连接有超时。密码校验使用 [Argon2id 0.5.3](https://docs.rs/argon2/0.5.3/argon2/)。

客户端首次自动信任设备密钥，不能验证首次连接时是否连接到了预期设备；后续检查记录，不自动覆盖变化的密钥。配置删除或重装后的密钥变化需要用户核实并清理对应信任记录。

## 公共接口与系统实现

公共 Rust 库入口为 `src/lib.rs`，平台接口在 `src/platform/mod.rs`：

| 接口/模块 | 职责 |
|---|---|
| `HostPlatform` | 系统信息、权限、当前用户目录、原生界面与资源工厂 |
| `CaptureSession` | 画面流、像素和输入坐标尺寸、采集分辨率 |
| `InputController` / `SessionPower` | 键鼠操作与会话期间防止自动休眠 |
| `FileSystem` / `FileTransfer` | 受限目录访问与上传原子发布 |
| `desktop::ControlSession` | UI 线程命令、剪贴板、输入与电源资源统一释放 |
| `server` / `file_worker` | WebSocket、心跳、会话归属、文件超时和阻塞隔离 |

按 Rust `target_os` 编译对应平台实现。连接握手使用协议版本 1，服务端返回 `os` 与 `capabilities`；网页校验版本并启用支持的控件，缺少能力或版本不匹配明确报错。

Mac 保留 ScreenCaptureKit 11.0.0、Enigo 0.6.1、AppKit 剪贴板和 IOKit 电源断言。Windows 使用 XCap 0.8.3 的 GDI 截屏路径、Enigo 0.6.1、Arboard 3.6.1、Win32 原生窗口和电源 API；XCap 版本选取与现有 Mac 依赖兼容的版本。Windows 目录使用 cap-std/cap-fs-ext 4.0.3 固定目录句柄，拒绝路径穿越与跟随目录链接，上传通过 `SetFileInformationByHandle` 且 `ReplaceIfExists=false` 发布。Mac 保留 `openat/O_NOFOLLOW` 与 `renameatx_np(RENAME_EXCL)`。

文件访问使用独立工作线程，单次请求 10 秒超时，阻塞时画面和心跳继续。超时/断开仅取消请求及后续操作，不能强制中断系统调用；调用返回后清理未完成上传。全局最多一个文件线程，反复重连不会累积阻塞任务。完成上传恰逢超时时，需刷新目录确认是否已发布。[Tokio 官方限制](https://docs.rs/tokio/1.53.2/tokio/task/fn.spawn_blocking.html)

Windows 当前实现面向已登录、单显示器普通桌面，不支持登录前控制、锁屏和 UAC 安全桌面；普通权限不能保证控制管理员窗口，Windows 的 [SendInput/UIPI 限制](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)仍适用。熄屏控制未提供，能力明确返回 false。Linux 后端尚未实现。

## 构建与验证

```sh
cargo fmt --all -- --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
node --test web/*.test.mjs
```

Mac 打包：`python3 scripts/bundle.py`，输出位于 Cargo target 的 `release/bundle/macos/LanDeskServer.app`。开发包使用临时签名，更新后可能需要自行恢复权限；不重置其他应用权限。

Windows 服务端：`cargo build --locked --release`，输出 `target/release/LanDeskServer.exe`。[服务端工作流](.github/workflows/server.yml)在 Mac/Windows 分别执行测试、Clippy 和构建，artifact 为 `LanDeskServer-windows-x64` / `LanDeskServer-macos`。客户端使用 [Windows client 工作流](.github/workflows/windows-client.yml)，artifact 为 `LanDeskClient-windows-x64`。Windows 构建使用 MSVC、Windows SDK 和 CMake；最终用户不需要构建工具。

自动化测试覆盖密码校验与设备身份保持、实际 TCP 加密通道中的 HTTP/WebSocket 收发、拒绝命令行及任意转发、密码轮换断连、配置失败保留旧服务，以及原有网页和远控会话回归。测试通过不代表 Windows/Mac 桌面采集、权限、键鼠或防火墙已完成实机验收。系统服务、登录前控制、Linux 服务端、设备发现及中继尚未实现。
