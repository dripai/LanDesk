# LanDesk

Mac 运行 Rust 远控服务，Windows 通过 SSH 加密隧道和浏览器操作桌面。Windows 多服务器客户端为 [LanDeskClient](client/README.md)，使用 GPUI / gpui-component；原有 `scripts/LanDesk.cmd` 仍可单独使用，两者不能同时占用本机 17890 端口。

## 使用

1. Mac 打开 `LanDesk.app`，在系统设置中授予屏幕录制和辅助功能权限，然后重新打开应用。若系统另行请求远程桌面权限，需要允许。启动不会自动申请权限。
2. Mac 系统设置中开启“远程登录”。Windows 在 LanDeskClient 填写 Mac 地址、用户名、SSH 端口和密码，首次连接自动记录主机密钥，之后密钥变化会拒绝连接。
3. 客户端为每台服务器打开 `http://127.0.0.1:17890/s/<地址哈希>/`，浏览器自动连接桌面；使用独立连接脚本时仍打开根地址，无需六位连接码。断开或失败后，可点击“重新连接”。
4. 点击远程画面操作键鼠。Windows Ctrl 映射为 Mac Command；普通文字、中文和全角标点通过本地输入法提交。
5. 工具条可拖动、收起，提供文字输入、文字复制和粘贴、文件面板、采集分辨率、全屏和断开。浏览器地址栏通过全屏隐藏。
6. 右侧文件面板上半部显示当前目录的子目录，下半部显示文件。点击目录或路径导航切换；拖动画面与面板之间的分隔条调整宽度，也可聚焦分隔条后用左右方向键调整。右侧空白足够时自动显示，工具条按钮可手动展开和收起。
7. 拖普通文件到下半部或点“上传”，文件写入当前目录。同名拒绝覆盖；不支持整目录上传、删除或下载。
8. Ctrl+V 或粘贴按钮将 Windows 的文字或单张图片粘贴到 Mac 当前焦点。图片分块传输，写入 Mac 系统剪贴板后发送 Command+V，目标应用需支持图片粘贴；浏览器提供的非 PNG 图片会先转换为 PNG。Mac 到 Windows 仍只支持文字：先在远程 Mac 复制，再点击“复制 Mac 文字”。不自动同步，不支持文件剪贴板。
9. 分辨率可选原始像素、宽 1280/1920/2560 或自定义宽度（640 到屏幕原始宽度）。高度按比例计算，每次重连恢复原始像素；不会改变 Mac 系统分辨率，也不会拉伸画面。
10. Mac 最小化或隐藏窗口不停止服务，关闭主窗口才退出。Windows 客户端默认关闭或最小化后留在托盘；最后一个远控连接结束后连续 30 秒无连接，会自动关闭 SSH，需回客户端重新连接。首次连接后未打开远控同样计时；HTTP 保活不延长倒计时。右键托盘可立即断开或退出。

## 显示器与电源

已移除物理屏幕上的黑色遮罩及其采集排除逻辑，远端可以看到 LanDesk 窗口。建立的远控会话持有 IOKit `PreventUserIdleSystemSleep` 断言，结束时释放；它允许显示器休眠，同时阻止系统因空闲自动睡眠。合盖、主动睡眠和低电量不在此保证范围内。

macOS 提供 `pmset displaysleepnow`，用于立即关闭显示器而非让整机睡眠。Apple 也提供“显示器关闭时防止自动睡眠”设置。可通过 `pmset -g cap` 查询当前设备是否支持 `displaysleep`。熄屏后 ScreenCaptureKit 是否持续输出、远程操作是否唤醒屏幕仍待实测，因此目前未提供“一直熄屏远控”开关。

