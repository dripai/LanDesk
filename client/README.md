# LanDeskClient

Windows 10 / 11 x64 客户端，使用 GPUI Kit 0.7.1（GPUI / gpui-component）管理多台服务器。提供 **Mac 桌面** 和 **SSH 终端** 两种连接模式。

## 使用

1. 从本仓库 Actions 的 **Windows client** 成功构建下载 `LanDeskClient-windows-x64`，解压运行 `LanDeskClient.exe`。SSH 终端功能需使用包含本次修改的新构建；历史构建不含此功能。
2. 左侧点 `+` 添加服务器，选择连接模式和认证方式，填写名称、IP/主机名、SSH 端口和用户名，保存后点击“连接”，SSH 就绪后自动打开网页。Mac 桌面模式要求 Mac 开启“远程登录”、运行新版 LanDesk 并授予屏幕录制和辅助功能权限。SSH 终端模式只要求目标服务器提供 SSH、PTY 和 Shell。
3. 单击左侧条目查看连接信息；“编辑”后可保存或取消。编辑期间先完成或取消编辑，再切换条目。连接中禁止编辑或删除该条目；断开后可删除，删除同时移除它保存的密码。
4. SSH 密码下方可勾选“记住密码”，保存或连接时生效。密码保存到 Windows 凭据管理器，不写入 JSON。未记住密码时，每次连接需要输入；取消勾选并连接或保存会删除此条目的已存密码。
   私钥认证填写 Windows 本机私钥的绝对路径（例如 `C:\Users\Alice\.ssh\id_ed25519`），不展开 `~`。未加密私钥的口令留空，加密私钥每次连接输入口令；口令不保存，私钥内容不复制进配置或网页。切换为私钥并保存会删除原先记住的密码。
5. 每台服务器使用独立 SSH 会话、浏览器标签、连接状态和 30 秒无远控连接计时。切换列表不会断开连接；右侧“断开”只影响当前服务器。托盘菜单可“断开全部连接”或“退出”。
6. 首次 SSH 就绪后未打开网页会话，或最后一个桌面/终端 WebSocket 关闭后，连续 30 秒没有网页会话会自动关闭该 SSH。终端开着但没有输入不算无会话。宽限期内刷新或重连会取消倒计时。HTTP 保活和资源请求不会延长时间。网络异常识别依赖底层 TCP/SSH 心跳。
7. 超时后必须回客户端点击“连接”；统一网页入口保留，旧标签刷新会提示重新连接，不会自动创建 SSH。关闭整个客户端会结束全部 SSH 及网页入口。最小化或关窗口默认留在托盘，可在界面关闭此偏好。

## Linux 网页终端

链路为：浏览器 xterm.js → 本机 WebSocket → LanDeskClient 的 SSH 会话 → Linux PTY/Shell。网页和终端依赖均内嵌在客户端，不依赖 CDN，不在 Linux 上监听 Web 端口。无需 X11、Wayland、桌面环境或 Linux 版 LanDesk。

支持交互式 Shell、中文/UTF-8、终端尺寸同步、ANSI 全屏程序。Ctrl+C 交给终端；选中文字后 Ctrl+Shift+C 复制，Ctrl+Shift+V 粘贴，浏览器可能要求剪贴板权限。输出使用渲染确认限制积压，大段输入分块发送；输入队列超过 512 KiB 时拒绝本次输入并提示。

每个标签创建独立 Shell。关闭或断开一个标签仅关闭对应 SSH 通道；重新连接会创建新的 Shell，不恢复旧进程。需要任务跨断线运行时由用户自行使用服务器上的 tmux 等工具。客户端断开服务器会结束该服务器的全部终端连接。没有新增 SFTP 文件面板、批量脚本、进程管理面板或额外权限隔离，命令按 SSH 账户权限执行。

使用密码登录还要求服务器允许该账户的 SSH 密码认证；设置账户密码并不等于 SSH 已允许密码登录。客户端不会替你修改服务器密码或 SSH 策略。

## 地址与隔离

统一入口只监听 `127.0.0.1:17890`，页面地址为 `/s/<完整 SHA-256>/`。哈希输入为 UTF-8 JSON 二元组 `[规范化地址, SSH端口]`：IP 使用标准格式，主机名转小写并移除末尾点。用户名、名称和密码不参与计算。同一地址和 SSH 端口只能保存一条连接；主机名与其解析到的 IP 不合并，不进行隐式 DNS 去重。

哈希标识连接地址，不是设备硬件身份，也不是凭据。SSH 主机密钥负责识别主机：首次自动记录，后续变化拒绝，不显示指纹确认，也不覆盖旧记录。首次使用信任无法验证第一次连接的主机身份。

入口根据完整哈希路径分流，通过进程内独立通道连接对应 SSH；没有每服务器的公开本地端口。关闭路由后，已有引用无法转接到另一台机器。网页模块及 WebSocket 使用相对路径，标签标题显示服务器名称。入口验证 Host/Origin，终端还验证每次 SSH 连接随机生成的会话凭据；凭据不放在 URL 中，SSH 重连后旧凭据失效。每个 Mac 仍限制一个远控控制会话；多个不同 Mac 可同时连接。

