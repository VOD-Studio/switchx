import contextlib
import importlib.util
import io
import json
import re
import shutil
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import release

spec = importlib.util.spec_from_file_location("package_release", SCRIPTS / "package-release.py")
packaging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packaging)


class ReleaseMetadataTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "crates/switchx-ui").mkdir(parents=True)
        (self.root / "Cargo.toml").write_text(
            '[package]\nname = "switchx"\nversion.workspace = true\n'
            '[workspace]\nmembers = ["crates/switchx-ui"]\n'
            '[workspace.package]\nversion = "0.1.0" # preserve comment\nedition = "2024"\n',
            encoding="utf-8",
        )
        (self.root / "crates/switchx-ui/Cargo.toml").write_text(
            '[package]\nname = "switchx-ui"\nversion.workspace = true\n', encoding="utf-8",
        )
        (self.root / "Cargo.lock").write_text(
            'version = 4\n[[package]]\nname = "switchx"\nversion = "0.1.0"\n'
            '[[package]]\nname = "switchx-ui"\nversion = "0.1.0"\n'
            '[[package]]\nname = "dependency"\nversion = "0.1.0"\n'
            'source = "registry+https://github.com/rust-lang/crates.io-index"\n', encoding="utf-8",
        )
        self.changelog = self.root / "CHANGELOG.md"
        self.changelog.write_text('# Changelog\n\n## [Unreleased]\n\n### Added\n\n- 新功能。\n',
                                  encoding="utf-8")

    def snapshot(self):
        return {path: path.read_bytes() for path in self.root.rglob("*") if path.is_file()}

    def test_first_release_can_use_existing_unpublished_version(self):
        release.prepare(self.root, "0.1.0", "2026-10-12")
        self.assertEqual(release.check(self.root, "v0.1.0"), "0.1.0")
        self.assertEqual(release.release_notes(self.changelog.read_text(encoding="utf-8"), "0.1.0"),
                         "### Added\n\n- 新功能。\n")
        self.assertEqual(release.sections(self.changelog.read_text(encoding="utf-8"))["Unreleased"][1], "")

    def test_bump_updates_both_crates_without_updating_dependencies(self):
        release.prepare(self.root, "0.2.0-rc.1", "2026-10-12")
        self.assertEqual(release.check(self.root, "v0.2.0-rc.1"), "0.2.0-rc.1")
        locked = release.load_toml(self.root / "Cargo.lock")["package"]
        self.assertEqual([item["version"] for item in locked], ["0.2.0-rc.1", "0.2.0-rc.1", "0.1.0"])
        self.assertIn('# preserve comment', (self.root / "Cargo.toml").read_text(encoding="utf-8"))

    def test_tag_must_match_version_and_have_release_notes(self):
        for tag in ["v0.2.0", "0.1.0", "v0.1.0"]:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                release.check(self.root, tag)

    def test_lockfile_drift_is_rejected(self):
        path = self.root / "Cargo.lock"
        path.write_text(path.read_text(encoding="utf-8").replace('version = "0.1.0"', 'version = "0.2.0"', 1),
                        encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "Cargo.lock"):
            release.check(self.root)

    def test_member_must_inherit_workspace_version(self):
        (self.root / "crates/switchx-ui/Cargo.toml").write_text(
            '[package]\nname = "switchx-ui"\nversion = "0.1.0"\n', encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "inherit"):
            release.check(self.root)

    def test_invalid_prepare_leaves_all_files_unchanged(self):
        before = self.snapshot()
        for version, date in [("0.0.9", "2026-10-12"), ("v0.2.0", "2026-10-12"),
                              ("0.2.0", "2026-02-30"), ("0.2.0", "20261012")]:
            with self.subTest(version=version, date=date), self.assertRaises(ValueError):
                release.prepare(self.root, version, date)
            self.assertEqual(self.snapshot(), before)

    def test_empty_unreleased_and_duplicate_release_are_rejected(self):
        release.prepare(self.root, "0.1.0", "2026-10-12")
        before = self.snapshot()
        for version in ["0.1.0", "0.2.0"]:
            with self.subTest(version=version), self.assertRaises(ValueError):
                release.prepare(self.root, version, "2026-10-13")
            self.assertEqual(self.snapshot(), before)

    def test_invalid_changelog_dates_duplicates_and_order_are_rejected(self):
        for text in [
            '## [0.1.0] - 2026-10-12\n- Entry\n',
            '## [Unreleased]\n## [0.1.0]\n- Entry\n',
            '## [Unreleased]\n## [0.1.0] - 2026-02-30\n- Entry\n',
            '## [Unreleased]\n## [0.1.0] - 2026-10-12\n## [0.1.0] - 2026-10-12\n',
            '## [Unreleased]\n## [0.1.0] - 2026-10-12\n## [0.2.0] - 2026-10-13\n',
        ]:
            self.changelog.write_text(text, encoding="utf-8")
            with self.subTest(text=text), self.assertRaises(ValueError):
                release.check(self.root)

    def test_release_with_only_empty_category_headings_is_rejected(self):
        self.changelog.write_text('## [Unreleased]\n\n## [0.1.0] - 2026-10-12\n\n### Added\n',
                                  encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "no changelog entries"):
            release.check(self.root, "v0.1.0")

    def test_semver_validation_and_prerelease_order(self):
        for version in ["01.2.3", "1.02.3", "1.2", "v1.2.3", "1.2.3-01", "1.2.3+build", "1.٢.3"]:
            with self.subTest(version=version), self.assertRaises(ValueError):
                release.parse_version(version)
        ordered = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta",
                   "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0", "1.1.0"]
        self.assertEqual(sorted(reversed(ordered), key=release.parse_version), ordered)


