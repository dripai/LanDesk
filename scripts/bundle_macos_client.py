#!/usr/bin/env python3
"""Build a local, ad-hoc signed macOS client application."""
import argparse
import json
import plistlib
import shutil
import subprocess
import sys
from pathlib import Path


def main():
    if sys.platform != "darwin":
        raise SystemExit("LanDeskClient.app must be built on macOS")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--debug", action="store_true", help="build a development app")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    manifest = root / "client" / "Cargo.toml"
    command = ["cargo", "build", "--manifest-path", str(manifest), "--locked", "--bin", "LanDeskClient"]
    if not args.debug:
        command.append("--release")
    subprocess.run(command, cwd=root, check=True)
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--manifest-path", str(manifest), "--locked", "--no-deps", "--format-version", "1"],
        cwd=root, text=True, encoding="utf-8",
    ))
    version = next(p["version"] for p in metadata["packages"] if Path(p["manifest_path"]) == manifest)
    profile = "debug" if args.debug else "release"
    binary = Path(metadata["target_directory"]) / profile / "LanDeskClient"
    bundle = root / "client" / "target" / "bundle" / "macos" / "LanDeskClient.app"
    executable = bundle / "Contents" / "MacOS" / "LanDeskClient"
    executable.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary, executable)
    info = {
        "CFBundleName": "LanDeskClient",
        "CFBundleDisplayName": "LanDeskClient",
        "CFBundleIdentifier": "local.aiwork.LanDeskClient",
        "CFBundleExecutable": "LanDeskClient",
        "CFBundleVersion": version,
        "CFBundleShortVersionString": version,
        "CFBundlePackageType": "APPL",
        "NSHighResolutionCapable": True,
    }
    with (bundle / "Contents" / "Info.plist").open("wb") as file:
        plistlib.dump(info, file)
    subprocess.run(["codesign", "--force", "--sign", "-", "--identifier", info["CFBundleIdentifier"], str(bundle)], check=True)
    subprocess.run(["codesign", "--verify", "--strict", str(bundle)], check=True)
    print(bundle)


if __name__ == "__main__":
    main()
