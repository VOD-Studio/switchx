#!/usr/bin/env python3
"""Build portable native packages, or verify the complete release asset set."""

import argparse
import contextlib
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile
from pathlib import Path

from release import ROOT, check
import appimage


TARGETS = {
    "aarch64-apple-darwin": ("darwin", "arm64", ".dmg"),
    "x86_64-apple-darwin": ("darwin", "x86_64", ".dmg"),
    "aarch64-pc-windows-msvc": ("win32", "arm64", ".zip"),
    "x86_64-pc-windows-msvc": ("win32", "x86_64", ".zip"),
    "aarch64-unknown-linux-gnu": ("linux", "arm64", ".tar.gz"),
    "x86_64-unknown-linux-gnu": ("linux", "x86_64", ".tar.gz"),
}


def archive_name(version, target):
    return f"SwitchX-{version}-{target}{TARGETS[target][2]}"


def package_names(version, target):
    names = [archive_name(version, target)]
    if "linux" in target:
        names.append(f"SwitchX-{version}-{target}.AppImage")
    return names


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def output(*command):
    return subprocess.check_output(command, cwd=ROOT, text=True, encoding="utf-8").strip()


def create_dmg(source, image, volume_name):
    command = ["hdiutil", "create", "-volname", volume_name, "-srcfolder", str(source),
               "-fs", "HFS+", "-format", "UDZO", "-imagekey", "zlib-level=9", "-ov", "-quiet",
               str(image)]
    # GitHub's macOS runners intermittently report "Resource busy" while creating images.
    for attempt in range(3):
        if attempt:
            time.sleep(5)
        result = subprocess.run(command)
        if result.returncode == 0:
            return
    raise subprocess.CalledProcessError(result.returncode, command)


@contextlib.contextmanager
def mounted(image):
    with tempfile.TemporaryDirectory(prefix="switchx-dmg-") as mountpoint:
        # Attaching verifies the image checksums; -nobrowse keeps it out of Finder.
        subprocess.run(["hdiutil", "attach", "-readonly", "-nobrowse", "-noautoopen", "-quiet",
                        "-mountpoint", mountpoint, str(image)], check=True)
        try:
            yield Path(mountpoint)
        finally:
            subprocess.run(["hdiutil", "detach", "-force", "-quiet", mountpoint], check=True)


def build(target, destination):
    version = check(ROOT)
    host = output("rustc", "-vV").split("host: ", 1)[1].splitlines()[0]
    if host != target:
        raise ValueError(f"Use a native {target} runner; current Rust host is {host}")
    destination.mkdir(parents=True, exist_ok=True)
    if "apple-darwin" in target:
        subprocess.run(["sh", "scripts/bundle-macos.sh", "--release", "--target", target],
                       cwd=ROOT, check=True, env={**os.environ, "PYTHON": sys.executable})
    else:
        subprocess.run(["cargo", "build", "--locked", "--release", "--bin", "switchx",
                        "--target", target], cwd=ROOT, check=True)
    metadata = json.loads(output("cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"))
    built = Path(metadata["target_directory"]) / target / "release"
    executable = built / ("switchx.exe" if "windows" in target else "switchx")
    if output(str(executable), "--version") != f"SwitchX {version}":
        raise ValueError("Built executable does not report the workspace version")
    name = archive_name(version, target)
    archive = destination / name
    archives = [archive]
    with tempfile.TemporaryDirectory(prefix="switchx-package-") as temporary:
        staging = Path(temporary) / f"SwitchX-{version}-{target}"
        staging.mkdir()
        for document in ["README.md", "CHANGELOG.md"]:
            shutil.copy2(ROOT / document, staging)
        shutil.copy2(ROOT / "assets/providers/NOTICE.md", staging / "Provider-Icons-NOTICE.md")
        if "apple-darwin" in target:
            shutil.copytree(built / "SwitchX.app", staging / "SwitchX.app", symlinks=True)
            # Lets users drag the app into Applications from the opened image.
            (staging / "Applications").symlink_to("/Applications")
        else:
            shutil.copy2(executable, staging)
        (staging / "BUILD-INFO.json").write_text(json.dumps({
            "version": version,
            "target": target,
            "commit": output("git", "rev-parse", "HEAD"),
            "dirty": bool(output("git", "status", "--porcelain")),
            "rustc": output("rustc", "--version"),
        }, indent=2) + "\n", encoding="utf-8")
        if name.endswith(".dmg"):
            create_dmg(staging, archive, f"SwitchX {version}")
            with mounted(archive) as volume:
                # The copy inside the image is what users install; its ad-hoc seal must hold.
                subprocess.run(["codesign", "--verify", "--deep", "--strict",
                                str(volume / "SwitchX.app")], check=True)
        elif name.endswith(".zip"):
            shutil.make_archive(str(archive.with_suffix("")), "zip", staging.parent, staging.name)
        else:
            with tarfile.open(archive, "w:gz") as destination_file:
                destination_file.add(staging, arcname=staging.name)
        if "linux" in target:
            image = destination / package_names(version, target)[1]
            appimage.build(staging, image, target, version)
            archives.append(image)
    for archive in archives:
        (destination / f"{archive.name}.sha256").write_text(
            f"{digest(archive)}  {archive.name}\n", encoding="utf-8")
        print(archive)


