# Windows 服务端测试包

已实现 Windows 控制台桌面采集、鼠标键盘、中文输入、文字剪贴板读取、Ctrl+Alt+Delete、自动启动服务，以及配套 1920×1080 / 60 Hz 虚拟显示驱动。网页继续使用悬浮工具条，通过 LanDeskClient 的 SSH 隧道连接。

代码路径覆盖未登录、锁屏和登录后的控制台桌面；这些场景、UAC 和 Win10 无头输出仍待实机验证。当前环境是 Mac，不能将交叉编译通过视为 Windows 远控已验证。

## 安装

目标系统为 Windows 10 / Windows 11 x64，使用管理员 Windows PowerShell 5.1。OpenSSH 系统组件要求 Windows 10 1809 或以上；完整驱动兼容性仍须目标系统验证。完整解压 `LanDeskServer-windows-x64.zip`，在解压目录执行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-LanDeskServer.ps1
```

如果被控机尚未启用 OpenSSH Server，需要安装并启动 SSH 传输组件时执行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-LanDeskServer.ps1 -InstallOpenSSH
```

`-InstallOpenSSH` 使用 Windows 可选功能，需要 Windows Update 或配置的组件来源。Windows 安装该组件会创建 TCP 22 防火墙规则；已有规则保持不变，缺失时脚本只补充专用/域网络的同子网规则。不改现有 SSH 登录策略、账号密码或 RDP 配置。不加此参数时不改 SSH 和防火墙。已安装 LanDesk 后补 SSH，请按下方微软 OpenSSH 文档操作。

安装完成会显示 **Web 访问密钥**，请保存。它与 Windows 登录密码、SSH 密码是三项不同的凭据。服务端密钥只保存在 `%ProgramData%\LanDeskServer\access-key.txt`，目录仅 SYSTEM 和管理员可访问；不会写入 URL、浏览器存储或日志。

服务安装到 `%ProgramFiles%\LanDeskServer`，以 LocalSystem 自动启动。安装会检查签名驱动、创建虚拟显示器，并验证桌面工作进程的 HTTP 入口已启动；这一步不证明能采集出画面。驱动要求重启时，重启后再测试。

为了支持登录界面的 Ctrl+Alt+Delete，安装时若 `SoftwareSASGeneration` 策略不存在，会设置为 `1`（只允许服务），卸载时移除自己创建且未被修改的值。已有禁止策略会明确阻止安装；不覆盖组织策略，不关闭 UAC，不设置自动登录。驱动细节和运行库要求见安装包 `virtual-display.md` 与 `driver/README.md`。

## 连接和验收

1. 在办公机 LanDeskClient 新建 mini 主机连接，选择 **桌面** 模式，填写 mini 的 IP、SSH 端口、Windows SSH 用户名和密码/密钥。
2. 打开网页后输入安装时生成的 Web 访问密钥。此时应看到控制台当前画面；未登录时由用户在 Windows 登录界面输入自己的账号密码。
3. 测试鼠标移动/左右键、滚轮、中文输入、Ctrl+A/C/V、拖动、全屏和采集分辨率。文本粘贴通过 Unicode 输入；应用必须支持 Windows Unicode 输入事件。
4. 用工具条的 Ctrl+Alt+Delete 按钮进入 Windows 安全界面，测试锁屏、解锁、UAC 提示和重新登录。
5. mini 保持无实体显示器，重启后直接用 LanDeskClient 连接；无需先启动 RDP。服务跟随物理控制台，不接管已有 RDP 会话，既有 RDP 桌面不一定是网页看到的桌面。
6. 断开网页再连接，检查按键没有卡住。切换 Windows 用户导致控制台会话编号变化时，工作进程会重建，网页需点重新连接；同一会话内锁屏/解锁会重新绑定输入桌面。

办公机若也装服务端，必须从另一台电脑连接测试：服务端与 LanDeskClient 本地网关均使用 17890，不能在同一 Windows 机器同时运行。办公机控制 mini 时只安装/运行 LanDeskClient，mini 安装服务端。

当前 Windows 不提供文件面板、图片剪贴板、音频、独立多用户桌面或 BIOS/BitLocker 启动前画面。文件面板在 Windows 会话中隐藏，服务不会以 SYSTEM 身份对外提供文件管理。采集采用 GDI → JPEG，最高约 10 帧/秒；驱动的 60 Hz 不等于网页传输 60 帧/秒，不含硬件视频编码。

## 状态与卸载

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-LanDeskServer.ps1 -Action Status
# 需要重新查看访问密钥时，仍须管理员权限：
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-LanDeskServer.ps1 -Action Status -ShowAccessKey
# 停止并移除服务，保留访问密钥、日志、虚拟显示器及 SSH：
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-LanDeskServer.ps1 -Action Uninstall
# 另行移除 LanDesk 虚拟显示设备：
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-VirtualDisplay.ps1 -Action Uninstall
```

服务日志：`%ProgramData%\LanDeskServer\server.log`，最多 2 MB。驱动加载问题查看 `C:\Windows\INF\setupapi.dev.log` 和 `LanDeskDisplay.exe --status`。安装失败会回滚本次服务、策略和新建显示设备；回滚失败会明确报告并保留现场。共享 Driver Store、驱动配置、访问密钥和日志可能保留。可选 SSH 安装失败会单独报告，已完成的 LanDesk 安装和 Windows 组件变更会保留。

## 实现依据与验证边界

- 服务用 [CreateProcessAsUserW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw) 和带控制台 SessionId 的 SYSTEM 主令牌启动工作进程，避免在隔离的 Session 0 里采集。目标通过 [WTSGetActiveConsoleSessionId](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-wtsgetactiveconsolesessionid) 获取；工作进程由 Job Object 管理。
- 采集使用微软的 [GDI 采集方式](https://learn.microsoft.com/en-us/windows/win32/gdi/capturing-an-image)，通过 [OpenInputDesktop](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-openinputdesktop) / [SetThreadDesktop](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setthreaddesktop) 绑定当前桌面。采集/输入线程不创建窗口或钩子。
- 输入采用 [SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)，受 Windows 完整性和桌面权限约束；安全注意序列由服务调用 [SendSAS](https://learn.microsoft.com/en-us/windows/win32/api/sas/nf-sas-sendsas)，不伪造普通按键组合。
- 传输采用系统 [OpenSSH Server](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_install_firstuse)，桌面 HTTP 仅绑定 `127.0.0.1:17890`，WebSocket 校验 Origin、Host 和独立随机访问密钥后才启动采集及输入。
- Rust Windows API 绑定为项目锁定的 `windows-sys 0.61.2`。本机测试包通过官方 LLVM-MinGW 和 Rust `x86_64-pc-windows-gnullvm` 目标交叉构建；Windows CI 用 MSVC 构建。尚未在 Windows 实机执行，待测试包验收。

2026-10-14 本机验证：38 项 Rust 库测试、3 项平台契约测试、26 项网页测试通过；Windows 目标 Clippy、Mac Clippy、Rust 格式、UTF-8、diff 及安装脚本语法检查通过。Windows x64 两个 EXE 和 Windows 库测试程序均已完成交叉链接，库测试程序尚未在 Windows 执行。检查过服务端 EXE 的 DLL 导入，无额外 LLVM/GCC 运行库 DLL 依赖；仍需 Windows 系统自带的 DLL。这些结果不替代驱动安装、真实采集、登录切换和键鼠实测。
