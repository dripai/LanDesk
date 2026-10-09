# LanDesk

通过 SSH 加密隧道和浏览器远程操作电脑。Mac 与 Windows 服务端共用协议、网页和会话核心；Windows 连接管理客户端为 [LanDeskClient](client/README.md)。

## 使用

1. 被控 Mac 打开 `LanDesk.app`，自行授予屏幕录制、辅助功能权限并重开；在系统设置启用“远程登录”。Windows 服务端在已登录的普通桌面中启动 `landesk.exe`，需另行安装并启用 Windows OpenSSH Server。
2. Windows 打开 [LanDeskClient](client/README.md)，添加目标电脑的 IP/域名、SSH 端口、用户名和密码。首次连接记录主机密钥，密钥变化拒绝连接。
3. 每台电脑使用 `http://127.0.0.1:17890/s/<地址哈希>/` 独立标签。网页自动读取服务端系统与能力，不需要选择 Mac/Windows，也不使用六位连接码。更新后刷新网页。
4. 点击画面操作。连接 Mac 时 Ctrl 快捷键映射 Command；连接 Windows 时保留 Ctrl。支持方向键长按、中文与全角标点，松开或失焦释放按键。
5. 浮动工具条可拖动、收起，提供文字输入、剪贴板、文件面板、采集分辨率、全屏和断开。目录面板可调整宽度，目录与文件使用紧凑行高。
6. 文件范围限运行服务端的当前用户目录。拖拽普通文件或点上传，同名拒绝覆盖；单文件最多 512 MiB，64 KiB 分块。暂不支持目录上传、删除、下载和断点续传。
7. Ctrl+V 或粘贴按钮发送文字或单张图片。图片在目标系统剪贴板写入后，使用目标系统的粘贴快捷键；目标应用需要支持图片。取回剪贴板仍只支持文字，不自动同步。
8. 默认按屏幕实际像素采集、等比例显示；可选择采集宽度，不修改系统显示分辨率。当前仅支持一个显示器。
9. 关闭服务端窗口停止服务，最小化保留连接。同一台服务端仅允许一个控制会话；不同电脑可同时连接。最后一个网页关闭后，客户端保留 30 秒重连宽限，随后关闭对应 SSH 隧道。

当前服务端内部监听与客户端网页入口均使用本机 17890，因此同一台 Windows 暂不能同时运行服务端与 LanDeskClient；不同电脑之间连接不受影响。内部端口分离方案待确认。

## SSH 端口

服务端窗口可以输入 SSH 端口，点击“应用（管理员授权）”后实际修改系统配置并重启 SSH。仅在本机、无远控会话时允许操作；配置期间拒绝新远控。客户端按服务端显示的端口填写，首次连接前无法通过尚未建立的 SSH 自动发现端口。

- Mac：核对系统 launchd 使用 `SockServiceName=ssh` 后，修改 `/etc/services` 的 SSH 服务记录，使用 `systemsetup` 重启远程登录。管理员授权由系统弹窗处理；当前 macOS 的 `systemsetup` 另外要求完全磁盘访问权限，LanDesk 不自动授予。非标准 launchd 配置明确报错。
- Windows：修改系统 ProgramData 下的 `ssh/sshd_config` 并重启 `sshd` 服务，使用 UAC 授权。多 Port 或 Include 配置不自动修改。用户需要确保防火墙允许新 SSH 端口；不自动扩大防火墙规则。
- 变更前保存同目录 `.landesk-backup` 备份，原子写入配置，再检查新端口是否返回 SSH 握手。失败恢复原配置并重启；回滚失败保留备份并明确显示路径。已有备份时拒绝继续覆盖。
- 修改会影响这台电脑的系统 SSH 服务和其他 SSH 连接。公网路由器端口映射需另行配置；界面填写的是系统监听端口。

Mac 权限依据：本机 `man systemsetup`；管理员弹窗机制见 [Apple TN2065](https://developer.apple.com/library/archive/technotes/tn2065/_index.html)。监听方式根据本机 macOS 26.5.1 的 `/System/Library/LaunchDaemons/ssh.plist` 与 `launchd.plist(5)` 核对。Windows 配置位置和重启要求见 [Microsoft OpenSSH 文档](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh-server-configuration)。实际修改端口及重启后的持续可达性尚未实机验收。

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

Mac 打包：`python3 scripts/bundle.py`，输出位于 Cargo target 的 `release/bundle/macos/LanDesk.app`。开发包使用临时签名，更新后可能需要自行恢复权限；不重置其他应用权限。

Windows 服务端：`cargo build --locked --release`，输出 `target/release/landesk.exe`。[服务端工作流](.github/workflows/server.yml)在 Mac/Windows 分别执行测试、Clippy 和构建，Windows artifact 为 `LanDeskServer-windows-x64`。客户端继续使用 [Windows client 工作流](.github/workflows/windows-client.yml)与 `LanDeskClient-windows-x64` artifact。

本轮本机通过服务端 38 项、网页 14 项、客户端连接核心 12 项测试；Mac 与 Windows GNU 目标的全目标 Clippy 通过，Mac 应用打包与签名检查通过。Windows 桌面采集、键鼠、剪贴板、文件 ACL、UAC 和系统 SSH 端口变更仍需实机验收；交叉编译检查不代表这些功能已经验证。用户已确认重构前 Mac 远控及文件阻塞修复使用正常；重构版本尚未安装，需更新后复验。
