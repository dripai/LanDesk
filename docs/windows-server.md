# Windows 服务端 0.1.11 测试包

双击 `LanDeskServer.exe` 即可安装，接受 Windows 管理员授权后设置 LanDesk 连接密码。密码留空会随机生成并显示 8 位数字/大小写字母密码。安装完成打开设置窗口；后续普通启动默认进入托盘。0.1.12 设置窗口新增“修改连接密码”，可手工修改或留空随机生成；确认保存前不改变密码，保存后用于后续连接，当前连接保留。

**安装、更新、卸载、日志导出都在 Rust EXE 内执行，通过 Windows 原生 API 完成。不调用 PowerShell、batch、sc.exe、WinForms，也不要求系统 OpenSSH。只复制这一个 EXE 即可安装，签名驱动和配置模板已嵌入。** 管理员授权和 Windows 驱动签名验证仍然需要。

目标为 Windows 10 / 11 x64。0.1.7–0.1.9 的用户实机日志确认 Win10 原生安装、卸载、更新、服务、SSH 和首帧生成成功；0.1.10 用户在无实体显示器、重启后未连接 RDP 的 mini 上已通过 Web 登录并操作主屏，登录后虚拟副屏也显示桌面和任务栏。0.1.11 修复连接初始化时工具条偏到左侧的问题，已通过网页测试与 Windows 交叉构建，目标设备上的布局待复测。Windows 原生 API 测试加入 CI，本轮未运行；Win11、UAC、锁屏后再次登录等未全面验收。

## 连接

在办公机 LanDeskClient 选择桌面模式：

| 设置 | 值 |
|---|---|
| 主机 | mini 的 IP |
| SSH 端口 | 2222；重装保留已有配置端口 |
| 用户名 | LanDeskClient 0.1.1 桌面模式不需要填写 |
| 认证 | LanDesk 连接密码 |
| 密码 | 安装时设置或生成的 LanDesk 密码 |

只输入一次 LanDesk 密码，网页自动连接。未登录时操作 Windows 自己的登录界面；Windows 登录密码在远程画面里输入。服务不自动登录、不修改账号或 RDP、不切换系统用户。采集的是物理控制台，可能与已有 RDP 会话不同。跨公网需要可达的地址/端口或已有组网，本版没有 NAT 穿透和中继。

选屏只改变采集和输入目标，不改 Windows 主屏或搬动窗口。虚拟屏当前可作为扩展副屏；应用窗口仍可能出现在主屏或上次的位置。有主屏且无头冷启动也能稳定显示和控制时，直接使用主屏即可；“主屏”标记不证明连接了实体显示器，也不证明卸载虚拟驱动后仍能无头输出。当前保留驱动和显示布局。

0.1.11 在显示会话后才计算画面区域宽度，避免隐藏区域的宽度 0 将工具条居中点推到左边。首次连接与重新连接沿用顶部居中布局；拖动功能保持现有行为。

密码以 Argon2id 散列保存，无法恢复明文。密码、设备密钥和内部管道凭据位于 `%ProgramData%\LanDeskServer\transport\config.json`，普通用户不可读。升级和重装保留原身份及密码，不重新生成已安装服务的身份。

## 安装与更新

安装配置 `LanDeskTransport`（LocalService）和 `LanDeskServer`（LocalSystem）两个服务。桌面服务依赖通信服务。专用防火墙规则只允许当前服务端程序的 SSH TCP 端口入站，适用于所有网络配置文件。

已有运行中的 MttVDD 时直接复用，保留屏幕数量、分辨率、配置和归属；没有设备时保留现有 VDDPATH/XML，在原配置下安装 LanDesk 设备。没有 VDDPATH 使用上游默认 `C:\VirtualDisplayDriver`。新配置提供 1920×1080、60 Hz、SDR 显示器。Windows 提示驱动需要重启时，先重启再验收画面。

安装目录已有字符串文件、驱动目录等内容时保留。已有 LanDesk 程序替换前备份；相同 EXE 直接复用。没有归属标记且内容不同的同名 EXE 会报告冲突，不覆盖。

双击新版本 EXE 会更新已安装服务，保留密码、设备和启动设置，短暂中断远控。失败时停止新程序、恢复原 EXE、权限、应用列表版本和原服务运行状态；恢复失败会报告具体步骤，保留原程序备份路径。

新安装失败时按逆序回滚本次创建的服务、规则、入口、策略和显示设备。某个回滚步骤失败仍继续其余恢复，最终报告所有失败。配置、日志和 Windows 已暂存的共享驱动包可能保留。

## 启动与托盘

设置窗口提供两个整机选项：

