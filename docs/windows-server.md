# Windows 服务端 0.1.2 测试包

Windows 被控端现在自带 SSH 通信、虚拟显示器、桌面采集和键鼠控制，不需要安装系统 OpenSSH，也不需要先连接 RDP。用现有 LanDeskClient 的桌面模式连接，只输入一次 LanDesk 访问密码。

新增设置窗口和托盘，提供“开机启动”和“启动时最小化到托盘”。当前环境是 Mac：协议测试和 Windows 交叉构建可以在本机完成，Windows 安装、托盘、无头画面、登录/锁屏/UAC 及真实键鼠仍待实机验收。

## 安装与连接

目标为 Windows 10 / Windows 11 x64。完整解压测试包，在该目录打开**管理员 Windows PowerShell 5.1**：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-LanDeskServer.ps1
```

安装时输入 LanDesk 访问密码；留空会生成密码并仅显示一次，需自行保存。密码以 Argon2id 散列保存，无法通过状态命令恢复明文。它与 Windows 登录密码无关。

安装会配置两个后台服务、虚拟显示器和 TCP 2222 的程序专用入站防火墙规则，适用于所有网络配置文件；不会修改系统 SSH、账号、RDP 或自动登录。可用 `-Port 2223` 在首次安装时指定其他端口。驱动提示需要重启时先重启。已有版本先执行 `-Action Uninstall` 再安装；新版本的密码和设备身份会保留，原版本的 Web 密钥不再使用。

在办公机 LanDeskClient 填写：

| 设置 | 值 |
|---|---|
| 模式 | 桌面 |
| 主机 | mini 的 IP |
| SSH 端口 | 2222，或安装时指定的端口 |
| 用户名 | `landesk`，产品固定名称，无需创建 Windows 用户 |
| 认证 | 密码 |
| 密码 | 安装时设置或生成的 LanDesk 密码 |

网页自动连接，不再输入 Web 密钥。当前 Windows 界面显示什么，网页就显示什么；未登录时在 Windows 自己的登录界面操作。服务不自动登录、不接管 RDP 会话，也不切换系统用户。

客户端首次连接沿用现有主机密钥记录机制，之后设备密钥变化会拒绝连接。状态命令可显示 SSH 指纹，方便核对。跨公网仍需可达的地址/端口或已有组网，本版本不含 NAT 穿透和中继。

## 启动与托盘

安装完成会打开设置窗口；以后从开始菜单 **LanDeskServer** 或托盘“设置”打开。

- **开机启动**：默认开启。后台远控服务随 Windows 启动，无需用户登录；用户登录后启动托盘。取消后改为手动启动，不立即中断当前服务或远控连接。
- **启动时最小化到托盘**：默认开启。普通启动和登录自启时不弹设置窗口；从开始菜单“设置”打开时仍显示窗口。关闭或最小化窗口会收进托盘。

保存系统启动设置时，Windows 可能弹出管理员授权。设置保存失败会尝试恢复旧值，并明确报告错误。两个选项为整机设置。

退出托盘只关闭管理界面，后台远控继续运行。若关闭了开机启动，重启后手动打开已安装的服务端会启动后台服务（需要管理员授权）。托盘属于登录后的桌面；未登录时没有托盘，也不影响后台服务。

## 建议验收顺序

1. 首次连接检查画面、鼠标移动/左右键/滚轮、中文输入、Ctrl+A/C/V、拖动和全屏。Windows 文本粘贴通过 Unicode 输入，目标应用需支持此输入方式。
2. 点击工具条 Ctrl+Alt+Delete，测试 Windows 登录、锁屏/解锁、UAC 提示及注销。只在 Windows 界面输入 Windows 密码。
3. mini 不接实体显示器，重启后直接连接，无需先用 RDP。服务采集物理控制台，网页与已有 RDP 会话可能是不同桌面。
4. 关闭网页再连接，确认按键没有卡住；切换用户使控制台会话编号变化时，工作进程会重建，网页需重新连接。
5. 勾选/取消两个启动选项，保存后重启验证：后台服务是否自启、登录后的窗口是否进入托盘。关闭窗口、退出托盘时确认远控继续工作。

Windows 桌面服务使用受 ACL 保护的本机命名管道，不再监听内部 TCP 端口。默认配置不会与同机 LanDeskClient 的 17890 网关冲突；网页仍由客户端打开。

当前 Windows 不提供文件面板、图片剪贴板、音频、独立多用户桌面或 BIOS/BitLocker 启动前画面。GDI → JPEG 采集最高约 10 帧/秒，虚拟显示驱动 60 Hz 不等于网页 60 帧/秒。内置 SSH 仅承载 LanDesk 桌面，不提供 Shell、SFTP 或任意端口转发；Linux 终端模式仍连接 Linux 原有 SSH 服务。

## 状态、停止与卸载

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-LanDeskServer.ps1 -Action Status
# 停止后台服务，不只是退出托盘：
Stop-Service LanDeskServer
Stop-Service LanDeskTransport
# 移除两个服务、托盘自启、开始菜单入口和专用防火墙规则：
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-LanDeskServer.ps1 -Action Uninstall
# 另行移除虚拟显示设备：
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-VirtualDisplay.ps1 -Action Uninstall
```

