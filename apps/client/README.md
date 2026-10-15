# LanDeskClient

Windows 10 / 11 x64 与 macOS 客户端，使用 GPUI Kit 0.7.1 管理多台服务器，提供远程桌面和 SSH 终端两种模式。桌面模式使用 SSH `landesk` subsystem，需要本轮配套的 Server 构建。完整通信与平台设计见 [架构设计](../../docs/architecture.md)。

## 使用

1. Windows 从本仓库 Actions 的 **Windows client** 成功构建下载 `LanDeskClient-windows-x64`，解压运行 `LanDeskClient.exe`。Mac 打开 `LanDeskClient.app`。
2. 左侧点 `+` 添加服务器。远程桌面填写名称、地址、连接端口（默认 2222）和 LanDesk 连接密码，不填写用户名；Mac 需运行 LanDeskServer 并授予屏幕录制和辅助功能权限，不需要系统远程登录。服务端首次自动生成密码，可在服务端主界面查看和修改。SSH 终端继续填写系统 SSH 用户名、端口和密码/私钥，目标必须提供 SSH、PTY 和 Shell。保存后点击“连接”，SSH 就绪后自动打开网页。
3. 单击左侧条目查看连接信息；“编辑”后可保存或取消。编辑期间先完成或取消编辑，再切换条目。连接中禁止编辑或删除该条目；断开后可删除，删除同时移除它保存的密码。
4. SSH 密码下方可勾选“记住密码”，保存或连接时生效。密码保存到 Windows 凭据管理器或 macOS 钥匙串，不写入 JSON。未记住密码时，每次连接需要输入；取消勾选并连接或保存会删除此条目的已存密码。
   私钥认证填写本机私钥的绝对路径（Windows 例如 `C:\Users\Alice\.ssh\id_ed25519`，Mac 例如 `/Users/alice/.ssh/id_ed25519`），不展开 `~`。未加密私钥的口令留空，加密私钥每次连接输入口令；口令不保存，私钥内容不复制进配置或网页。切换为私钥并保存会删除原先记住的密码。
5. 每台服务器使用独立 SSH 会话、浏览器标签、连接状态和 30 秒无远控连接计时。切换列表不会断开连接；右侧“断开”只影响当前服务器。Windows 托盘菜单可“断开全部连接”或“退出”。
6. 首次 SSH 就绪后未打开网页会话，或最后一个桌面/终端 WebSocket 关闭后，连续 30 秒没有网页会话会自动关闭该 SSH。终端开着但没有输入不算无会话。宽限期内刷新或重连会取消倒计时。HTTP 保活和资源请求不会延长时间。网络异常识别依赖底层 TCP/SSH 心跳。
7. 超时后必须回客户端点击“连接”；统一网页入口保留，旧标签刷新会提示重新连接，不会自动创建 SSH。关闭整个客户端会结束全部 SSH 及网页入口。Windows 最小化或关窗口默认留在托盘，可在界面关闭此偏好。Mac 最小化到 Dock，关闭窗口保留连接，点击 Dock 图标恢复；通过应用菜单或 `⌘Q` 断开全部连接并退出，不创建菜单栏状态图标。

## Linux 网页终端

链路为：浏览器 xterm.js → 本机 WebSocket → LanDeskClient 的 SSH 会话 → Linux PTY/Shell。网页和终端依赖均内嵌在客户端，不依赖 CDN，不在 Linux 上监听 Web 端口。无需 X11、Wayland、桌面环境或 Linux 版 LanDesk。

支持交互式 Shell、中文/UTF-8、终端尺寸同步、ANSI 全屏程序。Ctrl+C 交给终端；选中文字后 Ctrl+Shift+C 复制，Ctrl+Shift+V 粘贴，浏览器可能要求剪贴板权限。输出使用渲染确认限制积压，大段输入分块发送；输入队列超过 512 KiB 时拒绝本次输入并提示。

每个标签创建独立 Shell。关闭或断开一个标签仅关闭对应 SSH 通道；重新连接会创建新的 Shell，不恢复旧进程。需要任务跨断线运行时由用户自行使用服务器上的 tmux 等工具。客户端断开服务器会结束该服务器的全部终端连接。终端命令与文件操作都按 SSH 账户权限执行，不提供额外权限隔离。

