# 内置虚拟显示驱动

Windows x64 服务端使用 [Virtual Display Driver 25.7.23 发布包](https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/tag/25.7.23)中的签名驱动，不安装上游控制面板或音频驱动。许可证为 [MIT](LICENSE)。驱动 INF 版本为 `12/24/2024,11.30.4.434`；文件名虽为 `VirtualDisplayDriver-x86.Driver.Only.zip`，实际 DLL 的 PE 架构为 AMD64。

INF 声明 UMDF 2.25 / IddCx 1.2。微软的 [UMDF 版本表](https://learn.microsoft.com/en-us/windows-hardware/drivers/wdf/umdf-version-history)将 UMDF 2.25 对应到 Windows 10 1803 起；这是运行时要求，不能据此推定某台机器的显卡和驱动已经通过验证。

构建前运行 `python scripts/prepare_virtual_display.py`。下载地址及 SHA-256 固定在脚本中；构建脚本再次校验每个文件并嵌入 EXE。INF 保持原始 UTF-16 字节，不能改名、重编码或修改内容后继续使用原签名。Windows CI 使用 Authenticode 和 Windows SDK SignTool 验证 catalog 签名与 INF/DLL 成员关系。

安装器在已有管理员授权过程中将文件放入 `%ProgramFiles%\LanDeskServer\virtual-display-25.7.23`，通过 [SetupCopyOEMInfW](https://learn.microsoft.com/en-us/windows/win32/api/setupapi/nf-setupapi-setupcopyoeminfw) 暂存驱动，不替换其他设备正在使用的驱动。系统仍执行驱动签名校验；LanDesk 不开启测试签名、不关闭签名检查、不导入根证书。

上游驱动从 `HKLM\SOFTWARE\MikeTheTech\VirtualDisplayDriver\VDDPATH` 读取配置目录，缺省为 `C:\VirtualDisplayDriver`。首次安装使用本目录的单屏 1920×1080@60Hz SDR 配置；已有注册表路径或默认位置配置时保留它，实际模式由原配置决定。该路径是上游全局设置，不是每个虚拟显示器独立的设置。模式采集仍支持 LanDesk 原有等比例缩小。

DXGI 枚举不到任何已连接输出时，SYSTEM 桌面进程通过 [SwDeviceCreate](https://learn.microsoft.com/en-us/windows/win32/api/swdevice/nf-swdevice-swdevicecreate) 创建 `LanDeskVirtualDisplay` 软件设备，硬件 ID 为 INF 已签名的 `MttVDD`。设备枚举成功不等于画面就绪：随后重新枚举 DXGI 输出，真正创建 Desktop Duplication 后才通知网页连接成功。设备在锁屏/UAC 切换期间保留，远控会话结束时随句柄关闭移除；物理屏存在时不创建设备。

卸载桌面服务会结束桌面进程、释放虚拟设备。驱动缓存、配置和版本目录保留，不删除其他软件可能共享的驱动包。

## 实机验收（待完成）

- Windows 10 x64 无屏 mini 主机：启动新版完成管理员安装；连接后确认 1920×1080 画面及键鼠操作。
- 锁屏、输入密码、解锁、UAC 切换后画面继续；重新连接可再次创建设备。
- 断开后设备管理器不残留在线的 LanDesk 虚拟设备；已有物理屏、上游 VDD 配置和设备不被删除或覆盖。
- 驱动安装失败时显示系统错误及 `setupapi.dev.log` 路径；已创建设备但未提供输出时显示设备问题代码。

本机 Mac 的测试及 Windows 交叉类型检查不能替代以上验收，也不能证明所有 Windows 10 显卡、系统策略均接受该驱动。
