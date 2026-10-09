import hashlib
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET
import zipfile

import prepare_virtual_display as vdd


class DriverPackageTests(unittest.TestCase):
    def archive(self, files):
        data = io.BytesIO()
        with zipfile.ZipFile(data, "w") as archive:
            for name, content in files.items():
                archive.writestr("VirtualDisplayDriver/" + name, content)
            archive.writestr("../../outside", b"untrusted")
        return data.getvalue()

    def test_verified_package_preserves_bytes_and_does_not_extract_other_paths(self):
        files = {"MttVDD.inf": "硬件描述\r\n".encode("utf-16"), "MttVDD.dll": b"MZ\x00\xff", "mttvdd.cat": b"signature"}
        data = self.archive(files)
        hashes = {name: hashlib.sha256(content).hexdigest() for name, content in files.items()}
        with tempfile.TemporaryDirectory() as temporary:
            dest = Path(temporary) / "driver"
            with patch.object(vdd, "SHA256", hashlib.sha256(data).hexdigest()), patch.object(vdd, "FILES", hashes):
                vdd.unpack(data, dest)
            self.assertEqual({p.name: p.read_bytes() for p in dest.iterdir()}, files)
            self.assertEqual(list(Path(temporary).iterdir()), [dest])

    def test_modified_archive_is_rejected_before_writing(self):
        with tempfile.TemporaryDirectory() as temporary:
            dest = Path(temporary)
            (dest / "MttVDD.dll").write_bytes(b"existing")
            with self.assertRaisesRegex(ValueError, "archive SHA-256"):
                vdd.unpack(b"corrupted archive", dest)
            self.assertEqual((dest / "MttVDD.dll").read_bytes(), b"existing")
            self.assertEqual(len(list(dest.iterdir())), 1)

    def test_missing_or_corrupted_member_does_not_publish_partial_package(self):
        for files, exception in [({"MttVDD.inf": b"wrong"}, KeyError),
                                 ({name: b"wrong" for name in vdd.FILES}, ValueError)]:
            data = self.archive(files)
            with self.subTest(files=list(files)), tempfile.TemporaryDirectory() as temporary:
                dest = Path(temporary)
                with patch.object(vdd, "SHA256", hashlib.sha256(data).hexdigest()), self.assertRaises(exception):
                    vdd.unpack(data, dest)
                self.assertEqual(list(dest.iterdir()), [])

    def test_build_revalidates_every_pinned_member(self):
        source = (vdd.ROOT / "build.rs").read_text(encoding="utf-8")
        for name, digest in vdd.FILES.items():
            self.assertIn(f'"{name}"', source)
            self.assertIn(f'"{digest}"', source)

    def test_new_install_has_exactly_one_1080p_sdr_display(self):
        root = ET.parse(vdd.ROOT / "third_party/virtual-display/settings.xml").getroot()
        self.assertEqual(root.findtext("monitors/count"), "1")
        modes = root.findall("resolutions/resolution")
        self.assertEqual(len(modes), 1)
        self.assertEqual([modes[0].findtext(k) for k in ("width", "height", "refresh_rate")], ["1920", "1080", "60"])
        self.assertEqual(root.findtext("options/SDR10bit"), "false")
        self.assertEqual(root.findtext("options/HDRPlus"), "false")


if __name__ == "__main__":
    unittest.main()