def verify_assets(directory, version, commit):
    packages = [(target, name) for target in TARGETS for name in package_names(version, target)]
    expected = {name for _, name in packages}
    expected |= {f"{name}.sha256" for name in expected}
    present = {item.name for item in directory.iterdir()} - {"SHA256SUMS"}
    if present != expected:
        raise ValueError(f"Release assets differ: missing={sorted(expected - present)}, "
                         f"unexpected={sorted(present - expected)}")
    checksums = []
    for target, name in packages:
        archive = directory / name
        checksum = f"{digest(archive)}  {name}\n"
        if (directory / f"{name}.sha256").read_text(encoding="utf-8") != checksum:
            raise ValueError(f"Checksum mismatch: {name}")
        info_path = f"SwitchX-{version}-{target}/BUILD-INFO.json"
        if name.endswith(".dmg"):
            # The image's root is the package folder, so metadata sits at the volume root.
            with mounted(archive) as volume:
                info = json.loads((volume / "BUILD-INFO.json").read_text(encoding="utf-8"))
        elif name.endswith(".AppImage"):
            info = appimage.read_build_info(archive)
        elif name.endswith(".zip"):
            with zipfile.ZipFile(archive) as source:
                info = json.loads(source.read(info_path))
        else:
            with tarfile.open(archive) as source:
                stream = source.extractfile(info_path)
                if stream is None:
                    raise ValueError(f"Missing build metadata: {name}")
                info = json.load(stream)
        if (info.get("version"), info.get("target"), info.get("commit")) != (version, target, commit):
            raise ValueError(f"Build metadata mismatch: {name}")
        if info.get("dirty") is not False:
            raise ValueError(f"Package was built from an uncommitted working tree: {name}")
        checksums.append(checksum)
    (directory / "SHA256SUMS").write_text("".join(sorted(checksums)), encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--target", choices=TARGETS)
    action.add_argument("--verify-dir", type=Path)
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist")
    parser.add_argument("--tag")
    args = parser.parse_args()
    try:
        if args.verify_dir:
            if not args.tag:
                raise ValueError("--verify-dir requires --tag")
            verify_assets(args.verify_dir, check(ROOT, args.tag), output("git", "rev-parse", "HEAD"))
            print("Verified all eight native packages and wrote SHA256SUMS")
        else:
            build(args.target, args.output_dir.resolve())
    except (ValueError, OSError, subprocess.CalledProcessError, KeyError) as error:
        parser.exit(1, f"Packaging error: {error}\n")


if __name__ == "__main__":
    main()
