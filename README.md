# LanDesk

Mac 运行 Rust 远控服务，Windows 通过 SSH 加密隧道和浏览器操作桌面。Windows 多服务器客户端为 [LanDeskClient](client/README.md)，使用 GPUI / gpui-component；原有 `scripts/LanDesk.cmd` 仍可单独使用，两者不能同时占用本机 17890 端口。

## 实验分支

`main` 保留已验证的 Mac 稳定版本；`codex/platform-virtual-display` 开发公共平台接口与 Mac 虚拟显示器。Windows 服务端后续接入。入口、接口、生命周期和实际验证范围见 [平台与虚拟显示器说明](docs/platforms.md)。

## 使用

1. Mac 打开 `LanDesk.app`，在系统设置中授予屏幕录制和辅助功能权限，然后重新打开应用。若系统另行请求远程桌面权限，需要允许。启动不会自动申请权限。
2. Mac 系统设置中开启“远程登录”。Windows 在 LanDeskClient 填写 Mac 地址、用户名、SSH 端口和密码，首次连接自动记录主机密钥，之后密钥变化会拒绝连接。
3. 客户端为每台服务器打开 `http://127.0.0.1:17890/s/<地址哈希>/`，浏览器自动连接桌面；使用独立连接脚本时仍打开根地址，无需六位连接码。断开或失败后，可点击“重新连接”。
4. 点击远程画面操作键鼠。Windows Ctrl 映射为 Mac Command；普通文字、中文和全角标点通过本地输入法提交。方向键、退格、Delete、Enter、Tab、Home/End、PageUp/PageDown 支持长按重复，也保留 Shift 等组合键状态；松开或失焦时释放。
5. 工具条可拖动、收起，提供文字输入、文字复制和粘贴、文件面板、采集分辨率、全屏和断开。浏览器地址栏通过全屏隐藏。
6. 右侧文件面板顶部仅保留路径及右侧的上级目录、刷新按钮；目录和文件使用 26 像素紧凑行高。上半部显示当前目录的子目录，下半部显示文件。点击目录或路径导航切换；拖动画面与面板之间的分隔条调整宽度，也可聚焦分隔条后用左右方向键调整。右侧空白足够时自动显示，工具条按钮可手动展开和收起。
7. 拖普通文件到下半部或点“上传”，文件写入当前目录。同名拒绝覆盖；不支持整目录上传、删除或下载。
8. Ctrl+V 或粘贴按钮将 Windows 的文字或单张图片粘贴到 Mac 当前焦点。图片分块传输，写入 Mac 系统剪贴板后发送 Command+V，目标应用需支持图片粘贴；浏览器提供的非 PNG 图片会先转换为 PNG。Ctrl+C 会在远程 Mac 执行复制，并将文字写入 Windows 剪贴板；也可点击“复制 Mac 文字”读取已有文字。同步由快捷键或按钮触发，不后台轮询，不支持文件剪贴板。浏览器可能要求剪贴板权限，拒绝时提示错误并保持远控。
9. 分辨率可选原始像素、宽 1280/1920/2560 或自定义宽度（640 到屏幕原始宽度）。高度按比例计算，每次重连恢复原始像素；不会改变 Mac 系统分辨率，也不会拉伸画面。
10. Mac 最小化或隐藏窗口不停止服务，关闭主窗口才退出。Windows 客户端默认关闭或最小化后留在托盘；最后一个远控连接结束后连续 30 秒无连接，会自动关闭 SSH，需回客户端重新连接。首次连接后未打开远控同样计时；HTTP 保活不延长倒计时。右键托盘可立即断开或退出。

## 显示器与电源

已移除物理屏幕上的黑色遮罩及其采集排除逻辑，远端可以看到 LanDesk 窗口。建立的远控会话持有 IOKit `PreventUserIdleSystemSleep` 断言，结束时释放；它允许显示器休眠，同时阻止系统因空闲自动睡眠。合盖、主动睡眠和低电量不在此保证范围内。

macOS 提供 `pmset displaysleepnow`，用于立即关闭显示器而非让整机睡眠。Apple 也提供“显示器关闭时防止自动睡眠”设置。可通过 `pmset -g cap` 查询当前设备是否支持 `displaysleep`。熄屏后 ScreenCaptureKit 是否持续输出、远程操作是否唤醒屏幕仍待实测，因此目前未提供“一直熄屏远控”开关。

