# LanDesk 应用图标

Client 和 Server 共用现有的蓝底白色 L。唯一图形定义在 `scripts/generate_icons.py`；运行 `python3 scripts/generate_icons.py`（需要 Pillow）重新生成全部资源。生成文件纳入 Git，正常编译不需要 Python 或 Pillow。

- `tray.rgba`：32×32 RGBA，保留原托盘像素，两端直接嵌入。
- `LanDesk.ico`：16、20、24、32、40、48、64、128、256 像素，嵌入 Windows EXE 的图标资源 ID 1。GPUI 0.3.8 自动将它用于窗口类图标，系统可用于标题栏、任务栏和 EXE/快捷方式；原生安装窗口也加载同一资源。
- `LanDesk.icns`：Mac 多分辨率图标。两个打包脚本写入 `Contents/Resources` 并设置 `CFBundleIconFile`，用于 Finder、应用程序和 Dock。Mac 外围增加透明留白和圆角，L 图形与颜色一致。
- `LanDesk.svg`、`LanDesk.png`：矢量标识与 Mac 图标预览。

图标编译缺少资源编译器或资源文件时构建失败，不静默输出无图标的包。图标资源不包含 manifest，保留 GPUI 自带的 DPI manifest。

依据：[Windows ICON 资源](https://learn.microsoft.com/en-us/windows/win32/menurc/icon-resource)、[Apple CFBundleIconFile](https://developer.apple.com/library/archive/documentation/General/Reference/InfoPlistKeyReference/Articles/CoreFoundationKeys.html#//apple_ref/doc/uid/TP40009249-SW10)。
