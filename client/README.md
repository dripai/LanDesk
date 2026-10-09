# LanDeskClient

Windows 10 / 11 x64 客户端，使用 GPUI Kit 0.7.1（GPUI / gpui-component）管理多个 Mac / Windows 服务端的 SSH 连接。

## 使用

1. 从本仓库 Actions 的 **Windows client** 成功构建下载 `LanDeskClient-windows-x64`，解压运行 `LanDeskClient.exe`。目标电脑需要运行相同版本的 LanDesk 服务端。Mac 更新后可能需要自行恢复屏幕录制和辅助功能权限；Windows 服务端需在已登录桌面运行并启用 OpenSSH Server。
2. 左侧点 `+` 添加服务器，填写名称、IP/主机名、服务端显示的 SSH 端口和用户名，保存后点击“连接”。Mac 应先开启“远程登录”；Windows 应先启用 OpenSSH Server。
3. 单击左侧条目查看连接信息；“编辑”后可保存或取消。编辑期间先完成或取消编辑，再切换条目。连接中禁止编辑或删除该条目；断开后可删除，删除同时移除它保存的密码。
4. SSH 密码下方可勾选“记住密码”，保存或连接时生效。密码保存到 Windows 凭据管理器，不写入 JSON。未记住密码时，每次连接需要输入；取消勾选并连接或保存会删除此条目的已存密码。
5. 每台服务器使用独立 SSH 会话、浏览器标签、连接状态和 30 秒无远控连接计时。切换列表不会断开连接；右侧“断开”只影响当前服务器。托盘菜单可“断开全部连接”或“退出”。
6. 首次 SSH 就绪后未打开远控，或最后一个远控 WebSocket 关闭后，连续 30 秒没有远控连接会自动关闭该 SSH 隧道。宽限期内刷新或重连会取消倒计时。HTTP 保活和资源请求不会延长时间。网络异常需先由 WebSocket/SSH 心跳识别。
7. 超时后必须回客户端点击“连接”；统一网页入口保留，旧标签刷新会提示重新连接，不会自动创建 SSH。关闭整个客户端会结束全部 SSH 及网页入口。最小化或关窗口默认留在托盘，可在界面关闭此偏好。

## 地址与隔离

统一入口只监听 `127.0.0.1:17890`，页面地址为 `/s/<完整 SHA-256>/`。哈希输入为 UTF-8 JSON 二元组 `[规范化地址, SSH端口]`：IP 使用标准格式，主机名转小写并移除末尾点。用户名、名称和密码不参与计算。同一地址和 SSH 端口只能保存一条连接；主机名与其解析到的 IP 不合并，不进行隐式 DNS 去重。

哈希标识连接地址，不是设备硬件身份，也不是凭据。SSH 主机密钥负责识别主机：首次自动记录，后续变化拒绝，不显示指纹确认，也不覆盖旧记录。首次使用信任无法验证第一次连接的主机身份。

入口根据完整哈希路径分流，通过进程内独立通道转发到对应 SSH；没有每服务器的公开本地端口。关闭路由后，已有引用无法转接到另一台机器。网页模块及 WebSocket 使用相对路径，标签标题显示服务器名称。入口验证 Host/Origin，服务端仍独立检查来源。每台服务端仍限制一个远控控制会话；不同 Mac / Windows 可同时连接。

## 配置与凭据

配置位于 `%APPDATA%\LanDeskClient\settings.json`，包括服务器列表和托盘偏好。每台服务器的凭据目标为 `LanDeskClient/SSH/<哈希>`，凭据内仍校验用户名及地址。修改地址会更换标识和网页路径；保存时删除旧条目的凭据，目标密码须重新输入。同一地址更换用户名也需重新输入密码。

旧的单服务器配置及其匹配密码会一次性迁入列表，成功后写入新版格式。配置和凭据修改失败会回滚；回滚失败明确报错。损坏配置、重复连接和不匹配的旧凭据不会被静默忽略。

SSH 主机信任文件仍为同目录 `known_hosts`，同进程并发更新加锁并原子写入。删除连接保留主机信任记录，以便以后重新添加时仍能检查密钥变化。当前只支持 SSH 密码认证，不支持 SSH 密钥、跳板机或主机证书。

固定网页端口冲突会明确报错；不能与旧的 `scripts/LanDesk.cmd` 或本机 LanDesk 服务端同时占用 17890。该脚本仍是独立单服务器转发，不具备客户端的自动关闭逻辑。服务端内部端口分离方案待确认。

## 构建与验证

使用 Rust stable MSVC、Visual Studio C++ 工具链、Windows SDK 和 CMake：

```sh
cargo fmt --manifest-path client/Cargo.toml --check
cargo test --manifest-path client/Cargo.toml --locked --no-default-features --lib
cargo clippy --manifest-path client/Cargo.toml --locked --all-targets -- -D warnings
cargo build --manifest-path client/Cargo.toml --locked --release
```

本机连接核心测试覆盖：地址规范化/去重，配置迁移及增删改，真实双 SSH 服务的同端口路由隔离，WebSocket 64 KiB 往返，单台断开不影响另一台，HTTP 保活不续命，30 秒无远控关闭，重连宽限期及端口释放。Windows 凭据隔离和回滚测试由 Windows 工作流执行。Windows 实际 UI、托盘和到 Mac 的完整远控仍需实机验收；编译成功不等于实机验证。

官方依据：[GPUI Kit](https://gpui-kit.com/docs/installation/)、[russh](https://docs.rs/russh/0.64.1/russh/)、[Hyper HTTP Upgrade](https://docs.rs/hyper/latest/hyper/upgrade/index.html)、[Windows 凭据](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credwritew)。实现核对了锁定的 GPUI Kit 0.7.1、Hyper 1.12.0、russh 0.64.1 源码。
