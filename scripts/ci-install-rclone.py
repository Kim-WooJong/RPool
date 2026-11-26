#!/usr/bin/env python3
"""Install pinned public rclone with official SHA-256 checksum verification."""
import hashlib
import os
import platform
from pathlib import Path
import subprocess
import zipfile
VERSION = "1.75.1"
# Recorded official release checksums, independent of the runtime download host.
EXPECTED = {
    "linux-amd64": "982b5aa772841168f8e380f139e9e787b2a105403e32b94da8676a0e1c0a13ab",
    "osx-amd64": "29253d0288b8fbbac46baad6e5f6add6cb01d462c79f10805bbd4631c4cdf82c",
    "osx-arm64": "c61d7a371c62bcbbe882c3423aa4b8bf63485c248dd0f692997b8f0c3f6d0c6f",
    "windows-amd64": "200eb602c126d82aa38b51e0f6b9ae837473ff99b51278d3f6f837574c494d6e"
}
system = {"Windows": "windows", "Darwin": "osx", "Linux": "linux"}[platform.system()]
arch = {"AMD64": "amd64", "x86_64": "amd64", "arm64": "arm64", "aarch64": "arm64"}[platform.machine()]
name = f"rclone-v{VERSION}-{system}-{arch}"
base = f"https://downloads.rclone.org/v{VERSION}/"
temp = Path(os.environ["RUNNER_TEMP"]) / "rpool-rclone-dependency"
temp.mkdir(exist_ok=False)
def fetch(url):
    # The official download endpoint accepts curl but can reject urllib clients.
    # Keep ordinary TLS verification and fail closed on HTTP/checksum errors.
    return subprocess.check_output(["curl", "--fail", "--location", "--silent",
        "--show-error", "--connect-timeout", "20", "--max-time", "180",
        "--retry", "2", url])
expected = EXPECTED.get(system + "-" + arch)
if not expected:
    raise SystemExit("Unsupported dependency target")
data = fetch(base + name + ".zip")
if hashlib.sha256(data).hexdigest() != expected:
    raise SystemExit("Dependency checksum mismatch")
archive = temp / (name + ".zip")
archive.write_bytes(data)
exe_name = "rclone.exe" if system == "windows" else "rclone"
exe = temp / exe_name
with zipfile.ZipFile(archive) as z:
    exe.write_bytes(z.read(name + "/" + exe_name))
exe.chmod(0o755)
with open(os.environ["GITHUB_PATH"], "a", encoding="utf-8") as f:
    f.write(str(temp) + "\n")
with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as f:
    f.write("RPOOL_TEST_RCLONE=" + str(exe) + "\n")
subprocess.run([str(exe), "version"], check=True)
