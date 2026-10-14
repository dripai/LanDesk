# Windows 内置虚拟显示器

Windows 服务端附带的虚拟显示组件提供：一个 **1920×1080、60 Hz、SDR** 屏幕。安装包包含原始签名驱动和 `LanDeskDisplay.exe`，不需要另装 DevCon 或驱动控制面板。

固定设备实例为 `ROOT\LANDESKDISPLAY\0000`，使用持久化的 Windows 根枚举设备。重复安装不增加设备；关闭安装程序不移除设备。重启恢复及真实显示输出待实机验证。

## 安装与测试

完整解压 `LanDeskServer-windows-x64`，在该目录打开**管理员 Windows PowerShell 5.1**：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-VirtualDisplay.ps1
```

需要管理员权限安装显示驱动；命令中的 Bypass 仅用于运行这份本地脚本，不修改系统执行策略或驱动签名策略。脚本校验驱动 SHA-256 和目录签名，Windows 继续校验整个驱动包。校验失败明确报错，不导入证书或开启测试签名。

查看设备状态：

```powershell
.\LanDeskDisplay.exe --status
```

`installed` 表示设备注册，`started` 表示 Windows 设备节点已启动，`problem` 是设备问题码。它们不代表捕获到画面。安装返回 `reboot_required` 时需要重启后再检查。

Win11 办公机先确认显示设置中多出一个 1920×1080 屏幕，重复安装不增加屏幕。Win10 mini 主机确认无物理显示器时设备启动、重启后设备仍存在。完整远控请安装同包的 `Install-LanDeskServer.ps1`，使用和验证范围见[Windows 服务端说明](windows-server.md)。单独安装显示组件不会启动远控服务。

卸载 LanDesk 创建的设备：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-VirtualDisplay.ps1 -Action Uninstall
```

卸载保留 `C:\VirtualDisplayDriver` 配置及 Windows Driver Store 中的共享驱动包，不移除其他设备。安装失败会尝试移除本次新建设备；回滚失败明确报错并保留配置，供卸载重试。Windows 已暂存的驱动包可能保留。

## 约束

- 此包只支持 Windows x64，目标为 Win10/Win11。签名信任、系统版本、显卡及驱动加载是否可用仍须目标机验证。
- 驱动采用全局配置路径。检测到其他 MttVDD 设备、已有第三方配置目录或全局注册表配置时明确拒绝，不覆盖它们。
- 单独使用本显示组件不改动账号、登录、RDP、SSH 或桌面会话逻辑。完整服务包另有开机服务、工作进程和访问密钥。
- 上游驱动版本、哈希和授权见安装包 `driver/README.md`、`driver/LICENSE`。上游列出 Visual C++ 运行库依赖；加载失败时先查设备问题码及 `C:\Windows\INF\setupapi.dev.log`，不把安装返回成功视为驱动运行成功。

## 构建与验证范围

在 Windows x64 仓库目录执行：

```powershell
cargo build --locked --release --bin LanDeskDisplay --bin LanDeskServer
python scripts/bundle_windows_display.py --exe target/release/LanDeskDisplay.exe --server-exe target/release/LanDeskServer.exe --output target/LanDeskServer-windows-x64
```

若配置了自定义 Cargo target-dir，替换命令中的可执行文件路径。输出目录必须不存在，避免混入旧包。

当前已验证驱动文件与上游 ZIP 一致，Windows GNU 目标 `cargo check` / Clippy、37 项本机公共库测试、Rust 格式及 diff 检查通过；新增文本 UTF-8、Python 打包脚本语法及 XML 单屏配置检查通过。Windows 实机安装、卸载、重启持久性和无头输出尚未验证。已添加 Windows CI 构建、PowerShell 语法/签名检查及只读状态检查；工作流尚未运行；本机已交叉构建 Windows x64 服务端和显示组件 EXE，尚未在 Windows 执行。CI 不安装显示驱动。

官方依据：[Microsoft IDD 模型](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/indirect-display-driver-model-overview)、[注册根枚举设备](https://learn.microsoft.com/en-us/previous-versions/windows/drivers/install/dif-registerdevice)、[DiInstallDevice](https://learn.microsoft.com/en-us/windows/win32/api/newdev/nf-newdev-diinstalldevice)、[限定单个 INF](https://learn.microsoft.com/en-us/windows/win32/api/setupapi/ns-setupapi-sp_devinstall_params_w)。驱动和配置依据：[上游 25.7.23](https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/tag/25.7.23)。
