# Windows 内置虚拟显示器

直接双击 `LanDeskServer.exe`，原生安装流程会一起处理显示器。驱动、配置模板和授权文件都在 EXE 内，不需要脚本或另外下载组件。完整操作见[Windows 服务端说明](windows-server.md)。

新配置提供一个 1920×1080、60 Hz、SDR 显示器，自建设备为 `ROOT\LANDESKDISPLAY\0000`。已有运行中的 MttVDD 直接复用，保留其配置、数量、分辨率和归属。只有全局配置而没有设备时保留原 VDDPATH/XML，在原配置下安装自建设备；没有 VDDPATH 使用上游默认 `C:\VirtualDisplayDriver`。已有设备未启动或配置缺失/无效会明确报错。

驱动采用上游 25.7.23 原始 DLL、INF 和 CAT。安装时校验 SHA-256、调用 WinVerifyTrust 检查目录签名，再由 Windows DiInstallDevice 验证并安装驱动；不导入证书或开启测试签名。需要管理员权限，Windows 提示需要重启时先重启。

状态命令：

```text
LanDeskServer.exe --status
```

`installed` / `started` 表示 LanDesk 自建设备的注册/启动状态；`devices` 列出系统当前的 MttVDD 实例、硬件 ID、启动状态与问题码；`available` 表示至少一个设备在运行。这些字段不证明已经捕获到画面。

卸载时可勾选“同时移除 LanDesk 虚拟显示器”，仅移除自建设备，保留其他设备、配置和共享 Driver Store 包。安装回滚只撤销本次创建的设备，不撤销复用设备。

## 构建与验证

```text
cargo build --locked --release --bin LanDeskServer --bin LanDeskDisplay
python scripts/bundle_windows_display.py --server-exe target/release/LanDeskServer.exe --output target/LanDeskServer-windows-x64
```

发布包只需要服务端 EXE 和说明文件，输出目录必须不存在。`LanDeskDisplay.exe` 仍可作为开发时的设备诊断工具构建，不参与服务端安装流程。

当前本机为 Mac，已完成 Windows 交叉构建。用户的 Win10 0.1.7/0.1.8 日志确认 MttVDD 设备启动且问题码为 0；0.1.10 用户在无实体屏幕、重启后未连接 RDP 的 mini 上通过 Web 登录并操作主屏，登录后的虚拟副屏也显示桌面和任务栏。选副屏不自动搬动应用窗口或改变主屏。尚未验证卸载虚拟驱动后能否继续无头输出；Win11、UAC、锁屏解锁等未全面验收。CI 增加原生服务、文件/注册表恢复、防火墙、快捷方式和目录签名测试，不安装显示设备；本轮未运行。

官方依据：[Microsoft IDD](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/indirect-display-driver-model-overview)、[根枚举设备注册](https://learn.microsoft.com/en-us/previous-versions/windows/drivers/install/dif-registerdevice)、[DiInstallDevice](https://learn.microsoft.com/en-us/windows/win32/api/newdev/nf-newdev-diinstalldevice)、[单 INF 选择](https://learn.microsoft.com/en-us/windows/win32/api/setupapi/ns-setupapi-sp_devinstall_params_w)、[上游 25.7.23](https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/tag/25.7.23)、[该版本 VDDPATH/loadSettings](https://github.com/VirtualDrivers/Virtual-Display-Driver/blob/25.7.23/Virtual%20Display%20Driver%20(HDR)/MttVDD/Driver.cpp)。