官方依据：[Apple 显示器与睡眠设置](https://support.apple.com/en-nz/guide/mac-help/-mchle41a6ccd/mac)、[IOPMAssertionCreateWithName](https://developer.apple.com/documentation/iokit/1557134-iopmassertioncreatewithname)、[CGDisplayIsAsleep](https://developer.apple.com/documentation/coregraphics/cgdisplayisasleep(_:))。命令与断言行为同时核对了本机 SDK 的 `pmset(1)` 和 `IOPMLib.h`。

## 边界

- Mac 服务端要求 macOS 14.2 或更高。单显示器、单控制会话；显示器布局变化时断开。
- ScreenCaptureKit 按 Retina 实际像素采集，JPEG 质量 92，目标 15 帧/秒，最多 1600 万像素。帧率和带宽取决于分辨率、设备和网络，慢连接只保留最新画面。
- 只监听 `127.0.0.1:17890`；SSH 认证并加密远程链路，浏览器连接仍严格校验 Host 和 Origin，不再要求应用连接码。服务本身不识别 SSH 用户：能访问 Mac 本机端口的程序或 SSH 隧道均可发起远控。心跳丢失最长 15 秒结束会话。
- 文件范围限当前 Mac 用户目录。通过目录句柄与 `O_NOFOLLOW` 防止路径及符号链接穿越；单文件最多 512 MiB，64 KiB 分块，完成并保存后用 `RENAME_EXCL` 原子发布。同名竞态也不覆盖，中断清理临时文件。每个目录最多 5000 条目，不支持非 UTF-8 文件名。上传权限 0600，不保留 Windows 元数据和可执行位。系统受保护目录可能另需文件访问授权。
- 纯文字单次最多 64 KiB UTF-8，不含空字符。剪贴板图片最多 10 MiB PNG、1600 万像素、边长 8192；Mac 完整解码校验后才修改剪贴板。图片只在内存中传输，30 秒无后续分块丢弃，断开清理；图片分块与文件上传分别处理。浏览器剪贴板拒绝授权时显示错误；Ctrl+V 可使用浏览器原生粘贴事件。
- 已移除 Mac 连接码设置。旧的 `~/Library/Application Support/LanDesk/settings.json` 不再读取或写入；升级不会删除该文件。
- 统一端口下的服务器标签共享浏览器同源；哈希路径用于连接去重和请求分流，不提供网页脚本之间的安全隔离。此模式用于自己信任的 Mac。参见 [URL Origin 标准](https://url.spec.whatwg.org/#concept-url-origin)。
- Windows 客户端使用 SSH 密码认证。可勾选“记住密码”，密码存入 Windows 凭据管理器，不写入配置文件；取消勾选并保存或连接后删除。首次自动记录主机密钥，变化时拒绝；首次使用信任无法验证第一次连接的主机身份。其他限制及配置路径见 [客户端说明](client/README.md)。
- 尚未提供 Mac 到 Windows 的图片复制、音频、驱动级隐私屏、合盖远控或系统登录界面控制保证。

## 构建与验证

Mac 构建需要 Rust 与 Apple Command Line Tools：

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
node --check web/app.js
node --check web/files.js
node --test web/*.test.mjs
python3 scripts/bundle.py
```

应用输出在 Cargo target 目录的 `release/bundle/macos/LanDesk.app`。`build.rs` 从 `xcrun` 获取实际 Swift 链接路径以支持 Command Line Tools。开发包使用临时签名，更新后可能需要重新授权；只重新添加 LanDesk，不重置其他应用权限。

Windows 构建由 [Windows client 工作流](.github/workflows/windows-client.yml) 执行。多服务器客户端此前已通过 [Windows 格式、14 项核心测试、Clippy 及 Release 构建](https://github.com/dripai/LanDesk/actions/runs/37833212339)，但该包随后实机发现右侧表单空白，不再推荐使用。已在 `0b54c8a` 修正左侧滚动容器占满窗口的问题；修复提交已触发新构建，本轮按要求不等待结果。请在 [Windows client 构建列表](https://github.com/dripai/LanDesk/actions/workflows/windows-client.yml) 选择包含该修复的成功构建，下载 `LanDeskClient-windows-x64` 后解压运行 `target/release/LanDeskClient.exe`。

本轮已验证：23 项 Mac Rust 测试、10 项网页测试、12 项本机客户端核心测试，格式及 Clippy 检查通过。Windows 的 14 项测试包含凭据隔离、账户绑定、删除及配置失败回滚，全部通过。双服务器集成测试通过真实本地 SSH 会话验证同端口路径分流、64 KiB WebSocket 往返、单台断开不影响其他服务器、30 秒无远控自动关闭及刷新宽限期。

独立浏览器模拟会话已验证 `/s/<哈希>/` 下资源加载、自动连接、断开和手动重连，以及服务器标签标题。Mac 服务测试确认来源缺失、跨站来源、Host 不匹配被拒绝，第二个控制会话不能接管已有会话。

本轮 Mac 应用已打包，尚未替换正在运行的应用；新客户端需要同时更新 Mac 端网页。Windows 右侧表单空白已根据 GPUI Component 0.7.1 实际滚动容器源码修正：在外层明确设置 220 像素宽度，避免默认全宽将详情区挤成零宽；修复后的 Windows 界面仍待实机复验。托盘、多台真实 Mac 远控以及熄屏持续采集仍未验收。编译和核心测试不代替这些实机验证。

此前版本已实测 Mac 画面、键鼠、中文、目录导航和面板调宽；Mac 窗口最小化后仍可连接，电源防睡眠断言随会话建立和释放。图片粘贴按钮已在本机真实会话使测试图片进入目标应用输入框，Windows Ctrl+V 到 Mac 的完整图片链路尚未实测。

核心依赖：`screencapturekit 11.0.0`、`enigo 0.6.1`、`axum 0.8.9`；客户端为 `gpui-kit 0.7.1`、`russh 0.64.1`、`tray-icon 0.21.2`，分别锁定在两份 Cargo.lock。

实现依据：[Apple 屏幕采集](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-in-macos)、[动态采集配置](https://developer.apple.com/documentation/screencapturekit/scstream/updateconfiguration(_:completionhandler:))、[AppKit 剪贴板](https://developer.apple.com/documentation/appkit/nspasteboard/string(fortype:))、[GPUI Kit 平台要求](https://gpui-kit.com/docs/installation/)。文件面板参考常见远程文件管理器的路径导航和紧凑列表，保留上下分区；参考：[AnyDesk 文件管理](https://support.anydesk.com/file-manager-and-file-transfer)。

## 文件上传的后续评估

当前开发小文件交互可继续使用现有的 512 MiB 单文件上限与分块传输。断点续传适合频繁传大文件或网络不稳定的情况，尚未实现；实现前需确定部分文件的保留期限、磁盘配额和重连认证，并校验源文件与目标目录身份，不能只保存一个偏移量。当前断开仍清理未完成上传。

可配置上传上限有价值，建议由 Mac 服务端配置并告知客户端，客户端只做提前提示；尚未提供此设置，不允许浏览器自行提高服务端限制。
