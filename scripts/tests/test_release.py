import importlib.util
import io
import json
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

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


class ReleaseAssetsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for target in packaging.TARGETS:
            self.write_archive(target)

    def write_archive(self, archive_target, **overrides):
        name = packaging.archive_name("0.1.0", archive_target)
        archive = self.root / name
        info_path = f"SwitchX-0.1.0-{archive_target}/BUILD-INFO.json"
        payload = json.dumps({"version": "0.1.0", "target": archive_target, "commit": "synthetic",
                              "dirty": False, **overrides}).encode()
        if name.endswith(".zip"):
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
        self.assertEqual(len(sums.splitlines()), 6)
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

    def test_corrupt_archive_blocks_release(self):
        with next(self.root.glob("*.zip")).open("ab") as destination:
            destination.write(b"corruption")
        with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
            packaging.verify_assets(self.root, "0.1.0", "synthetic")

    def test_wrong_source_commit_blocks_release(self):
        with self.assertRaisesRegex(ValueError, "Build metadata mismatch"):
            packaging.verify_assets(self.root, "0.1.0", "other-commit")

    def test_wrong_version_or_target_blocks_release_even_with_a_valid_checksum(self):
        target = next(iter(packaging.TARGETS))
        for metadata in [{"version": "0.2.0"}, {"target": "wrong-target"}]:
            self.write_archive(target, **metadata)
            with self.subTest(metadata=metadata), self.assertRaisesRegex(ValueError, "Build metadata mismatch"):
                packaging.verify_assets(self.root, "0.1.0", "synthetic")

    def test_dirty_working_tree_blocks_release(self):
        self.write_archive(next(iter(packaging.TARGETS)), dirty=True)
        with self.assertRaisesRegex(ValueError, "uncommitted working tree"):
            packaging.verify_assets(self.root, "0.1.0", "synthetic")


if __name__ == "__main__":
    unittest.main()
