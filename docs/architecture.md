# LanDesk 架构设计

本文是 Client、Server、平台实现及虚拟显示器的统一设计与运维说明。网络入口只有远端 SSH 与客户端本机网页两类；服务端内部不监听 TCP。不提供旧端口转发兼容路径。

## 角色与连接拓扑

Client 负责连接配置、凭据、本机网页入口和出站 SSH；Server 负责入站 SSH 认证、屏幕、键鼠、剪贴板和文件访问。两个程序独立运行，不互相启动、停止或持有安装锁。

```text
Windows A 浏览器 → A 的 Client → SSH → Mac B 的 Server
Mac B 浏览器     → B 的 Client → SSH → Windows C 的 Server
```

Mac B 可以同时被控并控制其他设备。退出 B 的 Client 只结束第二条链路；关闭 B 的 Server 才会影响第一条链路。嵌套浏览器远控会叠加画面压缩与延迟，不是透明跳板。SSH 跳板、中继、NAT 穿透尚未实现，目标地址必须可达。

| 边界 | 实现 | 配置 |
|---|---|---|
| Client 到 Server | SSH，默认 `2222`，可配置；桌面模式使用 `landesk` subsystem | 目标地址、SSH 端口、连接密码 |
| 浏览器到 Client | `127.0.0.1:0` 由系统分配端口；一个 Client 实例一个入口 | 无需用户配置；实际地址只在运行时保存 |
| Mac Server 内部 | 有界内存双向流承载 HTTP/WebSocket | 无 TCP 端口 |
| Windows Server 内部 | 受保护的本机命名管道 | 禁止远程管道客户端；验证 SYSTEM 所有者及内部密钥 |

桌面服务只接受专用 subsystem，不提供系统账户登录、Shell、任意 TCP 转发或远端端口转发。SSH 终端模式是 Client 的独立功能，连接普通 SSH 服务的 PTY/Shell/SFTP，使用该 SSH 账户权限，不依赖 LanDeskServer。

## 连接与安全边界

1. Client 启动本机动态网页入口，记录实际监听地址。配置文件不保存这个端口。
2. 连接目标并验证 SSH 主机密钥；首次连接记录密钥，之后密钥变化明确拒绝。连接密码留在本机凭据存储和 SSH 认证流程中，不进入网页。
3. 桌面模式打开 SSH session channel，请求 `landesk` subsystem，并等待明确的成功回执；拒绝、超时或后端不可用时不发布就绪状态。
4. 就绪连接注册 `/s/<连接ID>/` 路由，同时生成独立随机 256 位网页访问凭证。连接 ID 来自目标地址和端口的 SHA-256，仅用于路由，不是凭据。
5. Client 打开的 URL 在查询参数中携带启动凭证；网关验证后返回 303，跳到不含凭证的页面地址，并设置按端口命名、按连接路径限定的 `HttpOnly; SameSite=Strict` 会话 Cookie。入口和页面均禁止缓存，设置 `Referrer-Policy: no-referrer`。启动 URL 是敏感信息，不应复制分享或写入日志；本机浏览器历史、同账户恶意进程不属于该机制能完全隔离的范围。
6. 每次资源请求与 WebSocket 升级都校验实际 Host 和凭证；提供 Origin 时必须精确匹配实际入口，WebSocket 必须携带 Origin。其他本机端口、外部站点、缺失或失效凭证均不能访问会话。
7. 网关移除浏览器 Cookie，将已验证的请求转换为内部 HTTP 身份 `landesk.internal`。Server 的 SSH 边界覆盖外来内部认证头，注入本机私有密钥；资源与 WebSocket 统一验证该密钥。Windows 管道访问限制保持独立有效。

内部 HTTP 身份是协议常量，不解析 DNS、不监听端口，不代表远端设备身份。SSH 主机密钥与连接密码才承担远端认证。终端页面另有每会话 hello token；来源、网页凭据及终端初始化认证分别验证。

同一 Client 的目标页面共享浏览器同源，路径和 Cookie 不构成目标网页之间的隔离；应只连接可信服务器。首次 SSH 主机密钥采用首次使用信任，不能验证第一次连接时的主机身份。

