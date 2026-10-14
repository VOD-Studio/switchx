"""Linux AppImage packaging and cross-platform inspection (no FUSE required)."""

import hashlib
import json
import os
import shutil
import sqlite3
import struct
import subprocess
import tempfile
import urllib.request
from pathlib import Path

from release import ROOT


# Immutable release URLs and SHA-256 digests from the upstream release assets.
# The runtime is pinned separately so appimagetool never downloads a moving build.
TOOLS = {
    "linuxdeploy": ("linuxdeploy/linuxdeploy", "1-alpha-20251107-1", {
        "x86_64": "c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d",
        "aarch64": "620095110d693282b8ebeb244a95b5e911cf8f65f76c88b4b47d16ae6346fcff",
    }),
    "appimagetool": ("AppImage/appimagetool", "1.9.1", {
        "x86_64": "ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0",
        "aarch64": "f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158",
    }),
    "runtime": ("AppImage/type2-runtime", "20251108", {
        "x86_64": "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d",
        "aarch64": "00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444",
    }),
}


def tool(name, architecture):
    repository, version, digests = TOOLS[name]
    filename = f"{name}-{architecture}" + ("" if name == "runtime" else ".AppImage")
    cache = ROOT / "target/appimage-tools" / name / version
    cache.mkdir(parents=True, exist_ok=True)
    destination = cache / filename
    if not destination.exists():
        url = f"https://github.com/{repository}/releases/download/{version}/{filename}"
        print(f"Downloading {url}", flush=True)
        with tempfile.TemporaryDirectory(dir=cache) as temporary:
            downloaded = Path(temporary) / filename
            with urllib.request.urlopen(url, timeout=60) as source, downloaded.open("wb") as output:
                shutil.copyfileobj(source, output)
            verify_tool(downloaded, digests[architecture])
            downloaded.replace(destination)
    verify_tool(destination, digests[architecture])
    destination.chmod(0o755)
    return destination


def verify_tool(path, expected):
    with path.open("rb") as source:
        actual = hashlib.file_digest(source, "sha256").hexdigest()
    if actual != expected:
        raise ValueError(f"AppImage tool checksum mismatch: {path}")


def squashfs_offset(image):
    """Type 2 payload begins at the end of the runtime ELF, not a guessed magic string.

    Read ELF64 little-endian section headers without executing the Linux runtime,
    so the release verifier can also inspect both Linux architectures on macOS.
    """
    with image.open("rb") as source:
        header = source.read(64)
        if len(header) != 64 or header[:6] != b"\x7fELF\x02\x01" or header[8:11] != b"AI\x02":
            raise ValueError(f"Not a supported type 2 AppImage: {image}")
        table = struct.unpack_from("<Q", header, 40)[0]
        entry_size, count = struct.unpack_from("<HH", header, 58)
        if entry_size != 64 or not 0 < count <= 4096 or table + count * 64 > 16 * 1024 * 1024:
            raise ValueError(f"Invalid AppImage ELF section table: {image}")
        offset = table + count * entry_size
        source.seek(table)
        for _ in range(count):
            section = source.read(entry_size)
            if len(section) != entry_size:
                raise ValueError(f"Truncated AppImage runtime: {image}")
            kind = struct.unpack_from("<I", section, 4)[0]
            start, size = struct.unpack_from("<QQ", section, 24)
            if kind != 8:  # SHT_NOBITS is zero-initialized memory, not file contents.
                offset = max(offset, start + size)
        if offset > 16 * 1024 * 1024:
            raise ValueError(f"Invalid AppImage runtime size: {image}")
        source.seek(offset)
        if source.read(4) != b"hsqs":
            raise ValueError(f"Missing AppImage SquashFS payload: {image}")
    return offset


def read_build_info(image):
    if not shutil.which("unsquashfs"):
        raise ValueError("Install squashfs-tools (Linux) or squashfs (Homebrew) to verify AppImages")
    payload = subprocess.check_output([
        "unsquashfs", "-cat", "-o", str(squashfs_offset(image)), str(image), "BUILD-INFO.json",
    ])
    return json.loads(payload)