官方依据：[Apple 显示器与睡眠设置](https://support.apple.com/en-nz/guide/mac-help/-mchle41a6ccd/mac)、[IOPMAssertionCreateWithName](https://developer.apple.com/documentation/iokit/1557134-iopmassertioncreatewithname)、[CGDisplayIsAsleep](https://developer.apple.com/documentation/coregraphics/cgdisplayisasleep(_:))。命令与断言行为同时核对了本机 SDK 的 `pmset(1)` 和 `IOPMLib.h`。

## 边界

- Mac 服务端要求 macOS 14.2 或更高。支持多显示器，默认连接主屏，可在分辨率面板选择显示器；保持单控制会话。系统显示尺寸变化时重建所选屏幕的采集，鼠标每次按当前屏幕坐标范围定位，包含副屏的非零或负坐标原点。所选显示器被拔除时明确报错。
- ScreenCaptureKit 按 Retina 实际像素采集，JPEG 质量 92，目标 15 帧/秒，最多 1600 万像素。帧率和带宽取决于分辨率、设备和网络，慢连接只保留最新画面。
- 只监听 `127.0.0.1:17890`；SSH 认证并加密远程链路，浏览器连接仍严格校验 Host 和 Origin，不再要求应用连接码。服务本身不识别 SSH 用户：能访问 Mac 本机端口的程序或 SSH 隧道均可发起远控。Mac 服务端不设置握手、首帧、键鼠初始化/操作、画面发送或心跳等待期限；网页关闭、主动断开、网络报错或退出服务时结束会话，初始化尚未完成也会释放占用。网络静默中断的识别取决于底层 TCP/SSH，不再按 15 秒心跳判定断线。客户端 SSH 建连及无人连接 30 秒关闭隧道的现有规则不变。
- 文件范围限当前 Mac 用户目录。通过目录句柄与 `O_NOFOLLOW` 防止路径及符号链接穿越；单文件最多 512 MiB，64 KiB 分块，完成并保存后用 `RENAME_EXCL` 原子发布。同名竞态也不覆盖，中断清理临时文件。目录每批返回 500 条，通过“加载更多”继续浏览，不再因超过 5000 条目而整页失败；扫描期间已被删除的条目略过。不支持非 UTF-8 文件名。上传权限 0600，不保留 Windows 元数据和可执行位。系统受保护目录可能另需文件访问授权。
- 文件访问使用独立工作线程，不阻塞画面、心跳和断开处理。单次请求等待 10 秒后报错并取消文件会话；系统调用返回后清理未完成上传。系统授权弹窗仍需用户处理，旧访问未返回时文件面板会提示等待，远控可以继续或重连。全局最多一个文件工作线程，防止重连积累阻塞任务。若完成上传时恰逢超时，请刷新目录确认最终结果。
- 纯文字单次最多 64 KiB UTF-8，不含空字符。剪贴板图片最多 10 MiB PNG、1600 万像素、边长 8192；Mac 完整解码校验后才修改剪贴板。图片只在内存中传输，30 秒无后续分块丢弃，断开清理；图片分块与文件上传分别处理。浏览器剪贴板拒绝授权时显示错误；Ctrl+V 可使用浏览器原生粘贴事件。
- 已移除 Mac 连接码设置。旧的 `~/Library/Application Support/LanDesk/settings.json` 不再读取或写入；升级不会删除该文件。
- 统一端口下的服务器标签共享浏览器同源；哈希路径用于连接去重和请求分流，不提供网页脚本之间的安全隔离。此模式用于自己信任的 Mac。参见 [URL Origin 标准](https://url.spec.whatwg.org/#concept-url-origin)。
- Windows 客户端使用 SSH 密码认证。可勾选“记住密码”，密码存入 Windows 凭据管理器，不写入配置文件；取消勾选并保存或连接后删除。首次自动记录主机密钥，变化时拒绝；首次使用信任无法验证第一次连接的主机身份。其他限制及配置路径见 [客户端说明](client/README.md)。
- 尚未提供 Mac 到 Windows 的图片复制、音频、驱动级隐私屏、合盖远控或系统登录界面控制保证。

键鼠队列由注册在 AppKit common run-loop modes 的定时器处理，窗口按钮跟踪、拖动和模态循环期间也会处理鼠标松开及会话清理；不再只依赖外层事件循环。对应真实主线程测试覆盖普通、鼠标跟踪、模态循环及定时器清理；2026-10-11 已安装新版并恢复权限，用户实测远程最小化后仍能操作，断开后网页重连正常。[Apple Run Loops](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/Multithreading/RunLoopManagement/RunLoopManagement.html)。

采集由单独线程创建和清理，ScreenCaptureKit 停止采集的同步等待不占用连接任务。键鼠和剪贴板操作与画面发送分开；输入队列有界，连续鼠标移动合并，键盘和按钮保持顺序。前端解码期间保留最新待显示帧，避免最后一帧被丢弃。采集异常通过 SCStreamDelegate 上报，避免无提示停帧。

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

文件权限阻塞测试通过真实本地 WebSocket 配合模拟阻塞文件操作，覆盖画面继续发送、主动断开、浏览器关闭释放会话、10 秒文件超时、重连线程数量限制、迟到结果丢弃和未完成上传清理。取消连接超时的回归测试覆盖超过原 8 秒的握手、超过原 3 秒的键鼠初始化、超过原 15 秒无心跳仍传帧，以及初始化等待中断开释放占用。真实 macOS 文件授权弹窗及 VNC 退出导致的偶发断线仍待实机复验。项目锁定 Tokio 1.53.2；其[官方文档](https://docs.rs/tokio/1.53.2/tokio/task/fn.spawn_blocking.html)说明已开始的阻塞任务不能强制取消，长期工作循环应使用独立线程，因此文件超时只取消请求及后续操作，不伪称中止系统调用。

此前已验证：12 项本机客户端核心测试；Windows 的 14 项测试包含凭据隔离、账户绑定、删除及配置失败回滚，全部通过。双服务器集成测试通过真实本地 SSH 会话验证同端口路径分流、64 KiB WebSocket 往返、单台断开不影响其他服务器、30 秒无远控自动关闭及刷新宽限期。

独立浏览器模拟会话已验证 `/s/<哈希>/` 下资源加载、自动连接、断开和手动重连，以及服务器标签标题。Mac 服务测试确认来源缺失、跨站来源、Host 不匹配被拒绝，第二个控制会话不能接管已有会话。

稳定基线验证：37 项 Rust 测试、19 项网页测试和 Clippy 通过。覆盖输入等待期间继续传帧与断开、输入队列顺序、5011 项目录分页、最新帧解码、编辑键长按、Ctrl+C/Ctrl+V 协议与浏览器剪贴板调用、重连后的迟到响应和剪贴板拒绝授权。长按使用浏览器的 [KeyboardEvent.repeat](https://www.w3.org/TR/uievents/#dom-keyboardevent-repeat)，沿用 Enigo 0.6.1 的按下/释放接口。以上为本机自动化测试。2026-10-11 已安装新版并恢复两项系统权限，本机浏览器实测最小化后连接、1920×1200 真实画面、主动断开后重连、复制按钮将 Mac 测试文字写入浏览器剪贴板、粘贴按钮将浏览器文字输入 Mac 前台应用。用户已进一步确认远程 Ctrl+C 可将 Mac 文字复制到 Windows。Windows 到 Mac 的 Ctrl+V 完整链路、多显示器切换、采集中更改系统分辨率和真实采集异常恢复仍待实机验收；同机按钮测试不代替跨机快捷键验证。

此前版本已实测 Mac 画面、键鼠、中文、目录导航和面板调宽；Mac 窗口最小化后仍可连接，电源防睡眠断言随会话建立和释放。图片粘贴按钮已在本机真实会话使测试图片进入目标应用输入框，Windows Ctrl+V 到 Mac 的完整图片链路尚未实测。

核心依赖：`screencapturekit 11.0.0`、`enigo 0.6.1`、`axum 0.8.9`；客户端为 `gpui-kit 0.7.1`、`russh 0.64.1`、`tray-icon 0.21.2`，分别锁定在两份 Cargo.lock。

实现依据：[Apple 显示器全局坐标](https://developer.apple.com/documentation/coregraphics/cgdisplaybounds(_:))、[采集停止错误回调](https://developer.apple.com/documentation/screencapturekit/scstreamdelegate/stream(_:didstopwitherror:))、[Apple 屏幕采集](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-in-macos)、[动态采集配置](https://developer.apple.com/documentation/screencapturekit/scstream/updateconfiguration(_:completionhandler:))、[AppKit 剪贴板](https://developer.apple.com/documentation/appkit/nspasteboard/string(fortype:))、[GPUI Kit 平台要求](https://gpui-kit.com/docs/installation/)。文件面板参考常见远程文件管理器的路径导航和紧凑列表，保留上下分区；参考：[AnyDesk 文件管理](https://support.anydesk.com/file-manager-and-file-transfer)。

## 文件上传的后续评估

当前开发小文件交互可继续使用现有的 512 MiB 单文件上限与分块传输。断点续传适合频繁传大文件或网络不稳定的情况，尚未实现；实现前需确定部分文件的保留期限、磁盘配额和重连认证，并校验源文件与目标目录身份，不能只保存一个偏移量。当前断开仍清理未完成上传。

可配置上传上限有价值，建议由 Mac 服务端配置并告知客户端，客户端只做提前提示；尚未提供此设置，不允许浏览器自行提高服务端限制。