@contextlib.contextmanager
def synthetic_mount(image):
    """Stand in for hdiutil: a synthetic image holds only its BUILD-INFO.json bytes."""
    with tempfile.TemporaryDirectory() as volume:
        (Path(volume) / "BUILD-INFO.json").write_bytes(Path(image).read_bytes())
        yield Path(volume)


class ReleaseAssetsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        patcher = mock.patch.object(packaging, "mounted", synthetic_mount)
        patcher.start()
        self.addCleanup(patcher.stop)
        reader = mock.patch.object(packaging.appimage, "read_build_info",
                                   lambda path: json.loads(path.read_bytes()))
        reader.start()
        self.addCleanup(reader.stop)
        arch_reader = mock.patch.object(packaging.archlinux, "read_build_info",
                                        lambda path: json.loads(path.read_bytes()))
        arch_reader.start()
        self.addCleanup(arch_reader.stop)
        for target in packaging.TARGETS:
            self.write_archive(target)
        self.write_package(packaging.archlinux.package_name("0.1.0"), packaging.archlinux.TARGET)
        notes_directory = tempfile.TemporaryDirectory()
        self.addCleanup(notes_directory.cleanup)
        self.notes_input = Path(notes_directory.name) / "changelog.md"
        self.notes_output = Path(notes_directory.name) / "generated/release.md"
        self.notes = "### Added\n\n- 中文更新与 [文档](https://example.org/docs)。\n"
        self.notes_input.write_text(self.notes, encoding="utf-8")

    def write_archive(self, archive_target, **overrides):
        for name in packaging.package_names("0.1.0", archive_target):
            self.write_package(name, archive_target, **overrides)

    def write_package(self, name, archive_target, **overrides):
        archive = self.root / name
        info_path = f"SwitchX-0.1.0-{archive_target}/BUILD-INFO.json"
        payload = json.dumps({"version": "0.1.0", "target": archive_target, "commit": "synthetic",
                              "dirty": False, **overrides}).encode()
        if name.endswith((".dmg", ".AppImage", ".pkg.tar.zst")):
            archive.write_bytes(payload)
        elif name.endswith(".zip"):
            with zipfile.ZipFile(archive, "w") as destination:
                destination.writestr(info_path, payload)
        else:
            with tarfile.open(archive, "w:gz") as destination:
                info = tarfile.TarInfo(info_path)
                info.size = len(payload)
                destination.addfile(info, io.BytesIO(payload))
        (self.root / f"{name}.sha256").write_text(f"{packaging.digest(archive)}  {name}\n",
                                                  encoding="utf-8")

    def test_complete_asset_set_produces_repeatable_checksums(self):
        packaging.verify_assets(self.root, "0.1.0", "synthetic")
        sums = (self.root / "SHA256SUMS").read_text(encoding="utf-8")
        self.assertEqual(len(sums.splitlines()), 9)
        packaging.verify_assets(self.root, "0.1.0", "synthetic")
        self.assertEqual((self.root / "SHA256SUMS").read_text(encoding="utf-8"), sums)

    def test_missing_platform_blocks_release(self):
        next(self.root.glob("*.tar.gz")).unlink()
        with self.assertRaisesRegex(ValueError, "missing"):
            packaging.verify_assets(self.root, "0.1.0", "synthetic")
        self.assertFalse((self.root / "SHA256SUMS").exists())

    def test_extra_stale_asset_blocks_release(self):
        (self.root / "SwitchX-old.zip").write_bytes(b"stale")
        with self.assertRaisesRegex(ValueError, "unexpected"):
            packaging.verify_assets(self.root, "0.1.0", "synthetic")

    def test_missing_appimage_blocks_release(self):
        next(self.root.glob("*.AppImage")).unlink()
        with self.assertRaisesRegex(ValueError, "missing"):
            packaging.verify_assets(self.root, "0.1.0", "synthetic")

    def test_missing_arch_package_blocks_release(self):
        next(self.root.glob("*.pkg.tar.zst")).unlink()
        with self.assertRaisesRegex(ValueError, "missing"):
            packaging.verify_assets(self.root, "0.1.0", "synthetic")

    def test_arch_package_must_match_release_commit_and_be_clean(self):
        name = packaging.archlinux.package_name("0.1.0")
        for metadata, error in [({"commit": "stale"}, "Build metadata mismatch"),
                                ({"dirty": True}, "uncommitted working tree")]:
            self.write_package(name, packaging.archlinux.TARGET, **metadata)
            with self.subTest(metadata=metadata), self.assertRaisesRegex(ValueError, error):
                packaging.verify_assets(self.root, "0.1.0", "synthetic")

    def test_appimage_metadata_is_verified_independently_of_tarball(self):
        target = "aarch64-unknown-linux-gnu"
        name = packaging.package_names("0.1.0", target)[1]
        for metadata, error in [({"commit": "stale"}, "Build metadata mismatch"),
                                ({"dirty": True}, "uncommitted working tree")]:
            self.write_package(name, target, **metadata)
            with self.subTest(metadata=metadata), self.assertRaisesRegex(ValueError, error):
                packaging.verify_assets(self.root, "0.1.0", "synthetic")

    def test_corrupt_archive_blocks_release(self):
        with next(self.root.glob("*.zip")).open("ab") as destination:
            destination.write(b"corruption")
        with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
            packaging.verify_assets(self.root, "0.1.0", "synthetic")

    def test_wrong_source_commit_blocks_release(self):
        with self.assertRaisesRegex(ValueError, "Build metadata mismatch"):
            packaging.verify_assets(self.root, "0.1.0", "other-commit")

    def test_wrong_version_or_target_blocks_release_even_with_a_valid_checksum(self):
        for target in ["aarch64-apple-darwin", "x86_64-pc-windows-msvc"]:
            for metadata in [{"version": "0.2.0"}, {"target": "wrong-target"}]:
                self.write_archive(target, **metadata)
                with self.subTest(target=target, metadata=metadata), \
                        self.assertRaisesRegex(ValueError, "Build metadata mismatch"):
                    packaging.verify_assets(self.root, "0.1.0", "synthetic")
            self.write_archive(target)

    def test_dirty_working_tree_blocks_release(self):
        self.write_archive(next(iter(packaging.TARGETS)), dirty=True)
        with self.assertRaisesRegex(ValueError, "uncommitted working tree"):
            packaging.verify_assets(self.root, "0.1.0", "synthetic")

    def test_macos_packages_are_disk_images(self):
        for target in ["aarch64-apple-darwin", "x86_64-apple-darwin"]:
            self.assertEqual(packaging.archive_name("0.1.0", target), f"SwitchX-0.1.0-{target}.dmg")

    def generate(self, *extra):
        arguments = ["package-release.py", "--verify-dir", str(self.root), "--tag", "v0.1.0",
                     "--repository", "example-fork/switchx", "--notes-input", str(self.notes_input),
                     "--notes-output", str(self.notes_output), *extra]
        with mock.patch.object(sys, "argv", arguments), \
                mock.patch.object(packaging, "check", return_value="0.1.0"), \
                mock.patch.object(packaging, "output", return_value="synthetic"), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            packaging.main()

    def test_release_body_links_to_every_verified_package_and_preserves_changelog(self):
        self.generate()
        body = self.notes_output.read_text(encoding="utf-8")
        links = re.findall(r"\]\((https://github.com/[^)]+)\)", body)
        base = "https://github.com/example-fork/switchx/releases/download/v0.1.0/"
        packages = {path.name for path in self.root.iterdir()
                    if not path.name.endswith(".sha256") and path.name != "SHA256SUMS"}
        self.assertEqual(len(packages), 9)
        self.assertEqual(set(links), {base + name for name in packages | {"SHA256SUMS"}})
        self.assertEqual(len(links), 10)
        self.assertTrue(body.endswith("## 更新日志\n\n" + self.notes))
        self.assertEqual(self.notes_input.read_text(encoding="utf-8"), self.notes)
        self.generate()
        self.assertEqual(self.notes_output.read_text(encoding="utf-8"), body)

    def test_invalid_packages_do_not_write_release_body(self):
        target = "x86_64-unknown-linux-gnu"
        image = self.root / packaging.package_names("0.1.0", target)[1]
        for failure in ["missing", "checksum", "metadata"]:
            if failure == "missing":
                image.unlink()
            elif failure == "checksum":
                image.write_bytes(b"corrupted")
            else:
                self.write_package(image.name, target, commit="wrong-source")
            with self.subTest(failure=failure), self.assertRaises(SystemExit) as error:
                self.generate()
            self.assertEqual(error.exception.code, 1)
            self.assertFalse(self.notes_output.exists())
            self.write_archive(target)

    def test_notes_cannot_overwrite_input_or_become_a_release_asset(self):
        for destination in [self.notes_input, self.root / "release.md"]:
            with self.subTest(destination=destination), self.assertRaises(SystemExit):
                self.generate("--notes-output", str(destination))
            self.assertEqual(self.notes_input.read_text(encoding="utf-8"), self.notes)
            self.assertFalse((self.root / "release.md").exists())


