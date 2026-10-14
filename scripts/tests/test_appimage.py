import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import appimage


def runtime_header(machine=62):
    header = bytearray(64)
    header[:11] = b"\x7fELF\x02\x01\x01\x00AI\x02"
    struct.pack_into("<H", header, 18, machine)
    struct.pack_into("<Q", header, 40, 64)
    struct.pack_into("<HH", header, 58, 64, 1)
    section = bytearray(64)
    # NOBITS may extend far past the file; it must not move the payload offset.
    struct.pack_into("<I", section, 4, 8)
    struct.pack_into("<QQ", section, 24, 0, 100_000_000)
    return header + section


class AppImageInspectionTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.image = self.root / "synthetic.AppImage"

    def test_offset_reads_both_linux_architectures_without_executing_them(self):
        for machine in [62, 183]:
            self.image.write_bytes(runtime_header(machine) + b"hsqs")
            self.assertEqual(appimage.squashfs_offset(self.image), 128)

    def test_invalid_or_truncated_images_fail_closed(self):
        for content in [b"not an image", runtime_header()[:80],
                        runtime_header() + b"wrong payload"]:
            self.image.write_bytes(content)
            with self.subTest(content=content[:11]), self.assertRaises(ValueError):
                appimage.squashfs_offset(self.image)

    def test_downloaded_tool_must_match_pinned_digest(self):
        self.image.write_bytes(b"changed tool")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            appimage.verify_tool(self.image, "0" * 64)

    @unittest.skipUnless(shutil.which("mksquashfs") and shutil.which("unsquashfs"),
                         "requires squashfs-tools")
    def test_reads_metadata_from_real_squashfs_after_foreign_runtime(self):
        source = self.root / "AppDir"
        source.mkdir()
        info = {"version": "0.1.0", "target": "aarch64-unknown-linux-gnu", "dirty": False}
        (source / "BUILD-INFO.json").write_text(json.dumps(info), encoding="utf-8")
        filesystem = self.root / "filesystem"
        subprocess.run(["mksquashfs", str(source), str(filesystem), "-noappend", "-quiet",
                        "-processors", "1"], check=True, capture_output=True)
        self.image.write_bytes(runtime_header(183) + filesystem.read_bytes())
        self.assertEqual(appimage.read_build_info(self.image), info)

    @unittest.skipUnless(Path("/bin/sh").exists(), "requires a POSIX shell")
    def test_apprun_preserves_arguments_working_directory_and_runtime_paths(self):
        appdir = self.root / "App Dir"
        executable = appdir / "usr/bin/switchx"
        executable.parent.mkdir(parents=True)
        executable.write_text('#!/bin/sh\nprintf "%s\\n" "$PWD" "$APPIMAGE" "$APPDIR" "$@"\n',
                              encoding="utf-8")
        executable.chmod(0o755)
        apprun = appdir / "AppRun"
        shutil.copy2(appimage.ROOT / "packaging/linux/AppRun", apprun)
        arguments = ["credential", "spaces and ' quotes", str(self.root / "data")]
        environment = {**os.environ, "APPDIR": str(appdir), "APPIMAGE": "/fixed/SwitchX.AppImage"}
        result = subprocess.check_output(["/bin/sh", str(apprun), *arguments], cwd=self.root,
                                         env=environment, text=True)
        self.assertEqual(result.splitlines(), [str(self.root), environment["APPIMAGE"],
                                               str(appdir), *arguments])
