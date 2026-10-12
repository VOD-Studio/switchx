#!/usr/bin/env python3
"""Check release metadata, prepare a version, and extract its release notes."""

import argparse
import datetime
import re
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:
    raise SystemExit("Release tools require Python 3.11+; use PYTHON=python3.12 for Make commands")


ROOT = Path(__file__).resolve().parent.parent
VERSION = re.compile(
    r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
)
HEADING = re.compile(r"^## \[([^\]]+)\](?: - (\d{4}-\d{2}-\d{2}))?$", re.MULTILINE)


def parse_version(version):
    match = VERSION.fullmatch(version)
    if not match:
        raise ValueError(f"Invalid version: {version}; use X.Y.Z or X.Y.Z-rc.1")
    major, minor, patch, prerelease = match.groups()
    identifiers = prerelease.split(".") if prerelease else []
    if any(item.isdigit() and len(item) > 1 and item.startswith("0") for item in identifiers):
        raise ValueError("Numeric prerelease identifiers cannot have leading zeros")
    # A final version sorts after all prereleases of the same numeric version.
    suffix = tuple((0, int(item)) if item.isdigit() else (1, item) for item in identifiers)
    return (int(major), int(minor), int(patch), not prerelease, suffix)


def load_toml(path):
    with path.open("rb") as source:
        return tomllib.load(source)


def sections(text):
    headings = list(HEADING.finditer(text))
    if len(headings) != len(re.findall(r"^## .+$", text, re.MULTILINE)):
        raise ValueError("Changelog headings must use ## [Unreleased] or ## [X.Y.Z] - YYYY-MM-DD")
    entries = {}
    for index, heading in enumerate(headings):
        version, date = heading.groups()
        if version in entries:
            raise ValueError(f"Duplicate changelog section: {version}")
        if version != "Unreleased":
            parse_version(version)
            if not date:
                raise ValueError(f"Missing release date for {version}")
            datetime.date.fromisoformat(date)
        elif date:
            raise ValueError("Unreleased must not have a release date")
        end = headings[index + 1].start() if index + 1 < len(headings) else len(text)
        entries[version] = (heading, text[heading.end():end].strip())
    if not headings or headings[0].group(1) != "Unreleased":
        raise ValueError("CHANGELOG.md must start with an Unreleased section")
    return entries


def release_notes(text, version):
    entries = sections(text)
    if version not in entries:
        raise ValueError(f"CHANGELOG.md has no release section for {version}")
    notes = entries[version][1]
    if not re.search(r"^- \S", notes, re.MULTILINE):
        raise ValueError(f"Release {version} has no changelog entries")
    return notes + "\n"


def check(root, tag=None):
    manifest = load_toml(root / "Cargo.toml")
    version = manifest["workspace"]["package"]["version"]
    parse_version(version)
    members = [".", *manifest["workspace"]["members"]]
    names = set()
    for member in members:
        package = load_toml(root / member / "Cargo.toml")["package"]
        if package.get("version") != {"workspace": True}:
            raise ValueError(f"{member}: package version must inherit workspace.package.version")
        names.add(package["name"])
    locked = load_toml(root / "Cargo.lock")["package"]
    for name in names:
        local = [package for package in locked if package["name"] == name and "source" not in package]
        if len(local) != 1 or local[0]["version"] != version:
            raise ValueError(f"Cargo.lock version mismatch for {name}")
    changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    entries = sections(changelog)
    released = [item for item in entries if item != "Unreleased"]
    if released != sorted(released, key=parse_version, reverse=True):
        raise ValueError("Released changelog sections must be newest first")
    if released and parse_version(version) < parse_version(released[0]):
        raise ValueError("Workspace version is older than the latest changelog release")
    if tag is not None:
        if tag != f"v{version}":
            raise ValueError(f"Tag {tag} does not match workspace version v{version}")
        release_notes(changelog, version)
    return version


def prepare(root, version, date):
    current = check(root)
    parse_version(version)
    datetime.date.fromisoformat(date)
    changelog_path = root / "CHANGELOG.md"
    changelog = changelog_path.read_text(encoding="utf-8")
    entries = sections(changelog)
    if version in entries:
        raise ValueError(f"Version {version} already exists in CHANGELOG.md")
    if parse_version(version) < parse_version(current):
        raise ValueError(f"Version {version} is older than current version {current}")
    released = [item for item in entries if item != "Unreleased"]
    if released and parse_version(version) <= parse_version(released[0]):
        raise ValueError("New release must be newer than the last release")
    if not re.search(r"^- \S", entries["Unreleased"][1], re.MULTILINE):
        raise ValueError("Unreleased has no changelog entries")

    manifest_path = root / "Cargo.toml"
    manifest = manifest_path.read_text(encoding="utf-8")
    pattern = r'(\[workspace\.package\]\s*\nversion = ")[^"]+("[^\n]*)'
    manifest, count = re.subn(pattern, lambda match: match[1] + version + match[2], manifest)
    if count != 1:
        raise ValueError("Cannot locate workspace.package.version")
    lock_path = root / "Cargo.lock"
    lock = lock_path.read_text(encoding="utf-8")
    names = [load_toml(root / member / "Cargo.toml")["package"]["name"]
             for member in [".", *load_toml(manifest_path)["workspace"]["members"]]]
    for name in names:
        pattern = rf'(\[\[package\]\]\nname = "{re.escape(name)}"\nversion = ")[^"]+("\n)'
        lock, count = re.subn(pattern, lambda match: match[1] + version + match[2], lock)
        if count != 1:
            raise ValueError(f"Cannot locate {name} in Cargo.lock")
    heading = entries["Unreleased"][0]
    changelog = (changelog[:heading.end()] + f"\n\n## [{version}] - {date}"
                 + changelog[heading.end():])
    # Validate all text transformations before writing any file.
    release_notes(changelog, version)
    manifest_path.write_text(manifest, encoding="utf-8")
    lock_path.write_text(lock, encoding="utf-8")
    changelog_path.write_text(changelog, encoding="utf-8")
    check(root, f"v{version}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    validate = commands.add_parser("check")
    validate.add_argument("--tag")
    validate.add_argument("--notes-output", type=Path)
    bump = commands.add_parser("prepare")
    bump.add_argument("version")
    bump.add_argument("--date", default=datetime.date.today().isoformat())
    args = parser.parse_args()
    try:
        if args.command == "prepare":
            prepare(ROOT, args.version, args.date)
            print(f"Prepared v{args.version}; review CHANGELOG.md and run make check before tagging")
        else:
            version = check(ROOT, args.tag)
            if args.notes_output:
                if not args.tag:
                    raise ValueError("--notes-output requires --tag")
                args.notes_output.parent.mkdir(parents=True, exist_ok=True)
                args.notes_output.write_text(
                    release_notes((ROOT / "CHANGELOG.md").read_text(encoding="utf-8"), version),
                    encoding="utf-8",
                )
            print(version)
    except (ValueError, KeyError, OSError) as error:
        parser.exit(1, f"Release metadata error: {error}\n")


if __name__ == "__main__":
    main()
