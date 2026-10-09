"""Fetch the pinned, signed x64 VDD package for embedding in LanDeskServer."""
import argparse
import hashlib
import io
from pathlib import Path
import tempfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
URL = "https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/25.7.23/VirtualDisplayDriver-x86.Driver.Only.zip"
SHA256 = "e24210692b442b39af763536330ce78b423f19342b7a7792c26de3944e418b3a"
FILES = {
    "MttVDD.inf": "550d211fe481e74dfe3f9d724ed78be48b3a9113405965d683d9373e8d672f5d",
    "MttVDD.dll": "c9ca837f57a98fbd43bc416a7f535a95843626e7759eaf85cf0cd7ce334dbb05",
    "mttvdd.cat": "08a0093fc9b2e32b287a6f8a77ca4de0a31830d29fc33d2b13a918dc859468f6",
}


def unpack(data, destination):
    if hashlib.sha256(data).hexdigest() != SHA256:
        raise ValueError("Virtual Display Driver archive SHA-256 mismatch")
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        payload = {name: archive.read("VirtualDisplayDriver/" + name) for name in FILES}
    for name, content in payload.items():
        if hashlib.sha256(content).hexdigest() != FILES[name]:
            raise ValueError(f"Virtual Display Driver file SHA-256 mismatch: {name}")
    destination.mkdir(parents=True, exist_ok=True)
    # Never extract arbitrary archive paths; only these three signed files are admitted.
    with tempfile.TemporaryDirectory(dir=destination) as temporary:
        for name, content in payload.items():
            source = Path(temporary) / name
            source.write_bytes(content)
        for name in payload:
            (Path(temporary) / name).replace(destination / name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, help="Use an already downloaded, hash-verified archive")
    args = parser.parse_args()
    if args.archive:
        data = args.archive.read_bytes()
    else:
        with urllib.request.urlopen(URL, timeout=60) as response:
            data = response.read()
    destination = ROOT / "vendor/virtual-display"
    unpack(data, destination)
    print(f"Verified Virtual Display Driver 25.7.23 (x64): {destination}")


if __name__ == "__main__":
    main()
