# 平台接口与虚拟显示器

Mac 桌面采集和输入已有实机验证。Mac、Windows 桌面统一使用内置 SSH 与独立连接密码，客户端不要求用户名；Mac 不再依赖系统远程登录。0.1.13 首次启动自动生成 8 位数字/大小写字母密码，主界面显示原文，可复制、随机更换及手工修改；再次启动从加密配置恢复。Mac 密钥保存在钥匙串，Windows 使用 DPAPI。按全新安装使用，不兼容旧密码配置。2026-10-15 本机 Mac 已验证首次无密码弹窗、主界面显示生成密码、退出重开恢复同一密码；当前应用提示屏幕录制与辅助功能待授权，本轮未复验远控画面和输入。Windows 0.1.14 安装器自动清理旧服务和连接配置后全新安装，普通启动和重启保留密码；已交叉构建、Clippy 通过、库测试编译通过，尚未执行 Windows 专属测试或实机验证。SSH 终端继续直接连接 Linux 的系统 SSH，不要求桌面或 Linux LanDeskServer。Windows 采集、输入、内置 SSH、开机服务和托盘已接入，Win10 主屏操作已有用户实测，完整 Windows 场景仍待验收；Linux 图形桌面服务待实现。

## 代码边界

- `src/platform/mod.rs`：`DesktopControl` 管权限、键鼠、剪贴板和会话恢复；`CaptureBackend` / `FrameSource` 管画面来源；`VirtualDisplayProvider` 管虚拟显示设备；`FileBackend` / `FileSession` 管文件访问。
- `src/platform/commands.rs`：定义 `InputEvent`、`FileCommand` / `FileResponse`、`CaptureInfo` / `DisplayInfo`。平台实现不接收 `ClientMessage`，不返回无类型 JSON；协议转换集中在 `src/protocol.rs` 和服务端发送边界。
- `src/server_auth.rs`：服务端必须配置非空内部密钥，HTTP 资源和 WebSocket 升级统一先认证；`DesktopControl` 不再负责认证，也没有默认放行路径。Windows 与 Mac 沿用已有 SSH 通道和内部密钥。
- `src/capture.rs`：公共采集调度、命令队列、最新帧转发和显示信息更新，只依赖接口。后端和原生句柄在采集线程内部创建，不要求平台对象实现 `Send`。
- `src/server.rs`：公共 WebSocket 协议、会话所有权、取消和控制命令。`ready.capabilities` 由实际平台适配器提供。
- `src/file_worker.rs`：独立文件线程及取消策略，通过文件接口处理请求。
- `src/platform/macos/`：ScreenCaptureKit、CoreGraphics、AppKit、Enigo、IOKit、Mac 文件系统实现。Mac 输入继续在 AppKit 主线程和 common run-loop modes 运行。
- `src/platform/windows/`：GDI 采集、SendInput、控制台会话服务和内置显示驱动；`src/windows_server_main.rs` 为 Windows 服务入口。
- `client/src/connections.rs`：拥有网页入口、连接任务、取消信号和连接状态；每次连接带独立编号，旧连接事件不能覆盖重连状态。UI 只管理表单、凭据保存和展示，不自动重连。
- `src/main.rs`：组装 Mac 服务。新增系统应实现这些接口并添加自己的启动入口，不把系统判断散布到 WebSocket 控制流程。

网页终端已在客户端实现：本地 xterm.js 网页通过 WebSocket 连接客户端，再由 russh 创建 SSH PTY/Shell，独立于屏幕采集和屏幕权限。交互式命令使用 SSH 账户的系统权限；未提供批量脚本执行接口、任务编排或 Linux 图形文件管理。使用与验证范围见 [客户端说明](../client/README.md)。首轮不添加窗口搬移、镜像、主屏替换或实体屏自动熄灭。

## Mac 虚拟屏幕的使用与生命周期