卸载保留访问密码散列、设备身份密钥、日志、托盘偏好和虚拟显示器；不修改系统 OpenSSH。重装继续使用原 SSH 端口和凭据。

程序位于 `%ProgramFiles%\LanDeskServer`；配置为 `%ProgramData%\LanDeskServer\transport\config.json`，仅 SYSTEM、管理员及通信服务账号可读，普通用户不能读取。日志 `server.log` / `transport.log` 各最多 2 MB，不记录输入或密码。驱动排查见 `C:\Windows\INF\setupapi.dev.log` 和 `LanDeskDisplay.exe --status`。

安装失败会尝试回滚此次服务、启动入口、规则、策略和新建设备；回滚失败明确报告。配置、日志和 Windows 已暂存的驱动文件可能保留。为了支持 Ctrl+Alt+Delete，若 `SoftwareSASGeneration` 不存在，安装设置为允许服务调用的 `1`，卸载移除自己创建且未被修改的值；已有组织禁止策略不会被覆盖。

## 实现与验证

- 通信使用项目锁定的 [russh 0.64.1](https://github.com/Eugeny/russh)，密码使用 [Argon2 0.6.0](https://docs.rs/argon2/0.6.0/argon2/)。`LanDeskTransport` 以 [LocalService](https://learn.microsoft.com/en-us/windows/win32/services/localservice-account) 运行，只转发桌面 HTTP/WebSocket。内部连接采用 [Windows 命名管道权限](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)，拒绝远程管道客户端，并先验证管道所有者为 SYSTEM 再注入随机凭据。网页、URL、浏览器存储和客户端均不接触该凭据。
- `LanDeskServer` 以 SYSTEM 运行，通过 [CreateProcessAsUserW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw) 在 [活动控制台会话](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-wtsgetactiveconsolesessionid) 启动采集/输入进程；SCM 依赖关系负责先启动通信服务。两个服务和托盘使用同一个 EXE 的不同启动模式。
- 采集为 [GDI](https://learn.microsoft.com/en-us/windows/win32/gdi/capturing-an-image)，采集和输入线程通过 [OpenInputDesktop](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-openinputdesktop) / [SetThreadDesktop](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setthreaddesktop) 跟随当前桌面，使用 [SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput) / [SendSAS](https://learn.microsoft.com/en-us/windows/win32/api/sas/nf-sas-sendsas)。不实现 Windows 登录逻辑。
- 设置窗口使用 Win32 和 [Shell_NotifyIcon](https://learn.microsoft.com/en-us/windows/win32/shell/notification-area)，开机选项调用 [ChangeServiceConfig](https://learn.microsoft.com/en-us/windows/win32/services/changing-a-service-configuration)，登录托盘通过 HKLM Run 启动。Windows API 绑定为 `windows-sys 0.61.2`。
- 本机测试包通过官方 LLVM-MinGW 和 Rust `x86_64-pc-windows-gnullvm` 交叉构建；加密依赖采用 [aws-lc-sys 官方预编译 NASM 对象](https://github.com/aws/aws-lc-rs/tree/main/aws-lc-sys#use-of-prebuilt-nasm-objects)，未关闭加密或汇编加速。Windows CI 使用 MSVC，本轮尚未运行。

2026-10-14 本机验证：40 项 Rust 库测试、3 项平台契约测试通过，包括真实 SSH 密码认证、HTTP/WebSocket 转发、转发目标拒绝、断线释放、身份持久化和内部 HTTP 认证。网页回归、静态检查、格式、UTF-8、安装脚本语法和 Windows 构建结果见测试包 BUILD.txt。这些检查不等于 Windows 服务安装、托盘、驱动、无头桌面及登录界面实机验证。
