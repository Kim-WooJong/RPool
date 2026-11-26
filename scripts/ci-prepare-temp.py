#!/usr/bin/env python3
"""Use a private physical CI temp root; do not relax product symlink checks."""
import os
from pathlib import Path
import tempfile

def prepare(base, env_file):
    base = Path(base).resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix="rpool-tests-", dir=base))
    for path in [root, *root.parents]:
        if path.is_symlink() or not path.is_dir():
            raise RuntimeError("Test temp root must have physical directory ancestors")
    value = str(root) + os.sep
    if "\n" in value or "\r" in value:
        raise RuntimeError("Invalid CI environment path")
    with Path(env_file).open("a", encoding="utf-8") as f:
        f.write("TMPDIR=" + value + "\n")
    return root

if __name__ == "__main__":
    prepare(os.environ["RUNNER_TEMP"], os.environ["GITHUB_ENV"])
    print("PHYSICAL_TEST_TMPDIR_OK")