class DownloadTableTests(unittest.TestCase):
    def test_platforms_architectures_and_prerelease_filenames(self):
        body = packaging.download_table("0.2.0-rc.1", "my-org/switchx-fork")
        rows = [line for line in body.splitlines() if line.startswith("| ")][2:]
        self.assertEqual(len(rows), 7)
        expected = [
            ("macOS", "Apple Silicon (ARM64)", "aarch64-apple-darwin.dmg", "DMG"),
            ("macOS", "Intel (x64)", "x86_64-apple-darwin.dmg", "DMG"),
            ("Windows", "ARM64", "aarch64-pc-windows-msvc.zip", "ZIP"),
            ("Windows", "x64", "x86_64-pc-windows-msvc.zip", "ZIP"),
            ("Linux", "ARM64", "aarch64-unknown-linux-gnu.AppImage", "AppImage"),
            ("Linux", "x64", "x86_64-unknown-linux-gnu.AppImage", "AppImage"),
        ]
        for platform, architecture, suffix, label in expected:
            row = next(row for row in rows if row.startswith(f"| {platform} | {architecture} |"))
            self.assertIn(f"SwitchX-0.2.0-rc.1-{suffix}", row)
            self.assertIn(f"![{platform} {architecture} {label}]", row)
            if platform == "Linux":
                self.assertLess(row.index("AppImage"), row.index("tar.gz"))
        arch = next(row for row in rows if row.startswith("| Arch Linux / Omarchy | x64 |"))
        self.assertIn("switchx-0.2.0pre.rc.1-1-x86_64.pkg.tar.zst", arch)
        self.assertNotIn("/latest/", body)
        links = re.findall(r"\]\((https://github.com/[^)]+)\)", body)
        self.assertEqual(len(links), 10)
        self.assertTrue(all(link.startswith(
            "https://github.com/my-org/switchx-fork/releases/download/v0.2.0-rc.1/") for link in links))

    def test_invalid_repositories_cannot_change_download_link_structure(self):
        for repository in ["switchx", "https://github.com/org/repo", "org/repo/other",
                           "org/repo?query=1", "org/repo\n", "org/repo)[unexpected](url)"]:
            with self.subTest(repository=repository), self.assertRaisesRegex(ValueError, "OWNER/REPO"):
                packaging.download_table("0.1.0", repository)

    def test_notes_options_require_verification_and_each_other(self):
        for arguments in [
            ["--verify-dir", "dist", "--tag", "v0.1.0", "--repository", "org/repo"],
            ["--target", "x86_64-unknown-linux-gnu", "--repository", "org/repo",
             "--notes-input", "notes.md", "--notes-output", "body.md"],
        ]:
            with self.subTest(arguments=arguments), \
                    mock.patch.object(sys, "argv", ["package-release.py", *arguments]), \
                    mock.patch.object(packaging, "build") as build, \
                    mock.patch.object(packaging, "verify_assets") as verify, \
                    contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                packaging.main()
            build.assert_not_called()
            verify.assert_not_called()


@unittest.skipUnless(shutil.which("hdiutil"), "requires macOS hdiutil")
class DiskImageTests(unittest.TestCase):
    def test_image_keeps_package_files_and_applications_link(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "SwitchX-0.1.0-aarch64-apple-darwin"
            source.mkdir()
            (source / "BUILD-INFO.json").write_text('{"version": "0.1.0"}\n', encoding="utf-8")
            (source / "Applications").symlink_to("/Applications")
            image = Path(temporary) / "SwitchX.dmg"
            packaging.create_dmg(source, image, "SwitchX 0.1.0")
            with packaging.mounted(image) as volume:
                self.assertEqual(json.loads((volume / "BUILD-INFO.json").read_text(encoding="utf-8")),
                                 {"version": "0.1.0"})
                self.assertEqual((volume / "Applications").readlink(), Path("/Applications"))
            self.assertFalse(volume.exists())


if __name__ == "__main__":
    unittest.main()
