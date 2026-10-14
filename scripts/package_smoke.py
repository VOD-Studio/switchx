#!/usr/bin/env python3
"""Check packaged executables with isolated, synthetic credentials only."""

import argparse
import json
import os
import sqlite3
import subprocess
import tempfile
from pathlib import Path


def smoke_test(executable, version, *, appimage=False):
    """Exercise fresh runtime launches using only an isolated synthetic database."""
    with tempfile.TemporaryDirectory(prefix="switchx-package-smoke-") as temporary:
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
        launcher = root / ("SwitchX 中文 space" + (".AppImage" if appimage else ""))
        launcher.symlink_to(executable)
        environment = {**os.environ, "SWITCHX_DATA_DIR": str(data),
                       "CODEX_HOME": str(root / "codex")}
        if appimage:
            environment["APPIMAGE_EXTRACT_AND_RUN"] = "1"
        for arguments, expected in [
            (["--version"], f"SwitchX {version}"),
            (["credential", "synthetic-api", str(data)], "synthetic-api-key"),
            (["local-token", reference, str(data)], token),
        ]:
            result = subprocess.run([str(launcher), *arguments], env=environment,
                                    capture_output=True, text=True, timeout=60)
            if result.returncode != 0 or result.stdout.strip() != expected:
                # Never echo helper stdout or credential contents on failure.
                raise ValueError(f"Package {arguments[0]} smoke test failed")
        if database.read_bytes() != before or (root / "codex").exists():
            raise ValueError("Package helper modified its isolated credential/configuration data")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable", type=Path)
    parser.add_argument("version")
    args = parser.parse_args()
    smoke_test(args.executable.resolve(), args.version)
