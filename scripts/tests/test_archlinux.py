import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import archlinux
import release


class ArchSourceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.root = self.directory / "checkout with spaces"
        self.root.mkdir()
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        (self.root / "packaging/arch").mkdir(parents=True)
        shutil.copy2(SCRIPTS.parent / "packaging/arch/PKGBUILD.in", self.root / "packaging/arch")
        (self.root / "Cargo.toml").write_text(
            '[package]\nname = "switchx"\nversion.workspace = true\n'
            '[workspace]\nmembers = []\n[workspace.package]\nversion = "0.1.0"\n',
            encoding="utf-8")
        (self.root / "Cargo.lock").write_text(
            'version = 4\n[[package]]\nname = "switchx"\nversion = "0.1.0"\n', encoding="utf-8")
        (self.root / "CHANGELOG.md").write_text('## [Unreleased]\n\n- Synthetic.\n', encoding="utf-8")
        (self.root / "icon.png").write_bytes(b"\x89PNG\r\n\x1a\nsynthetic")
        self.git("add", ".")
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "commit", "-qm", "synthetic")
        self.destination = self.directory / "output"

    def git(self, *arguments):
        return archlinux.git(self.root, *arguments)

    def test_snapshot_includes_hydrated_working_files_and_excludes_untracked_data(self):
        (self.root / "icon.png").write_bytes(b"\x89PNG\r\n\x1a\nmodified")
        (self.root / "private.json").write_text('synthetic private data', encoding="utf-8")
        archlinux.prepare(self.root, self.destination)
        with tarfile.open(self.destination / "switchx-0.1.0.tar.gz") as archive:
            self.assertEqual(archive.extractfile("switchx-0.1.0/icon.png").read(),
                             b"\x89PNG\r\n\x1a\nmodified")
            self.assertFalse(any("private.json" in name or "/.git/" in name
                                 for name in archive.getnames()))
            info = json.load(archive.extractfile("switchx-0.1.0/BUILD-INFO.json"))
            self.assertTrue(info["dirty"])
            self.assertEqual(info["commit"], self.git("rev-parse", "HEAD").decode().strip())
        recipe = (self.destination / "PKGBUILD").read_text(encoding="utf-8")
        self.assertIn(archlinux.digest(self.destination / "switchx-0.1.0.tar.gz"), recipe)
        self.assertNotIn("@VERSION@", recipe)

    def test_lfs_pointer_is_rejected_before_makepkg(self):
        (self.root / "icon.png").write_text(
            'version https://git-lfs.github.com/spec/v1\noid sha256:synthetic\n', encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "Git LFS pointer"):
            archlinux.prepare(self.root, self.destination)

    def test_modified_source_keeps_its_timestamp_for_cargo_cache_invalidation(self):
        modified = int(self.git("show", "-s", "--format=%ct", "HEAD")) + 10
        os.utime(self.root / "icon.png", (modified, modified))
        archlinux.prepare(self.root, self.destination)
        with tarfile.open(self.destination / "switchx-0.1.0.tar.gz") as archive:
            self.assertEqual(archive.getmember("switchx-0.1.0/icon.png").mtime, modified)

    def test_source_symlink_cannot_read_outside_checkout(self):
        outside = self.directory / "outside"
        outside.write_text("synthetic private data", encoding="utf-8")
        (self.root / "icon.png").unlink()
        (self.root / "icon.png").symlink_to(outside)
        with self.assertRaisesRegex(ValueError, "symlinks"):
            archlinux.prepare(self.root, self.destination)

    def test_prepared_release_drives_recipe_without_another_version_edit(self):
        release.prepare(self.root, "0.2.0-rc.1", "2026-10-14")
        self.assertEqual(archlinux.prepare(self.root, self.destination), "0.2.0-rc.1")
        recipe = (self.destination / "PKGBUILD").read_text(encoding="utf-8")
        self.assertIn("pkgver=0.2.0pre.rc.1\n", recipe)
        self.assertIn("_version='0.2.0-rc.1'", recipe)


@unittest.skipUnless(shutil.which("vercmp"), "requires pacman vercmp")
class ArchVersionTests(unittest.TestCase):
    def test_prerelease_upgrades_to_final_with_pacman_ordering(self):
        for version in ["0.2.0-alpha.1", "0.2.0-rc.1", "0.2.0-1", "0.2.0-beta-test"]:
            with self.subTest(version=version):
                result = subprocess.check_output(
                    ["vercmp", archlinux.package_version(version), "0.2.0"], text=True)
                self.assertLess(int(result), 0)


@unittest.skipUnless(shutil.which("bsdtar"), "requires libarchive bsdtar")
class ArchInspectionTests(unittest.TestCase):
    def test_reads_real_zstd_package_and_rejects_inconsistent_pacman_metadata(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            content = root / "content"
            metadata = content / archlinux.METADATA
            metadata.parent.mkdir(parents=True)
            info = {"version": "0.1.0", "target": archlinux.TARGET, "commit": "synthetic",
                    "dirty": False, "format": "archlinux", "pkgver": "0.1.0", "pkgrel": 1}
            metadata.write_text(json.dumps(info), encoding="utf-8")
            package = root / archlinux.package_name("0.1.0")
            for arch, version in [("x86_64", "0.1.0-1"), ("aarch64", "0.1.0-1"),
                                  ("x86_64", "0.1.1-1")]:
                (content / ".PKGINFO").write_text(
                    f"pkgname = switchx\npkgver = {version}\narch = {arch}\n", encoding="utf-8")
                subprocess.run(["bsdtar", "--zstd", "-cf", str(package), "-C", str(content),
                                ".PKGINFO", "usr"], check=True)
                if arch == "x86_64" and version == "0.1.0-1":
                    self.assertEqual(archlinux.read_build_info(package), info)
                else:
                    with self.assertRaisesRegex(ValueError, "Arch package metadata mismatch"):
                        archlinux.read_build_info(package)
