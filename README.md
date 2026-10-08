# LanDesk

Mac 运行 Rust 远控服务，Windows 通过 SSH 加密隧道和浏览器操作桌面。Windows 启动器为 [LanDeskClient](client/README.md)，使用 GPUI / gpui-component；原有 `scripts/LanDesk.cmd` 仍可单独使用，两者不能同时占用本机 17890 端口。

## 使用

1. Mac 打开 `LanDesk.app`，在系统设置中授予屏幕录制和辅助功能权限，然后重新打开应用。若系统另行请求远程桌面权限，需要允许。启动不会自动申请权限。
2. Mac 系统设置中开启“远程登录”。Windows 在 LanDeskClient 填写 Mac 地址、用户名、SSH 端口和密码，首次连接核对主机指纹。
3. 浏览器打开 `http://127.0.0.1:17890`，输入 Mac 窗口显示的六位连接码。可在 Mac 保存固定码，或选择每次启动生成随机码；连接期间不可改码。
4. 点击远程画面操作键鼠。Windows Ctrl 映射为 Mac Command；普通文字、中文和全角标点通过本地输入法提交。
5. 工具条可拖动、收起，提供文字输入、文字复制和粘贴、文件面板、采集分辨率、全屏和断开。浏览器地址栏通过全屏隐藏。
6. 右侧文件面板上半部显示当前目录的子目录，下半部显示文件。点击目录或路径导航切换；拖动画面与面板之间的分隔条调整宽度，也可聚焦分隔条后用左右方向键调整。右侧空白足够时自动显示，工具条按钮可手动展开和收起。
7. 拖普通文件到下半部或点“上传”，文件写入当前目录。同名拒绝覆盖；不支持整目录上传、删除或下载。
8. Ctrl+V 或粘贴按钮将 Windows 的纯文字输入 Mac 当前焦点。先在远程 Mac 复制文字，再点击“复制 Mac 文字”取回 Windows 剪贴板。不自动同步，不支持图片或文件剪贴板。
9. 分辨率可选原始像素、宽 1280/1920/2560 或自定义宽度（640 到屏幕原始宽度）。高度按比例计算，每次重连恢复原始像素；不会改变 Mac 系统分辨率，也不会拉伸画面。
10. Mac 最小化或隐藏窗口不停止服务，关闭主窗口才退出。Windows 客户端默认关闭或最小化后留在托盘；右键托盘可断开或退出。

## 显示器与电源

已移除物理屏幕上的黑色遮罩及其采集排除逻辑，远端可以看到 LanDesk 窗口。认证后的远控会话持有 IOKit `PreventUserIdleSystemSleep` 断言，结束时释放；它允许显示器休眠，同时阻止系统因空闲自动睡眠。合盖、主动睡眠和低电量不在此保证范围内。

macOS 提供 `pmset displaysleepnow`，用于立即关闭显示器而非让整机睡眠。Apple 也提供“显示器关闭时防止自动睡眠”设置。本机是 Apple M1、macOS 26.5.1；熄屏后 ScreenCaptureKit 是否持续输出、远程操作是否唤醒屏幕仍待实测，因此目前未提供“一直熄屏远控”开关。

官方依据：[Apple 显示器与睡眠设置](https://support.apple.com/en-nz/guide/mac-help/-mchle41a6ccd/mac)、[IOPMAssertionCreateWithName](https://developer.apple.com/documentation/iokit/1557134-iopmassertioncreatewithname)、[CGDisplayIsAsleep](https://developer.apple.com/documentation/coregraphics/cgdisplayisasleep(_:))。命令与断言行为同时核对了本机 SDK 的 `pmset(1)` 和 `IOPMLib.h`。

## 边界

- Mac 服务端要求 macOS 14.2 或更高，当前只验证 M1 / macOS 26.5.1。单显示器、单控制会话；显示器布局变化时断开。
- ScreenCaptureKit 按 Retina 实际像素采集，JPEG 质量 92，目标 15 帧/秒，最多 1600 万像素。帧率和带宽取决于分辨率、设备和网络，慢连接只保留最新画面。
- 只监听 `127.0.0.1:17890`；SSH 加密远程链路，浏览器额外校验来源与六位连接码，每分钟最多五次失败认证。心跳丢失最长 15 秒结束会话。
- 文件范围限当前 Mac 用户目录。通过目录句柄与 `O_NOFOLLOW` 防止路径及符号链接穿越；单文件最多 512 MiB，64 KiB 分块，完成并保存后用 `RENAME_EXCL` 原子发布。同名竞态也不覆盖，中断清理临时文件。每个目录最多 5000 条目，不支持非 UTF-8 文件名。上传权限 0600，不保留 Windows 元数据和可执行位。系统受保护目录可能另需文件访问授权。
- 纯文字单次最多 64 KiB UTF-8，不含空字符。浏览器剪贴板拒绝授权时显示错误；Ctrl+V 可使用浏览器原生粘贴事件。
- Mac 设置在 `~/Library/Application Support/LanDesk/settings.json`。固定码明文保存在当前用户可读写的 0600 文件，随机模式不保存生成的码。原子保存，损坏设置明确报错。
- Windows 客户端使用 SSH 密码认证，密码不写入配置。首次主机密钥需核对确认，变化时拒绝。其他限制及配置路径见 [客户端说明](client/README.md)。
- 尚未提供图片剪贴板、音频、驱动级隐私屏、合盖远控或系统登录界面控制保证。

## 构建与验证

Mac 本机使用 Rust 1.99.0 和 Apple Command Line Tools：

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
node --check web/app.js
node --check web/files.js
node --test web/*.test.mjs
python3 scripts/bundle.py
```

应用输出在 Cargo target 目录的 `release/bundle/macos/LanDesk.app`。`build.rs` 从 `xcrun` 获取实际 Swift 链接路径以支持 Command Line Tools。开发包使用临时签名，更新后可能需要重新授权；只重新添加 LanDesk，不重置其他应用权限。

Windows 构建由 [Windows client 工作流](.github/workflows/windows-client.yml) 执行，下载产物后解压运行。当前源码的 Windows 编译、托盘和真实连接验证仍在进行，不能将提交工作流视为构建已通过。

已完成的源码验证：30 项 Mac Rust 测试、6 项网页键盘和宽度边界测试、客户端设置原子保存与损坏配置测试。本轮 UI 实机、熄屏和 Windows 客户端结果待补充。

此前版本已实际验证远程画面、键鼠、中文文本发送、应用最小化与恢复保持连接；这些历史结果不代替本轮新功能验收。

核心依赖：`screencapturekit 11.0.0`、`enigo 0.6.1`、`axum 0.8.9`；客户端为 `gpui-kit 0.7.1`、`russh 0.64.1`、`tray-icon 0.21.2`，分别锁定在两份 Cargo.lock。

实现依据：[Apple 屏幕采集](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-in-macos)、[动态采集配置](https://developer.apple.com/documentation/screencapturekit/scstream/updateconfiguration(_:completionhandler:))、[AppKit 剪贴板](https://developer.apple.com/documentation/appkit/nspasteboard/string(fortype:))、[GPUI Kit 平台要求](https://gpui-kit.com/docs/installation/)。文件面板参考常见远程文件管理器的路径导航和紧凑列表，保留上下分区；参考：[AnyDesk 文件管理](https://support.anydesk.com/file-manager-and-file-transfer)。