网页连接失败或断开后，选择“虚拟屏幕（实验）”、填写宽高，再点重新连接。已连接时，在工具条的显示设置中展开“虚拟屏幕（实验）”，点击“使用虚拟屏幕”。默认 1920×1080，宽高为实际像素；现有采集缩放和等比例显示继续保留。

`hello.display` 和 `set_display_source.display` 使用相同结构：

```json
{"kind":"virtual","width":1920,"height":1080}
```

选择已有屏幕：`{"kind":"existing","id":123}`；`id:null` 为系统主屏。握手省略 `display` 表示重连到服务端进程中最后成功选择的屏幕，首次连接为主屏。不会因为采集失败隐式创建虚拟屏或跳到另一块屏。

虚拟设备由服务端持有，网页断开/刷新后继续存在；相同宽高复用同一设备。新建或采集初始化失败不会替换当前选择。新屏采集初始化成功后提交选择，旧屏保留到旧采集对象释放。退出服务端后释放它创建的设备。选择回实体屏时仍保留最后一块虚拟屏到服务退出，以免网页切换触发窗口迁移。配置不落盘，重启服务后重新选择。

创建失败会明确返回错误；实验功能不会安装驱动、改变系统 SSH、关闭实体屏、变更主屏或自动搬动窗口。继承现有采集的 1600 万像素预算。创建虚拟屏本身相当于插入显示器，macOS 仍可能自行调整桌面布局。

## API 依据和验证范围

虚拟设备使用未公开的 `CGVirtualDisplay`，通过当前 `objc2 0.6.3` 调用。未发现 Apple 公开且承诺兼容的同等创建接口。方法声明参考 [Chromium 的实现](https://chromium.googlesource.com/chromium/src/+/HEAD/ui/display/mac/test/virtual_display_util_mac.mm)；调用参数已与本机 macOS 26.5.1 的 Objective-C 方法签名核对，运行时检查所需类和选择器。系统升级仍可能改变行为。实际捕获使用项目既有 `screencapturekit 11.0.0` 和 [Apple ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit/scshareablecontent)。

2026-10-11 本机已验证：创建 1920×1080 设备、读取实际像素尺寸、保持原主屏、复用、释放后从在线显示器列表移除；ScreenCaptureKit 也已输出并成功解码真实 1920×1080 JPEG 首帧，切换 1280×720 后再次解码通过，并验证采集重连复用同一虚拟屏。硬件测试显式执行：

```sh
LANDESK_TEST_VIRTUAL_DISPLAY=1 LANDESK_TEST_VIRTUAL_CAPTURE=1 cargo test --locked --test virtual_display
```

普通 `cargo test` 不创建系统显示器。首帧测试需要测试进程已有屏幕录制权限；未授权时明确失败，不主动申请权限。测试用 `CGGetOnlineDisplayList` 检查移除；本机 `CGDisplayIsOnline` 对无效编号返回 `UINT32_MAX`，不可按非零视为仍在线。

Windows 服务端已完成 x64 EXE 交叉构建及 Windows 目标 Clippy；这不代表 Windows 实机验证通过。2026-10-15 的架构重构通过本机 Rust 库测试、平台契约测试和网页测试，覆盖失败保留旧会话、显示设备持有、重连、无首帧时选择虚拟屏、内置 SSH 传输和内部凭据拒绝路径。Windows 测试包、具体边界及安装步骤见 [Windows 服务端说明](windows-server.md)。

尚未验证：打包 App 下网页切换虚拟屏和真实远程键鼠、没有实体屏的 Mac 启动、合盖/锁屏/FileVault 登录阶段，以及其他 macOS 版本。虚拟屏不是独立用户会话，也不提供这些登录或电源行为的保证。

## WebRTC 与 webrpc

[WebRTC](https://webrtc.org/getting-started/overview) 用于实时媒体和数据传输；[webrpc](https://github.com/webrpc/webrpc) 是根据接口描述生成 RPC 客户端和服务端代码的工具。二者都不创建操作系统显示器。当前保留 SSH + WebSocket，未来替换媒体传输也应复用采集接口，无需改动虚拟显示设备的生命周期。
