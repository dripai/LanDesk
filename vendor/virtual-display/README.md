# Windows 虚拟显示驱动

内置 [VirtualDrivers/Virtual-Display-Driver 25.7.23](https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/tag/25.7.23) 的原始驱动。下载资产名为 `VirtualDisplayDriver-x86.Driver.Only.zip`，但其中 INF 指定 **NTamd64**，DLL 的 PE Machine 为 **AMD64**；此包仅用于 Windows x64。

`MttVDD.inf`、`MttVDD.dll`、`mttvdd.cat` 与上述官方 ZIP 中的文件逐字节一致。INF 版本为 `12/24/2024,11.30.4.434`；禁止编辑 INF/DLL/CAT，否则原始签名失效。分发须附带本目录 MIT LICENSE。

| 文件 | SHA-256 |
| --- | --- |
| MttVDD.inf | 550d211fe481e74dfe3f9d724ed78be48b3a9113405965d683d9373e8d672f5d |
| MttVDD.dll | c9ca837f57a98fbd43bc416a7f535a95843626e7759eaf85cf0cd7ce334dbb05 |
| mttvdd.cat | 08a0093fc9b2e32b287a6f8a77ca4de0a31830d29fc33d2b13a918dc859468f6 |

`vdd_settings.xml` 是 LanDesk 的配置：一个 1920×1080、60 Hz、SDR 屏幕，关闭硬件光标和日志。字段及默认配置路径 `C:\VirtualDisplayDriver` 已核对该 tag 的 [Driver.cpp](https://github.com/VirtualDrivers/Virtual-Display-Driver/blob/25.7.23/Virtual%20Display%20Driver%20%28HDR%29/MttVDD/Driver.cpp) 与配置模板。

安装前在目标 Windows 检查目录签名信任，安装由系统校验驱动包；不导入证书、不启用测试签名、不修改 Secure Boot。签名、Win10/Win11 加载与无头屏幕输出仍需实机验证。