断开立即取消通道与转发任务，路由注销后凭证失效。同一目标重连生成新凭证，旧 Cookie、旧启动 URL 不可复用。客户端重新连接后自动打开新 URL；不能靠刷新旧地址创建 SSH。

## 生命周期与故障范围

`ConnectionManager` 拥有网页入口、SSH 任务、取消信号和连接状态。每次连接使用递增编号，旧任务迟到事件不能覆盖新连接；网页入口重启也使用独立编号，旧入口退出事件不能破坏新入口。

连接状态为连接中、已连接、断开中、已停止；同一目标的任务未结束前不能再次连接。多个目标共享网页监听器，各自拥有 SSH、路由、凭证、任务和空闲计时。断开一个目标不影响其他目标。

- 网页入口启动/运行失败：阻止新连接，停止依赖它的会话，显示错误和重试入口；配置添加、编辑和保存仍可用。旧连接清理完后可重启入口。
- 配置加载失败：明确报错，阻止覆盖原配置；不能以空配置静默替代并写回。
- 凭据读取/写入失败：报告对应操作错误，不将其当成密码不存在。
- 单目标认证、权限、采集或通道失败：限制在该目标，不关闭其他连接或同机 Server。
- 退出 Client：取消连接、等待任务结束，再关闭本机入口。关闭窗口与退出应用是不同操作。

Windows Client 可选择最小化/关闭窗口留在托盘。Mac Client 不创建菜单栏状态图标：最小化到 Dock，关闭窗口保留连接，Dock 重新打开恢复窗口，应用菜单或 `⌘Q` 断开连接后退出。

SSH 就绪后，连续 30 秒没有桌面/终端 WebSocket 会话则关闭该 SSH；HTTP 保活与资源访问不续期。最后一个网页会话关闭后开始计时，宽限期内刷新可继续使用同一连接。终端打开但不输入不算空闲。网络故障由 TCP/SSH 心跳识别，不按桌面输入或画面频率判断。

## 模块边界

| 模块 | 职责 |
|---|---|
| `client/src/ui.rs` | 表单、用户操作、错误展示；不自行管理 SSH 任务 |
| `client/src/connections.rs` | 入口、连接状态、任务与取消、运行时浏览器 URL |
| `client/src/gateway.rs` | 动态监听、浏览器来源与会话认证、路由、HTTP/WebSocket 转发 |
| `client/src/tunnel.rs` | SSH 身份验证、subsystem 握手、会话任务及空闲关闭 |
| `client/src/terminal*.rs` | PTY/Shell、终端网页、SFTP；与桌面平台无关 |
| `client/src/credentials/` | Windows Credential Manager / macOS 钥匙串；公共事务负责配置与凭据回滚 |
| `src/transport.rs` | 入站 SSH、连接密码验证、专用 subsystem、内部认证注入 |
| `src/server.rs` / `src/server_auth.rs` | HTTP/WebSocket、控制会话所有权、内部认证、取消与协议处理 |
| `src/protocol.rs` | 浏览器协议与类型化平台命令转换 |
| `src/capture.rs` / `src/file_worker.rs` | 独立采集及文件工作线程、背压、取消、迟到结果隔离 |
| `src/platform/` | `DesktopControl`、`CaptureBackend`、`FrameSource`、`VirtualDisplayProvider`、`FileBackend` / `FileSession` |
| `src/main.rs` | Mac 主线程与服务组装，内存流连接桌面后端 |
| `src/platform/windows/` | Windows 服务、桌面工作进程、命名管道、采集、输入、驱动与安装 |

平台层接收 `InputEvent`、`FileCommand` 等类型化命令，返回 `CaptureInfo`、`DisplayInfo`、`FileResponse`；不接收整个浏览器消息，也不生成任意 JSON。平台接口不存在默认放行的认证实现。

