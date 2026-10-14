#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 HiFiPhile
# SPDX-License-Identifier: GPL-3.0-only
"""Verify a tagged release module before publishing it, and write SHA256SUMS.

Usage: verify-release.py vMAJOR.MINOR [module.zip]
"""
import hashlib
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import zipfile

PIN_FILE = Path("release-signing-certificate.sha256")
UPDATE_JSON = "https://github.com/HiFiPhile/UVCamera/raw/master/app/module/updates/release/info.json"
ABIS = ["arm64-v8a", "armeabi-v7a", "x86_64"]
SUPERVISOR = b"Lcom/hifiphile/uvcamera/standalone/AuthSupervisor;"


def main():
    if len(sys.argv) not in (2, 3) or not re.fullmatch(r"v[0-9]+\.[0-9]+", sys.argv[1]):
        raise SystemExit(__doc__)
    version = sys.argv[1][1:]
    major, minor = map(int, version.split("."))
    module = Path(sys.argv[2]) if len(sys.argv) == 3 else \
        Path(f"app/build/distributions/release/UVCamera-{version}-release.zip")

    # The public half of the release key, committed so a build signed with
    # any other key fails here.
    expected_pin = PIN_FILE.read_text().strip()
    assert re.fullmatch(r"[0-9a-f]{64}", expected_pin), f"{PIN_FILE} is not a SHA-256"

    with zipfile.ZipFile(module) as archive:
        props = dict(line.split("=", 1) for line in archive.read("module.prop").decode().splitlines() if "=" in line)
        assert props["id"] == "com.hifiphile.uvcamera", props["id"]
        assert props["name"] == "UVCamera", props["name"]
        assert props["version"] == "v" + version, props["version"]
        assert int(props["versionCode"]) == (major << 16) | (minor << 8), props["versionCode"]
        assert props["updateJson"] == UPDATE_JSON, props["updateJson"]
        # The supervisor only lets the app with this certificate talk to uvcd.
        assert archive.read("client-cert.sha256").decode().strip() == expected_pin, "Daemon pin differs"
        for abi in ABIS:
            assert archive.read("uvcd." + abi).startswith(b"\x7fELF"), f"uvcd.{abi} is not an ELF"

        with tempfile.TemporaryDirectory() as temporary:
            apk = Path(temporary) / "UVCamera.apk"
            apk.write_bytes(archive.read("UVCamera.apk"))
            sdk = Path(os.environ["ANDROID_HOME"]) / "build-tools/37.0.0"
            verified = subprocess.check_output([str(sdk / "apksigner"), "verify", "--print-certs", str(apk)], text=True)
            pins = re.findall(r"certificate SHA-256 digest:\s*([0-9a-fA-F]{64})", verified)
            assert pins, f"No signing certificate digest in apksigner output:\n{verified}"
            assert {pin.lower() for pin in pins} == {expected_pin}, "APK signer differs from the release pin"
            # The module starts the supervisor by class name; shrinking must keep it.
            with zipfile.ZipFile(apk) as app:
                assert any(SUPERVISOR in app.read(name) for name in app.namelist()
                           if re.fullmatch(r"classes[0-9]*\.dex", name)), "Supervisor removed by shrinking"

    digest = hashlib.sha256(module.read_bytes()).hexdigest()
    (module.parent / "SHA256SUMS").write_text(f"{digest}  {module.name}\n")
    print(f"Verified {module.name}: metadata, {len(ABIS)} ABIs, APK signature, pin and supervisor")
    print(f"SHA-256: {digest}")


if __name__ == "__main__":
    main()
