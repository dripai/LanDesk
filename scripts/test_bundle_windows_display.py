"""Release guard: a console-subsystem server must never be packaged again."""
import struct
import tempfile
import unittest
from pathlib import Path
from bundle_windows_display import validate_executable


class ExecutableTests(unittest.TestCase):
    def image(self, subsystem=2):
        data = bytearray(512)
        data[:2] = b"MZ"
        struct.pack_into("<I", data, 60, 128)
        data[128:132] = b"PE\0\0"
        struct.pack_into("<H", data, 132, 0x8664)
        struct.pack_into("<H", data, 148, 240)
        struct.pack_into("<H", data, 152, 0x20b)
        struct.pack_into("<H", data, 220, subsystem)
        return data

    def validate(self, data):
        with tempfile.TemporaryDirectory() as directory:
            exe = Path(directory) / "LanDeskServer.exe"
            exe.write_bytes(data)
            validate_executable(exe)

    def test_gui_server_accepted(self):
        self.validate(self.image())

    def test_console_server_rejected(self):
        with self.assertRaisesRegex(ValueError, "GUI subsystem"):
            self.validate(self.image(3))

    def test_truncated_and_wrong_platform_images_rejected(self):
        for length in (0, 63, 133, 220, 391):
            with self.subTest(length=length), self.assertRaises(ValueError):
                self.validate(self.image()[:length])
        for offset, value in ((132, 0x14c), (148, 69), (152, 0x10b)):
            data = self.image()
            struct.pack_into("<H", data, offset, value)
            with self.subTest(offset=offset), self.assertRaises(ValueError):
                self.validate(data)


if __name__ == "__main__":
    unittest.main()