采集后端和原生句柄在专用线程创建。画面发送、输入和文件处理分别推进；输入队列有界，连续鼠标移动合并，键盘/按钮保持顺序。前端解码期间保存最新待显示帧。文件调用阻塞时不冻结画面或断开流程；超时不能伪称中止已进入操作系统的调用，取消后丢弃迟到结果并限制工作线程数量。

## 平台能力与约束

### macOS

使用 ScreenCaptureKit 采集、Enigo/CoreGraphics 输入、AppKit 窗口与剪贴板。输入及清理在 AppKit 主线程的 common run-loop modes 中执行，窗口拖动或模态循环不应吞掉按键释放。需要屏幕录制与辅助功能授权；启动不自动申请。

连接密码首次随机生成 8 位数字/大小写字母，界面可查看、复制及修改。验证使用 Argon2id，显示密码使用 AES-256-GCM 密文；加密密钥保存在钥匙串。修改失败不显示未保存密码。SSH 身份与内部认证密钥不提供给浏览器。

当前 `.app` 打包采用本机 ad-hoc 临时签名，没有 Developer ID 签名或公证；更新后系统可能重新要求隐私授权。正式分发签名尚未接入。

虚拟屏为实验能力，使用未公开的 `CGVirtualDisplay`，不是 Apple 承诺稳定的公开 API；实际采集仍使用 ScreenCaptureKit。运行时检查类和选择器，创建失败明确报错。

### Windows

提供 `LanDeskTransport`（LocalService）与 `LanDeskServer`（LocalSystem）两项服务，桌面服务依赖通信服务。桌面工作进程位于活动控制台会话，通信通过 SYSTEM 所有的受保护命名管道，不通过本机 TCP。托盘/设置界面与后台服务分离，退出管理界面不停止远控。

物理控制台可能与 RDP 会话不同；不自动登录、不修改系统账户或 RDP。支持 GDI/JPEG 采集、SendInput、剪贴板与文件访问。普通输入失败时保留画面和选屏，显示警告；实际输入成功后撤销警告，不重放失败点击或文字。Ctrl+Alt+Delete 的 SendSAS 调用无效果回执，日志成功不代表登录界面必然响应。

连接配置位于 `%ProgramData%\LanDeskServer\transport\config.json`，包含 Argon2id、AES-GCM 密文、SSH 身份与内部密钥，限制普通用户读取。密码加密密钥使用机器范围 DPAPI。专用本机密码管道允许交互用户读取显示密码的加密记录；界面校验 LocalService 管道所有者，不接收 SSH 私钥或内部密钥。机器范围 DPAPI 不能隔离已经取得密文的本机用户。

### 显示选择与虚拟设备生命周期

`hello.display` 与 `set_display_source.display` 使用统一结构：

```json
{"kind":"virtual","width":1920,"height":1080}
```

已有屏幕使用 `{"kind":"existing","id":123}`，`id:null` 为主屏。省略 display 时使用本次服务进程最后成功选择，首次为主屏；采集失败不自动切屏或创建虚拟屏。

虚拟设备由 Server 持有，网页断开/刷新后仍存在；相同尺寸复用。新设备创建及采集初始化成功后才提交选择；失败保留原选择，旧采集释放后再处理旧设备。切回实体屏仍保留最后虚拟屏到服务退出，配置不落盘。尺寸受 1600 万像素预算限制。创建显示器可能触发系统调整布局，但 LanDesk 不主动搬窗口、镜像或改变主屏。

Windows 驱动使用上游 MttVDD 25.7.23 原始 DLL/INF/CAT，安装前核验 SHA-256 和 WinVerifyTrust，再由 DiInstallDevice 验证安装；不导入证书或开启测试签名。新配置为 1920×1080、60 Hz、SDR；自建设备 `ROOT\LANDESKDISPLAY\0000`。已有正常 MttVDD 复用并保留配置及归属；设备异常或配置无效时明确失败。没有配置时使用上游默认 `C:\VirtualDisplayDriver`。

无头冷启动、合盖、FileVault、UAC、锁屏和不同系统版本的能力必须逐项实机验证，不能由编译成功或显示器枚举成功推出。Linux 图形桌面服务、音频与透明跳板尚未实现。

## Windows 安装、维护与诊断

