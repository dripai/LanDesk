#!/usr/bin/env python3
"""Build the macOS application without changing system configuration."""
import json
import plistlib
import shutil
import subprocess
from pathlib import Path

root = Path(__file__).resolve().parent.parent
subprocess.run(["cargo", "build", "--release", "--locked"], cwd=root, check=True)
metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=root, text=True, encoding="utf-8"))
target = Path(metadata["target_directory"]) / "release"
bundle = target / "bundle" / "macos" / "LanDeskServer.app"
macos = bundle / "Contents" / "MacOS"
macos.mkdir(parents=True, exist_ok=True)
shutil.copy2(target / "LanDeskServer", macos / "LanDeskServer")
info = {
    "CFBundleName": "LanDeskServer",
    "CFBundleDisplayName": "LanDeskServer",
    "CFBundleIdentifier": "local.aiwork.LanDesk",
    "CFBundleExecutable": "LanDeskServer",
    "CFBundleVersion": "2",
    "CFBundleShortVersionString": "0.1.1",
    "CFBundlePackageType": "APPL",
    "LSMinimumSystemVersion": "14.2",
    "NSHighResolutionCapable": True,
    "NSScreenCaptureUsageDescription": "通过 LanDeskClient 的加密连接提供 Mac 桌面画面。",
}
with (bundle / "Contents" / "Info.plist").open("wb") as file:
    plistlib.dump(info, file)
subprocess.run(["codesign", "--force", "--sign", "-", "--identifier", "local.aiwork.LanDesk", str(bundle)], check=True)
subprocess.run(["codesign", "--verify", "--strict", str(bundle)], check=True)
print(bundle)