页面与桌面模式共用可拖动、可收起的悬浮工具条，不占整行顶栏。工具条提供选区复制、粘贴、文件面板、全屏、重连和断开。页面固定在可视高度内；历史输出只在终端内部滚动，目录和文件列表独立滚动。缩放窗口、全屏或调整右侧面板宽度会重新计算终端行列；正在看历史输出时不强制跳到底部。

右侧默认显示目录与文件面板，可浏览 SSH 账户有权访问的目录（从 `/` 开始），支持上级目录、刷新、每批最多 500 项的分页、选择或拖入普通文件上传。目录切换完成前禁用上传，避免写入之前的目录。文件操作使用同一 SSH 连接的独立 SFTP v3 通道，依赖服务器启用 SFTP；不可用时明确提示，终端仍可使用。没有下载、删除、整目录上传或跟随目录符号链接的入口。

单文件最多 512 MiB；上传先写同目录随机临时文件（0600 权限），完成后用 SFTP v3 RENAME 发布，同名目标拒绝覆盖。网页断开或取消时尝试清理未完成上传；SSH 断开、进程退出或网络故障可能留下 `.landesk-*.part`，清理失败会报错，不能保证断网后远程删除成功。文件操作不会调用 Shell 拼接命令。

使用密码登录还要求服务器允许该账户的 SSH 密码认证；设置账户密码并不等于 SSH 已允许密码登录。客户端不会替你修改服务器密码或 SSH 策略。

## 地址与隔离

统一入口使用系统分配的 `127.0.0.1:<动态端口>`，页面地址为 `/s/<完整 SHA-256>/`。客户端自动打开携带随机会话凭证的启动 URL，验证后跳转到无凭证参数的地址，使用 HttpOnly Cookie 访问资源及 WebSocket；启动 URL 不要分享。每次重连更换凭证，旧凭证失效，连接 ID 不提供授权。哈希输入为 UTF-8 JSON 二元组 `[规范化地址, SSH端口]`：IP 使用标准格式，主机名转小写并移除末尾点。用户名、名称和密码不参与计算。同一地址和 SSH 端口只能保存一条连接；主机名与其解析到的 IP 不合并，不进行隐式 DNS 去重。

哈希标识连接地址，不是设备硬件身份，也不是凭据。SSH 主机密钥负责识别主机：首次自动记录，后续变化拒绝，不显示指纹确认，也不覆盖旧记录。首次使用信任无法验证第一次连接的主机身份。

入口根据完整哈希路径分流，通过进程内独立通道连接对应 SSH；没有每服务器的公开本地端口。关闭路由后，已有引用无法转接到另一台机器。网页模块及 WebSocket 使用相对路径，标签标题显示服务器名称。入口验证 Host/Origin，终端与文件通道另有每次 SSH 连接随机生成的 hello token；这个 token 不放在 URL 中，SSH 重连后失效。每个 Server 限制一个远控控制会话；多个不同 Server 可同时连接。

各路径共享浏览器同源，哈希和会话凭据不构成同源网页之间的安全隔离；只连接自己信任的服务器。密码、私钥和私钥口令不会传给网页。

## 配置与凭据

配置位于 `%APPDATA%\LanDeskClient\settings.json`，包括服务器列表和托盘偏好。每台服务器的凭据目标为 `LanDeskClient/SSH/<哈希>`，凭据内校验地址、端口和协议身份；桌面模式的 SSH 用户名为内部固定字段，用户不需要填写。修改地址会更换标识和网页路径；保存时删除旧条目的凭据，目标密码须重新输入。SSH 终端更换系统用户名也需重新输入密码。

旧的单服务器配置及其匹配密码会一次性迁入列表，成功后写入新版格式。配置和凭据修改失败会回滚；回滚失败明确报错。损坏配置、重复连接和不匹配的旧凭据不会被静默忽略。

SSH 主机信任文件仍为同目录 `known_hosts`，同进程并发更新加锁并原子写入。删除连接保留主机信任记录，以便以后重新添加时仍能检查密钥变化。支持密码认证和 russh 可解析的私钥文件认证，不支持 SSH Agent、跳板机、keyboard-interactive 或主机证书。认证失败直接报错，不自动切换认证方式。

网页入口与服务端完全独立，可在同机共存。入口失败不禁用配置编辑，界面提供重试；其他目标认证失败不影响已有连接。不支持旧固定端口转发脚本。

连接任务由 `src/connections.rs` 的 `ConnectionManager` 管理，UI 负责表单和展示。连接状态明确区分连接中、已连接、断开中和已停止；断开后的迟到就绪事件不会重新打开网页，旧连接事件不能覆盖新连接。退出时先取消所有连接，等待任务停止后关闭网页入口。