双击 `LanDeskServer.exe`，管理员授权后完成安装；驱动与模板内嵌，无需系统 OpenSSH、PowerShell 或维护脚本。防火墙规则仅放行指定服务端程序的 SSH 入站端口。驱动提示重启时需重启后验收。

安装器只有全新安装路径：校验归属，清理被替换的服务/程序/连接配置，再创建服务与入口。更新会生成新密码和 SSH 身份；正常运行及重启保留它们。无关文件、共享驱动和已有显示配置必须保留，同名但归属不明的程序拒绝覆盖。安装失败逆序回滚本次创建对象并记录回滚失败；不承诺恢复已经清理的旧服务。

- 开机启动：默认启用，后台不依赖用户登录；取消只改为手动启动，不立即断开。
- 启动时最小化：控制登录后管理界面，开始菜单入口可打开设置。
- 卸载：停止服务和界面，删除程序、启动入口、防火墙与应用登记；保留配置、日志及无关文件。可选移除 LanDesk 自建虚拟设备，不移除第三方设备和共享 Driver Store 包。
- 修复日志并启动：验证归属和配置，修复日志目录、文件及 LocalService 权限，按依赖顺序启动服务。不重建密码、不重装驱动；配置缺失明确报错。
- 导出日志：生成 `%ProgramData%\LanDeskServer\diagnostics\LanDeskServer-diagnostics-*.zip`。不持有安装锁；日志缺失或读取失败记入 `status.json`，仍可导出诊断状态。

只读检查：

```text
LanDeskServer.exe --version
LanDeskServer.exe --status
```

后台日志：`install.log`、`uninstall.log`、`server.log`、`transport.log`、`worker.log`；带 UTC 时间、PID、版本，约 2 MB 轮换。界面日志在 `%LOCALAPPDATA%\LanDeskServer\logs\ui.log`，需单独提供。诊断只收集明确列出的日志、服务、设备与会话元数据，不收集密码、配置、私钥、画面、输入、剪贴板或窗口标题。首帧日志只记录是否纯色，不记录像素。

服务先注册 SCM 再初始化日志；初始化失败通过 SCM 报错。维护操作的密码输入、确认与完成弹窗不占维护锁；等待辅助进程时继续处理窗口消息。