各路径共享浏览器同源，哈希和会话凭据不构成同源网页之间的安全隔离；只连接自己信任的服务器。密码、私钥和私钥口令不会传给网页。

## 配置与凭据

配置位于 `%APPDATA%\LanDeskClient\settings.json`，包括服务器列表和托盘偏好。每台服务器的凭据目标为 `LanDeskClient/SSH/<哈希>`，凭据内仍校验用户名及地址。修改地址会更换标识和网页路径；保存时删除旧条目的凭据，目标密码须重新输入。同一地址更换用户名也需重新输入密码。

旧的单服务器配置及其匹配密码会一次性迁入列表，成功后写入新版格式。配置和凭据修改失败会回滚；回滚失败明确报错。损坏配置、重复连接和不匹配的旧凭据不会被静默忽略。

SSH 主机信任文件仍为同目录 `known_hosts`，同进程并发更新加锁并原子写入。删除连接保留主机信任记录，以便以后重新添加时仍能检查密钥变化。支持密码认证和 russh 可解析的私钥文件认证，不支持 SSH Agent、跳板机、keyboard-interactive 或主机证书。认证失败直接报错，不自动切换认证方式。

旧配置未指定连接模式和认证方式时继续使用 Mac 桌面和密码认证。

固定网页端口冲突会明确报错；不能与旧的 `scripts/LanDesk.cmd` 同时占用端口。该脚本仍是独立单服务器转发，不具备客户端的自动关闭逻辑。

## 构建与验证

使用 Rust stable MSVC、Visual Studio C++ 工具链、Windows SDK 和 CMake：

```sh
cargo fmt --manifest-path client/Cargo.toml --check
cargo test --manifest-path client/Cargo.toml --locked --no-default-features --lib --tests
node --test client/web/protocol.test.mjs
cargo clippy --manifest-path client/Cargo.toml --locked --all-targets -- -D warnings
cargo build --manifest-path client/Cargo.toml --locked --release
```

终端静态依赖锁定 xterm.js 6.0.0 和 addon-fit 0.11.0，发布构建直接嵌入 `web/vendor/` 的 JS/CSS，保留 MIT 许可证。更新依赖时在 `client/web` 执行 `npm ci`、`npm run vendor` 并提交依赖锁及生成文件；普通 Rust 构建不要求 npm 安装。

本机连接核心测试覆盖：地址规范化/去重，配置迁移及增删改，真实双 SSH 服务的同端口路由隔离，WebSocket 64 KiB 往返，单台断开不影响另一台，HTTP 保活不续命，30 秒无会话关闭，重连宽限期及端口释放。终端集成测试使用真实本地 SSH/HTTP/WebSocket，覆盖密码和加密私钥、UTF-8/控制字节、尺寸变化、退出码、多标签隔离、PTY 拒绝和初始化中断、来源与令牌校验，以及 512 KiB 输出在慢速渲染下暂停/恢复不丢失。

2026-10-14：本机终端核心与网页协议测试通过；Mac 诊断入口连接真实 Ubuntu 24.04.5 服务器，已验证私钥登录、中文、Shell 命令、top 显示、多标签独立关闭、退出码及网页重连。Windows 新界面、快捷键/剪贴板和 EXE 打包仍需 Windows 构建及实机验收；本机缺少 MinGW 编译器和资源编译器，交叉检查未完成，Mac 测试不代替 Windows 验证。

开发时可在 Mac/Linux 运行相同核心的诊断入口：

```sh
cargo run --manifest-path client/Cargo.toml --locked --no-default-features --example terminal_gateway -- example.com alice 22 /absolute/path/id_ed25519 /absolute/path/known_hosts
```

入口监听 `[::1]:17890` 并打印 `localhost` 网页地址，要求本机启用 IPv6 回环；与 Windows 正式客户端的 IPv4 入口不同。按回车退出，30 秒无网页会话也会退出。加密私钥的口令通过 `LANDESK_SSH_SECRET` 环境变量提供；私钥路径用 `-` 时改用该变量中的 SSH 密码。不要把真实凭据写入源码或命令历史。

官方依据：[GPUI Kit](https://gpui-kit.com/docs/installation/)、[russh 0.64.1 Channel](https://docs.rs/russh/0.64.1/russh/struct.Channel.html)、[Hyper HTTP Upgrade](https://docs.rs/hyper/1.12.0/hyper/upgrade/index.html)、[xterm.js API](https://xtermjs.org/docs/api/terminal/classes/terminal/)、[xterm.js 安全建议](https://xtermjs.org/docs/guides/security/)、[Windows 凭据](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credwritew)。实现核对了锁定的 GPUI Kit 0.7.1、Hyper 1.12.0、russh 0.64.1 源码。