## 构建与验证

macOS 本机构建需要 Rust 和 Xcode Command Line Tools，使用锁定的 GPUI Kit 0.7.1：

```sh
python3 scripts/bundle_macos_client.py --debug  # 本机开发验证
# 正式优化构建省略 --debug
open apps/client/target/bundle/macos/LanDeskClient.app
```

脚本生成当前 Mac 架构的 `.app`，采用本地临时签名，未做 Apple Developer ID 签名或公证。配置位于 `~/Library/Application Support/LanDeskClient/`，密码使用系统钥匙串。网页入口动态分配，仅绑定回环地址，可以和本机 LanDesk 服务端同时运行。

Windows 构建使用 Rust stable MSVC、Visual Studio C++ 工具链、Windows SDK 和 CMake：

```sh
cargo fmt --manifest-path apps/client/Cargo.toml --check
cargo test --manifest-path apps/client/Cargo.toml --locked --no-default-features --lib --tests
node --test apps/client/web/*.test.mjs web/*.test.mjs
cargo clippy --manifest-path apps/client/Cargo.toml --locked --all-targets -- -D warnings
cargo build --manifest-path apps/client/Cargo.toml --locked --release
```

终端静态依赖锁定 xterm.js 6.0.0 和 addon-fit 0.11.0，发布构建直接嵌入 `web/vendor/` 的 JS/CSS，保留 MIT 许可证。更新依赖时在 `apps/client/web` 执行 `npm ci`、`npm run vendor` 并提交依赖锁及生成文件；普通 Rust 构建不要求 npm 安装。

本机连接核心测试覆盖：地址规范化/去重，配置增删改及损坏/旧格式拒绝，真实双 SSH 服务的同端口路由隔离，WebSocket 64 KiB 往返，单台断开不影响另一台，HTTP 保活不续命，30 秒无会话关闭，重连宽限期及端口释放。终端集成测试使用真实本地 SSH/HTTP/WebSocket，覆盖密码和加密私钥、UTF-8/控制字节、尺寸变化、退出码、多标签隔离、PTY 拒绝和初始化中断、来源与令牌校验，以及 512 KiB 输出在慢速渲染下暂停/恢复不丢失。

开发时可在 Mac/Linux 运行相同核心的诊断入口：

```sh
cargo run --manifest-path apps/client/Cargo.toml --locked --no-default-features --example terminal_gateway -- example.com alice 22 /absolute/path/id_ed25519 /absolute/path/known_hosts
```

入口与正式客户端一致，监听动态 IPv4 回环地址，打印带会话凭证的启动 URL，勿分享或收集到日志。按回车退出，30 秒无网页会话也会退出。加密私钥的口令通过 `LANDESK_SSH_SECRET` 环境变量提供；私钥路径用 `-` 时改用该变量中的 SSH 密码。不要把真实凭据写入源码或命令历史。

官方依据：[GPUI Kit](https://gpui-kit.com/docs/installation/)、[russh 0.64.1 Channel](https://docs.rs/russh/0.64.1/russh/struct.Channel.html)、[Hyper HTTP Upgrade](https://docs.rs/hyper/1.12.0/hyper/upgrade/index.html)、[xterm.js API](https://xtermjs.org/docs/api/terminal/classes/terminal/)、[xterm.js 安全建议](https://xtermjs.org/docs/guides/security/)、[Windows 凭据](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credwritew)。实现核对了锁定的 GPUI Kit 0.7.1、Hyper 1.12.0、russh 0.64.1 源码。

文件功能依据：[russh-sftp 3.0.0](https://docs.rs/russh-sftp/3.0.0/russh_sftp/client/struct.RawSftpSession.html)、[SFTP v3 文件创建和重命名语义](https://datatracker.ietf.org/doc/html/draft-ietf-secsh-filexfer-02#section-6.5)。布局核对了内嵌 addon-fit 0.11.0 的 `proposeDimensions`：测量容器不能带额外内边距，否则终端行数可能超过实际可用高度。

Mac 凭据依据：[security-framework 3.7.0](https://docs.rs/security-framework/3.7.0/security_framework/passwords/index.html)。窗口关闭与恢复使用锁定 GPUI 的 `on_reopen` 及 AppKit `NSWindow`，不使用 Windows 托盘机制。

本轮验证及待验收项统一记录在 [架构设计](../../docs/architecture.md#版本依据与验证记录)。