## 构建与验证

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --manifest-path client/Cargo.toml --check
cargo test --manifest-path client/Cargo.toml --locked --no-default-features --lib --tests
cargo clippy --manifest-path client/Cargo.toml --locked --all-targets -- -D warnings
node --test web/*.test.mjs client/web/*.test.mjs
python3 scripts/bundle.py
python3 scripts/bundle_macos_client.py --debug
```

跨程序集成测试使用真实 Client、SSH Server 和内存 HTTP/WebSocket 后端，桌面平台使用测试替身，不采集屏幕或注入输入：

```sh
cargo build --manifest-path client/Cargo.toml --locked --no-default-features --example desktop_probe
LANDESK_CLIENT_PROBE=/absolute/cargo-target/debug/examples/desktop_probe \
  cargo test --locked --lib actual_client_crosses_subsystem_memory_backend_and_browser_authentication -- --ignored
```

将环境变量替换为实际 Cargo target 下的可执行文件绝对路径；Windows 工作流已包含这项测试。

Windows 在 MSVC、Windows SDK、CMake 环境构建：

```text
cargo build --locked --release --bin LanDeskServer --bin LanDeskDisplay
python scripts/bundle_windows_display.py --server-exe target/release/LanDeskServer.exe --output target/LanDeskServer-windows-x64
cargo build --manifest-path client/Cargo.toml --locked --release --bin LanDeskClient
```

普通测试不安装驱动、不创建虚拟设备。Windows 管理员原生维护测试由 CI 显式运行：`cargo test --locked --lib native_setup_ -- --ignored --test-threads=1`。Mac 真实虚拟显示测试需要显式设置 `LANDESK_TEST_VIRTUAL_DISPLAY=1 LANDESK_TEST_VIRTUAL_CAPTURE=1`，且测试进程已有屏幕录制权限。

验收必须覆盖：同机 Client/Server 共存、两条独立链路、旧端口已占用时 Client 可启动、跨目标隔离、网页凭证缺失/错误/失效、错误 Origin/Host、未知 subsystem/Shell/端口转发拒绝、入口失败后配置可编辑及重试、退出释放所有任务。UI 构建与协议测试不等于真实跨机桌面/键鼠验收。

## 版本依据与验证记录

锁定依赖：russh 0.64.1、Tokio 1.53.2、Hyper 1.12.0、Axum 0.8.9、GPUI Kit 0.7.1、ScreenCaptureKit Rust 11.0.0、Enigo 0.6.1、windows-sys 0.61.2、windows 0.61.3、security-framework 3.7.0。接口先核对本机锁定源码。

- 2026-10-15，0.3.0：Mac 服务端 54 项库测试、3 项平台契约测试及原生运行循环测试通过；客户端 26 项核心测试与 7 项 SSH/终端集成测试通过；29 项网页测试通过。跨程序集成测试验证了 subsystem、内存后端、网页凭证交换、静态资源、WebSocket 升级与断开路由清理；没有验证真实画面或输入。
- 两端格式检查和 Clippy 通过。Windows x64 gnullvm 客户端全目标编译检查、服务端库及两个程序 Clippy 通过；这是 macOS 交叉检查，不等于 Windows MSVC CI 或实机通过。
- Mac arm64 Server 优化构建及 Client 调试构建完成，均采用 ad-hoc 签名。Client 已更新到 `/Applications/LanDeskClient.app` 并验证界面可操作；旧 Server 仍占用 `127.0.0.1:17890` 时，新 Client 使用系统分配的端口正常启动。当前运行的旧 Server 未被替换，新 Server 构建产物尚未实机启动验收。
- 既有实测：Mac 真实画面、键鼠、中文、文件操作、复制/粘贴按钮；Mac 虚拟屏创建、尺寸、复用、释放和真实 JPEG 首帧。用户曾验证 Win10 mini 无实体屏冷启动后通过网页登录、操作主屏与登录后的虚拟副屏。
- 尚未验证：本轮新协议在真实 Windows 设备的部署、三机嵌套完整操作、Mac Intel/旧系统兼容性、Win11/UAC/锁屏全面场景。历史实测不代表本轮版本已通过相同验收。

官方及实现依据：

- [SSH subsystem / russh Handler](https://docs.rs/russh/0.64.1/russh/server/trait.Handler.html)、[Tokio 动态监听](https://docs.rs/tokio/1.53.2/tokio/net/struct.TcpListener.html)、[Tokio 内存流](https://docs.rs/tokio/1.53.2/tokio/io/fn.duplex.html)、[Hyper HTTP Upgrade](https://docs.rs/hyper/1.12.0/hyper/upgrade/index.html)。
- [ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit)、[AppKit Run Loops](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/Multithreading/RunLoopManagement/RunLoopManagement.html)、[钥匙串接口](https://docs.rs/security-framework/3.7.0/security_framework/passwords/index.html)、[Developer ID](https://developer.apple.com/developer-id/)。
- [Mac 虚拟显示器参考实现（Chromium）](https://chromium.googlesource.com/chromium/src/+/HEAD/ui/display/mac/test/virtual_display_util_mac.mm)：不是公开 Apple API 承诺。
- [Windows IDD](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/indirect-display-driver-model-overview)、[MttVDD 25.7.23](https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/tag/25.7.23)、[DiInstallDevice](https://learn.microsoft.com/en-us/windows/win32/api/newdev/nf-newdev-diinstalldevice)、[WinVerifyTrust](https://learn.microsoft.com/en-us/windows/win32/api/wintrust/nf-wintrust-winverifytrust)。
- [Windows 服务入口](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-startservicectrldispatcherw)、[DeleteService](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-deleteservice)、[CreateProcessAsUserW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw)、[SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)、[SendSAS](https://learn.microsoft.com/en-us/windows/win32/api/sas/nf-sas-sendsas)、[DPAPI](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata)。