- **开机启动**：默认开启。后台服务随 Windows 启动，不需要用户登录；用户登录后启动托盘。取消改为手动启动，不立即中断当前远控。
- **启动时最小化到托盘**：默认开启。普通启动和登录自启进入托盘；开始菜单的 LanDeskServer 入口打开设置窗口。

关闭窗口收进托盘，退出托盘只退出管理界面，后台服务继续运行。未登录时没有托盘，不影响后台服务。保存整机设置需要管理员授权；失败恢复旧设置并报错。

## 卸载

点击设置中的 **卸载服务**，或在 Windows 应用列表选择 **LanDeskServer → 卸载**。确认窗口可勾选 **同时移除 LanDesk 虚拟显示器**，默认不勾选。

卸载停止两个服务和管理界面，移除服务、开机入口、开始菜单入口、专用防火墙规则、程序与应用列表登记。保留密码配置、日志、托盘偏好、其他已有文件、显示配置和共享 Driver Store 包。勾选显示器移除也只移除 LanDesk 自建设备，复用的第三方设备保留。

卸载由同一个 EXE 的临时副本执行，避免运行中的程序阻碍删除。临时副本位于管理员专属目录，由 Windows 在下次自然重启时清理；不调用外部脚本、不主动重启。卸载失败明确报告可能已移除的组件，记录 `uninstall.log`。

只读检查可在终端运行，无需维护脚本：

```text
LanDeskServer.exe --version
LanDeskServer.exe --status
```

## 日志

设置窗口或托盘菜单点击 **导出日志**，授权后打开 `%ProgramData%\LanDeskServer\logs\exports`。发送最新的 `LanDeskServer-diagnostics-*.zip`，同时说明操作和发生时间。

- `install.log` / `uninstall.log`：原生安装、卸载步骤、Windows API 错误与回滚结果。
- `server.log` / `transport.log` / `worker.log`：服务、通信和桌面组件。
- `server.log` 记录工作进程所用的控制台 winlogon 进程与令牌；`worker.log` 记录自身的系统身份、会话、UIAccess、完整性级别、登录 SID，以及首次输入失败时的窗口站、桌面名称和该桌面是否正在接收输入。启动采集时记录屏幕编号、位置和主屏/虚拟屏标记；首帧及桌面变化时记录画面是否完全纯色，不记录像素或颜色。
- 0.1.10 记录桌面线程使用 `GENERIC_ALL`，以及首次输入失败时有无前台窗口、所属进程号。Ctrl+Alt+Delete 记录工作进程发起、SCM 接收、后台分发、策略值和 SendSAS 返回。API 无执行结果回执，日志返回不代表登录界面已经响应。
- 每份日志带 UTC 时间、进程号与版本，约 2 MB 后轮换，保留一份 `*.previous.log`。安装、卸载开始时也保留上一份记录。
- 界面与提权错误位于 `%LOCALAPPDATA%\LanDeskServer\logs\ui.log`，发生此类问题时同时发送它。管理员导出进程不读取用户目录日志。
- ZIP 只收集明确列出的后台日志及 Windows、服务、显示设备状态。`status.json` 增加控制台编号、会话连接状态/协议，以及 winlogon、LogonUI 和 LanDeskServer 的进程/令牌元数据；读不到的字段明确记录错误。不包含账号名、窗口标题、命令行、配置、密码、私钥、画面、输入或剪贴板内容。导出的 ZIP 自行保留或删除。

## 键鼠失败时保持查看

普通键鼠注入、文字输入、粘贴或切换显示前的按键释放失败时，显示持续警告并保留画面、屏幕切换和连接。同一次不可用期间只发一次警告，即使失败事件数量变化也不重新提示；网页也只在首次失败时释放按键，避免警告与释放相互触发。后续有效输入仍然尝试，实际输入成功后自动撤销警告。失败的按下事件会安排按键/鼠标释放重试，不重放此前失败的点击或文字。

0.1.9 改为复制目标控制台会话的 winlogon 系统令牌，用它启动桌面工作进程和发送安全注意序列；保留令牌原有登录组、完整性与 UIAccess 属性，不手动修改这些属性。仅选择 Windows 系统目录中的 winlogon，并验证其系统身份和会话。后台服务启用 SeDebugPrivilege 以打开目标系统进程；找不到或无法打开时明确报错，不退回旧的服务令牌路径。

