# LanDesk

Windows / macOS 远程桌面服务与客户端。Client 通过 SSH 连接目标，使用本机浏览器操作远程桌面；也可连接普通 SSH 服务使用网页终端及 SFTP。Client 与 Server 可以在同一台机器同时运行。

完整设计、平台边界、Windows 安装维护、虚拟显示器和验证记录统一见 [架构设计](docs/architecture.md)。客户端操作见 [LanDeskClient](apps/client/README.md)。

## 使用

1. 目标设备启动 LanDeskServer。Mac 打开 `LanDesk.app` 并授予屏幕录制和辅助功能权限；Windows 双击 `LanDeskServer.exe`，管理员授权安装后台服务。无需系统 OpenSSH。
2. 客户端添加“远程桌面”连接，填写目标地址、连接端口（默认 `2222`）和服务端显示的连接密码；无需系统用户名。首次连接记录 SSH 主机密钥，后续变化会拒绝。
3. 点击连接，客户端自动打开动态本机网页地址。浏览器入口只绑定回环地址，不需要设置端口或关闭同机服务端。页面支持键鼠、中文输入、剪贴板、文件上传、分辨率和屏幕选择。
4. 多台目标使用独立连接。关闭最后一个网页会话后 30 秒关闭该 SSH；再次连接从客户端打开新页面。退出 Client 不停止本机 Server。

桌面通道采用 SSH `landesk` subsystem；客户端与服务端须使用本轮配套构建，不支持旧固定端口转发。普通 SSH 终端不受此协议变化影响。启动网页 URL 含短期会话凭证，不应分享。

## 构建

Mac 需要 Rust 和 Apple Command Line Tools：

```sh
python3 scripts/bundle.py
python3 scripts/bundle_macos_client.py --debug
```

服务端输出在 Cargo target 的 `release/bundle/macos/LanDesk.app`；客户端输出在 `apps/client/target/bundle/macos/LanDeskClient.app`。客户端省略 `--debug` 可生成优化构建。开发包使用本机临时签名，没有 Developer ID 或公证；系统隐私授权仍需用户授予。

Windows 由 [Server 工作流](.github/workflows/windows-display.yml) 和 [Client 工作流](.github/workflows/windows-client.yml) 在 MSVC 环境构建。编译成功不等于目标设备实机验证通过。

## 测试

```sh
python3 scripts/check_architecture.py
cargo fmt --all --check
cargo test -p landesk-core -p landesk --locked
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo test --manifest-path apps/client/Cargo.toml --locked --no-default-features --lib --tests
cargo clippy --manifest-path apps/client/Cargo.toml --locked --all-targets -- -D warnings
node --test web/*.test.mjs apps/client/web/*.test.mjs
```

硬件测试与 Windows 管理员维护测试需显式执行，见统一架构文档。音频、透明跳板、中继、NAT 穿透和 Linux 图形桌面服务尚未实现。Mac 虚拟显示器为使用未公开 API 的实验功能。
