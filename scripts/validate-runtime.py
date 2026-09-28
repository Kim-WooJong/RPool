#!/usr/bin/env python3
"""Cross-platform isolated B6 runner. Requires installed cargo, rclone, age, age-keygen."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rclone", default="rclone")
    parser.add_argument("--age", default="age")
    parser.add_argument("--age-keygen", default="age-keygen")
    parser.add_argument("--cargo", default="cargo")
    parser.add_argument("--toolchain", default="nightly")
    parser.add_argument("--timeout", type=int, default=1200)
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    env = {k: v for k, v in os.environ.items()
           if not k.upper().startswith(("RCLONE_", "_RCLONE_", "RPOOL_TEST_"))}
    tools = {}
    for name in ("cargo", "rclone", "age", "age_keygen"):
        raw = getattr(args, name)
        resolved = shutil.which(raw)
        if not resolved:
            parser.error("Required executable unavailable: " + name)
        tools[name] = str(Path(resolved).absolute())
    for name in ("rclone", "age", "age_keygen"):
        env["RPOOL_TEST_" + name.upper() + "_BIN"] = tools[name]
        version = [tools[name], "version" if name == "rclone" else "--version"]
        subprocess.run(version, env=env, check=True, timeout=30)
    command = [tools["cargo"], "+" + args.toolchain, "test", "--locked", "--bin", "rpool"]
    if args.offline:
        command.append("--offline")
    command += ["b6_real_", "--", "--ignored", "--test-threads=1"]
    return subprocess.run(command, cwd=root, env=env, timeout=args.timeout).returncode


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (subprocess.TimeoutExpired, subprocess.CalledProcessError):
        print("Runtime validation timed out or tool preflight failed.", file=sys.stderr)
        sys.exit(1)
