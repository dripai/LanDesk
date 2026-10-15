#!/usr/bin/env python3
"""Package the Windows service and its virtual display without installing them."""
import argparse
import hashlib
from pathlib import Path
import shutil
import struct


HASHES = {
    "MttVDD.dll": "c9ca837f57a98fbd43bc416a7f535a95843626e7759eaf85cf0cd7ce334dbb05",
    "MttVDD.inf": "550d211fe481e74dfe3f9d724ed78be48b3a9113405965d683d9373e8d672f5d",
    "mttvdd.cat": "08a0093fc9b2e32b287a6f8a77ca4de0a31830d29fc33d2b13a918dc859468f6",
}


def validate_executable(path):
    binary = path.read_bytes()
    if len(binary) < 64 or binary[:2] != b"MZ":
        raise ValueError(f"Expected a Windows x64 executable: {path}")
    offset = struct.unpack_from("<I", binary, 60)[0]
    if offset + 6 > len(binary) or binary[offset:offset + 4] != b"PE\0\0" or struct.unpack_from("<H", binary, offset + 4)[0] != 0x8664:
        raise ValueError(f"Expected an AMD64 PE executable: {path}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server-exe", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    driver = root / "vendor" / "virtual-display"
    for name, expected in HASHES.items():
        if hashlib.sha256((driver / name).read_bytes()).hexdigest() != expected:
            raise ValueError(f"Bundled driver checksum mismatch: {name}")
    validate_executable(args.server_exe)
    # Never replace an existing package or mix files from an earlier build.
    args.output.mkdir(parents=True, exist_ok=False)
    try:
        shutil.copy2(args.server_exe, args.output / "LanDeskServer.exe")
        shutil.copy2(driver / "LICENSE", args.output / "VIRTUAL-DISPLAY-LICENSE")
        shutil.copy2(root / "docs/architecture.md", args.output / "README.md")
    except BaseException:
        shutil.rmtree(args.output)
        raise
    print(args.output.resolve())


if __name__ == "__main__":
    main()