0.1.10 对齐微软桌面线程示例，使用 `OpenInputDesktop(..., GENERIC_ALL)` 打开当前桌面，再通过 `SetThreadDesktop` 绑定采集/输入线程，替换此前只有读对象、写对象和切换桌面的访问掩码。这是向现有 ACL 请求访问权限，不修改系统 ACL，也不提升令牌完整性或 UIAccess；官方示例不是 SendInput 必须使用此掩码的证明，是否解决当前设备的拒绝输入仍待实测。Ctrl+Alt+Delete 成功提交后显示提示，失败单独报错，保持连接；“请求已提交”不表示 Windows 已执行。

这不代表已经解决 Winlogon 的拒绝访问，也不保证能自动取得系统输入权限。不强制切换 RDP 会话、不自动登录、不改桌面 ACL。网络断开、认证失败、非法协议消息和画面采集停止仍明确报错。0.1.7 实机日志证明 Win10 的驱动、服务、SSH 和 Winlogon 首帧已成功；键鼠拒绝的根因仍待进一步确认。

## 验收与限制

Win10 mini 不接实体屏幕：安装后通过 LanDeskClient 验证画面、键鼠、登录、锁屏/解锁、UAC、Ctrl+Alt+Delete；然后重启，直接连接，确认无需 RDP。再检查两个启动选项、托盘、退出管理界面后远控持续、更新与卸载。

当前 Windows 没有文件面板、图片剪贴板、音频、独立多用户桌面或 BIOS/BitLocker 启动前画面。GDI → JPEG 约最高 10 帧/秒，显示驱动 60 Hz 不等于网页 60 帧/秒。内置 SSH 只承载 LanDesk 桌面，不提供 Shell、SFTP 或任意转发。登录界面、UAC 和无头设备可用性必须在实机验证。

## 实现依据

原生绑定为 `windows-sys 0.61.2`；COM 使用 Microsoft `windows 0.61.3`，避免自行编写 COM ABI。ZIP 使用 Rust `zip 2.4.2`（Stored，不启用外部压缩程序），XML 校验使用 `quick-xml 0.38.4`。

- 服务：[CreateServiceW](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-createservicew)、[ChangeServiceConfig2W](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-changeserviceconfig2w)、[DeleteService](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-deleteservice)。
- 目录权限：为保留已有驱动目录中其他文件的 ACL，使用微软仍支持、但标记为旧 API 的 [SetFileSecurityW](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-setfilesecurityw)，只处理指定对象，不向子文件传播；失败恢复原权限。
- 防火墙：[INetFwPolicy2](https://learn.microsoft.com/en-us/windows/win32/api/netfw/nn-netfw-inetfwpolicy2)；快捷方式：[IShellLinkW](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-ishelllinkw)。
- 驱动目录签名：[WinVerifyTrust](https://learn.microsoft.com/en-us/windows/win32/api/wintrust/nf-wintrust-winverifytrust)；设备：[DiInstallDevice](https://learn.microsoft.com/en-us/windows/win32/api/newdev/nf-newdev-diinstalldevice)。不导入证书或开启测试签名。
- 提权：[ShellExecuteEx / runas](https://learn.microsoft.com/en-us/windows/win32/shell/launch)；卸载临时副本清理：[MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw)。
- 工作进程：参考微软 [PowerToys Mouse Without Borders 的目标会话 winlogon 令牌启动方式](https://github.com/microsoft/PowerToys/blob/main/src/modules/MouseWithoutBorders/App/Service/NativeMethods.cs)，使用 [DuplicateTokenEx](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-duplicatetokenex)、[CreateProcessAsUserW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw) 和 [活动控制台会话](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-wtsgetactiveconsolesessionid)。这是实现参考，不是微软对 LanDesk 登录控制能力的保证。
- 诊断：[WTSEnumerateSessionsW](https://learn.microsoft.com/en-us/windows/win32/api/wtsapi32/nf-wtsapi32-wtsenumeratesessionsw)、[GetTokenInformation](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-gettokeninformation)。
- 桌面与输入：[OpenInputDesktop](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-openinputdesktop)、[SetThreadDesktop](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setthreaddesktop)、[SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)、[SendSAS](https://learn.microsoft.com/en-us/windows/win32/api/sas/nf-sas-sendsas)。
- 桌面访问掩码：[微软桌面采集示例中的线程绑定](https://github.com/microsoft/Windows-classic-samples/blob/main/Samples/DXGIDesktopDuplication/cpp/DesktopDuplication.cpp)、[桌面访问权限映射](https://learn.microsoft.com/en-us/windows/win32/winstation/desktop-security-and-access-rights)。本版继续使用 GDI/JPEG 采集，未引入 DXGI 采集路径。

实际验证结果见测试包 `BUILD.txt`。编译、库测试和官方 API 支持不等于目标 Windows 设备验证通过。
