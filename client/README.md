# LanDeskClient

Windows 10 / 11 x64 控制端，使用 GPUI Kit 0.7.1（GPUI / gpui-component）管理多个 Mac / Windows 的 **LanDeskServer**。无需安装 SSH 软件。

## 使用

1. 下载本仓库 Actions 的 `LanDeskClient-windows-x64`，解压运行 `LanDeskClient.exe`。目标电脑运行新版 `LanDeskServer`，设置访问密码并保存启动。Mac 需要屏幕录制和辅助功能权限；Windows 需要允许应用接受网络连接。
2. 左侧点 `+` 添加服务器，填写名称、IP/主机名、服务端连接端口（默认 **17891**），保存后输入 **LanDesk 访问密码**并点击“连接”，成功后自动打开网页。不使用系统账号密码。
3. 单击条目查看连接信息；“编辑”后可保存或取消。连接中禁止编辑和删除。删除条目同时移除它保存的访问密码。
4. “记住密码”在保存或连接时生效，存入 Windows 凭据管理器，不写入 JSON。取消勾选并连接或保存，会删除此条目的已存密码。
5. 每台服务器独立连接、状态和浏览器标签。切换列表不会断开连接；“断开”只影响当前服务器。托盘可断开全部或退出；最小化或关闭窗口默认留在托盘。
6. 关闭标签或长时间不操作都不会自动关闭连接。网页中的“重新连接”可恢复画面；网络中断后，网页请求会重新建立加密连接，不需要回客户端操作，也不会额外打开标签。
7. 在客户端主动断开或退出，才会撤销这条连接的网页重连能力；之后需先在客户端点击“连接”。没有“打开桌面”按钮或自动打开浏览器开关。
8. 重连所需的密码仅在当前连接生命周期内保留于内存，断开或退出后清除；是否写入 Windows 凭据管理器仍由“记住密码”决定。保留握手失败、请求卡住和网络失联的检测，不限制正常连接的持续时间。


## 地址、配置与凭据

网页入口只监听 `127.0.0.1:17890`，服务端默认监听 17891，两端可在同机运行。页面地址 `/s/<完整 SHA-256>/`，输入是 UTF-8 JSON `[规范化地址, 连接端口]`。IP 使用标准格式，主机名转小写并移除末尾点；名称和密码不参与计算。同地址和端口只能有一条记录，主机名不与解析到的 IP 隐式合并。

每条路由只进入对应加密会话，不创建每服务器的本地 TCP 端口。网页使用相对路径，入口校验 Host/Origin，服务端也检查来源。每台服务端仍只允许一个控制会话，不同服务器可同时连接。

配置为 `%APPDATA%\LanDeskClient\connections.json`（格式版本 3），访问凭据为 `LanDeskClient/Access/<哈希>`。原版本 2 连接列表自动移除旧的浏览器开关，保留服务器及凭据标识。更改地址或端口后需要重新输入密码，保存成功删除旧条目的凭据。配置与凭据修改失败会回滚；回滚失败明确报错。

此次从系统 SSH 改为内置连接，**需要重新添加连接**。旧的 `settings.json`、`LanDeskClient/SSH/*` 凭据和 `known_hosts` 保留原位，不读取或自动导入，避免把系统账号密码当成应用访问密码。

设备信任记录为同目录 `known_devices`，首次自动记录，后续变化拒绝，不弹指纹确认也不自动覆盖记录。首次信任不能验证第一次连接的设备身份。删除条目保留信任记录。仅支持 LanDesk 访问密码，不支持系统 SSH 登录、跳板机、SSH 密钥认证或主机证书。

## 构建与验证

使用 Rust stable MSVC、Visual Studio C++ 工具链、Windows SDK 和 CMake：

```sh
cargo fmt --manifest-path client/Cargo.toml --check
cargo test --manifest-path client/Cargo.toml --locked --no-default-features --lib
cargo clippy --manifest-path client/Cargo.toml --locked --all-targets -- -D warnings
cargo build --manifest-path client/Cargo.toml --locked --release --bin LanDeskClient
```

连接核心测试覆盖地址去重、配置增删改、双服务器同端口路由隔离、WebSocket 64 KiB 往返、单台断开互不影响、关闭全部标签超过原 30 秒限制后仍可重连、网络中断后的网页触发认证重连及主动断开后的路由撤销。Windows 工作流额外执行凭据隔离与回滚测试。Windows 实际界面、托盘和到新版服务端的远控仍需实机验收。

官方依据：[GPUI Kit](https://gpui-kit.com/docs/installation/)、[russh 0.64.1](https://docs.rs/russh/0.64.1/russh/)、[Hyper Upgrade](https://docs.rs/hyper/1.12.0/hyper/upgrade/index.html)、[Windows 凭据管理](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credwritew)。