def smoke_test(image, version):
    """Exercise fresh runtime launches using only an isolated synthetic database."""
    with tempfile.TemporaryDirectory(prefix="switchx-appimage-smoke-") as temporary:
        root = Path(temporary)
        data = root / "data with spaces"
        data.mkdir(mode=0o700)
        database = data / "switchx.sqlite"
        reference = "router-" + "a" * 32
        token = "b" * 64
        with sqlite3.connect(database) as connection:
            # The helper deliberately supports this old, stable credential schema.
            connection.executescript("""
                PRAGMA user_version = 8;
                CREATE TABLE providers (id TEXT PRIMARY KEY, settings_config TEXT NOT NULL);
                CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            """)
            connection.execute("INSERT INTO providers VALUES (?, ?)", (
                "synthetic-api", json.dumps({"auth": {"OPENAI_API_KEY": "synthetic-api-key"}}),
            ))
            connection.execute("INSERT INTO app_settings VALUES (?, ?)",
                               (f"local_token:{reference}", token))
        database.chmod(0o600)
        before = database.read_bytes()
        # Also check paths containing spaces and non-ASCII text, without relying on
        # a mount surviving from one credential-helper invocation to the next.
        launcher = root / "SwitchX 中文 space.AppImage"
        launcher.symlink_to(image)
        environment = {**os.environ, "APPIMAGE_EXTRACT_AND_RUN": "1",
                       "SWITCHX_DATA_DIR": str(data), "CODEX_HOME": str(root / "codex")}
        for arguments, expected in [
            (["--version"], f"SwitchX {version}"),
            (["credential", "synthetic-api", str(data)], "synthetic-api-key"),
            (["local-token", reference, str(data)], token),
        ]:
            result = subprocess.run([str(launcher), *arguments], env=environment,
                                    capture_output=True, text=True, timeout=60)
            if result.returncode != 0 or result.stdout.strip() != expected:
                # Never echo helper stdout or credential contents on failure.
                raise ValueError(f"AppImage {arguments[0]} smoke test failed")
        if database.read_bytes() != before or (root / "codex").exists():
            raise ValueError("AppImage helper modified its isolated credential/configuration data")


def build(staging, image, target, version):
    architecture = target.split("-", 1)[0]
    linuxdeploy = tool("linuxdeploy", architecture)
    appimagetool = tool("appimagetool", architecture)
    runtime = tool("runtime", architecture)
    with tempfile.TemporaryDirectory(prefix="switchx-appimage-") as temporary:
        appdir = Path(temporary) / "SwitchX.AppDir"
        appdir.mkdir()
        for filename in ["BUILD-INFO.json", "README.md", "CHANGELOG.md", "Provider-Icons-NOTICE.md"]:
            shutil.copy2(staging / filename, appdir / filename)
        icon = Path(temporary) / "switchx.png"
        converter = shutil.which("magick") or shutil.which("convert")
        if not converter:
            raise ValueError("Install ImageMagick to resize the AppImage desktop icon")
        subprocess.run([converter, str(ROOT / "assets/app-icon.png"), "-resize", "512x512",
                        str(icon)], check=True)
        # Extract-and-run supports containers without /dev/fuse; tool environment
        # variables stay confined to the packaging processes.
        # linuxdeploy's bundled strip predates RELR sections used by newer distros.
        # Keep system libraries intact; strip only our copied executable with the
        # native toolchain before deployment.
        subprocess.run(["strip", "--strip-unneeded", str(staging / "switchx")], check=True)
        environment = {**os.environ, "APPIMAGE_EXTRACT_AND_RUN": "1", "ARCH": architecture,
                       "VERSION": version, "NO_STRIP": "1"}
        subprocess.run([
            str(linuxdeploy), "--appdir", str(appdir), "--executable", str(staging / "switchx"),
            "--desktop-file", str(ROOT / "packaging/linux/switchx.desktop"),
            "--icon-file", str(icon), "--custom-apprun", str(ROOT / "packaging/linux/AppRun"),
        ], check=True, env=environment)
        subprocess.run([
            str(appimagetool), "--runtime-file", str(runtime), str(appdir), str(image),
        ], check=True, env=environment)
    image.chmod(0o755)
    smoke_test(image, version)
    if read_build_info(image) != json.loads((staging / "BUILD-INFO.json").read_text(encoding="utf-8")):
        raise ValueError("AppImage build metadata differs from its input")
