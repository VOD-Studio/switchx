#!/usr/bin/env python3
"""Build an Arch Linux package from the current tracked, hydrated checkout."""

import argparse
import hashlib
import io
import json
import os
import platform
import shutil
import subprocess
import tarfile
from pathlib import Path

from release import ROOT, check, parse_version


PKGREL = 1
TARGET = "x86_64-unknown-linux-gnu"
METADATA = "usr/share/doc/switchx/BUILD-INFO.json"


def package_version(version):
    parse_version(version)
    base, separator, suffix = version.partition("-")
    # A letter immediately after the numeric version sorts before the final in vercmp.
    return base + ("pre." + suffix.replace("-", "_") if separator else "")


def package_name(version):
    return f"switchx-{package_version(version)}-{PKGREL}-x86_64.pkg.tar.zst"


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def git(root, *arguments):
    return subprocess.check_output(["git", *arguments], cwd=root)


def write_source(root, destination, version):
    """Read tracked working files so Git LFS pointers never replace real images."""
    paths = sorted(os.fsdecode(item) for item in git(root, "ls-files", "-z").split(b"\0") if item)
    epoch = int(git(root, "show", "-s", "--format=%ct", "HEAD"))
    info = {
        "version": version, "target": TARGET,
        "commit": git(root, "rev-parse", "HEAD").decode().strip(),
        "dirty": bool(git(root, "status", "--porcelain", "--untracked-files=normal")),
        "format": "archlinux", "pkgver": package_version(version), "pkgrel": PKGREL,
    }
    prefix = f"switchx-{version}"
    with tarfile.open(destination, "w:gz", dereference=False) as archive:
        for name in paths:
            path = root / name
            if path.is_symlink():
                raise ValueError(f"Source symlinks are not supported: {name}")
            if not path.is_file():
                raise ValueError(f"Missing tracked source: {name}")
            with path.open("rb") as source:
                if source.read(128).startswith(b"version https://git-lfs.github.com/spec/v1"):
                    raise ValueError(f"Git LFS pointer: {name}; run git lfs pull before packaging")
                source.seek(0)
                member = archive.gettarinfo(str(path), arcname=f"{prefix}/{name}")
                member.uid = member.gid = 0
                member.uname = member.gname = ""
                # Cargo reuses target/arch-cargo across snapshots. Keep source mtimes
                # so an uncommitted edit invalidates its previous build artifacts.
                archive.addfile(member, source)
        payload = (json.dumps(info, indent=2) + "\n").encode()
        member = tarfile.TarInfo(f"{prefix}/BUILD-INFO.json")
        member.size, member.mtime = len(payload), epoch
        archive.addfile(member, io.BytesIO(payload))
    return info


def prepare(root, destination):
    version = check(root)
    destination.mkdir(parents=True, exist_ok=True)
    source = destination / f"switchx-{version}.tar.gz"
    write_source(root, source, version)
    recipe = (root / "packaging/arch/PKGBUILD.in").read_text(encoding="utf-8")
    for key, value in {
        "VERSION": version, "PKGVER": package_version(version), "PKGREL": str(PKGREL),
        "SOURCE_SHA256": digest(source),
    }.items():
        recipe = recipe.replace(f"@{key}@", value)
    (destination / "PKGBUILD").write_text(recipe, encoding="utf-8")
    return version


def read_build_info(package):
    """libarchive reads zstd packages on Linux and macOS without running the app."""
    if not shutil.which("bsdtar"):
        raise ValueError("Install libarchive (bsdtar) to inspect Arch packages")
    info = json.loads(subprocess.check_output(["bsdtar", "-xOf", str(package), METADATA]))
    metadata = subprocess.check_output(["bsdtar", "-xOf", str(package), ".PKGINFO"], text=True)
    fields = dict(line.split(" = ", 1) for line in metadata.splitlines() if " = " in line)
    expected = {"pkgname": "switchx", "pkgver": f"{package_version(info['version'])}-{PKGREL}",
                "arch": "x86_64"}
    if any(fields.get(key) != value for key, value in expected.items()) or (
        info.get("format"), info.get("pkgver"), info.get("pkgrel")
    ) != ("archlinux", package_version(info["version"]), PKGREL):
        raise ValueError(f"Arch package metadata mismatch: {package.name}")
    return info


def build(destination, syncdeps=False):
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise ValueError("Arch packaging requires a native x86_64 Arch Linux host")
    if os.geteuid() == 0:
        raise ValueError("Run make arch-package as an ordinary user; makepkg cannot run as root")
    if not shutil.which("makepkg"):
        raise ValueError("Install base-devel on Arch Linux before packaging")
    version = prepare(ROOT, destination)
    environment = {**os.environ, "PKGDEST": str(destination), "PKGEXT": ".pkg.tar.zst",
                   "CARGO_TARGET_DIR": str(ROOT / "target/arch-cargo")}
    with (destination / ".SRCINFO").open("w", encoding="utf-8") as output:
        subprocess.run(["makepkg", "--printsrcinfo"], cwd=destination, env=environment,
                       stdout=output, check=True)
    command = ["makepkg", "--force", "--cleanbuild"]
    if syncdeps:
        command.append("--syncdeps")
    subprocess.run(command, cwd=destination, env=environment, check=True)
    package = destination / package_name(version)
    info = read_build_info(package)
    if info["version"] != version or info["target"] != TARGET:
        raise ValueError("Arch package build metadata differs from the requested build")
    (destination / f"{package.name}.sha256").write_text(
        f"{digest(package)}  {package.name}\n", encoding="utf-8")
    print(package)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist/arch")
    parser.add_argument("--prepare-only", action="store_true",
                        help="write hydrated source archive and PKGBUILD without building")
    parser.add_argument("--syncdeps", action="store_true",
                        help="let makepkg install missing dependencies using pacman")
    args = parser.parse_args()
    try:
        if args.prepare_only:
            prepare(ROOT, args.output_dir.resolve())
            print(args.output_dir.resolve() / "PKGBUILD")
        else:
            build(args.output_dir.resolve(), args.syncdeps)
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Arch packaging error: {error}\n")


if __name__ == "__main__":
    main()
