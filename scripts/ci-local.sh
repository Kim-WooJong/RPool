#!/usr/bin/env bash
# Local CI: the checks to run before a commit, in order.
#
#   scripts/ci-local.sh            # fmt, check, clippy (report-only), tests,
#                                  # ignored tests when rclone/age exist,
#                                  # Windows cross check when installed
#   RPOOL_CI_FUSE=1 ...            # also run the /dev/fuse tests (Linux)
#   RPOOL_CI_SKIP_IGNORED=1 ...    # skip the real-tool (ignored) tests
#
# Run it from a working copy under projects/<project>/, never from the
# source repository in artifacts/ (build products stay out of it). Set
# CARGO_TARGET_DIR to keep target/ outside the tree if you like.
# Clippy is report-only until the existing lint debt is cleared.
set -uo pipefail

cd "$(dirname "$0")/.."
case "$PWD/" in
*/artifacts/*)
    echo "ci-local: refusing to build inside artifacts/ ($PWD); use a projects/ working copy" >&2
    exit 2
    ;;
esac

failed=()
step() {
    local name=$1
    shift
    echo
    echo "==> $name: $*"
    if "$@"; then
        echo "--> $name: ok"
    else
        echo "--> $name: FAILED"
        failed+=("$name")
    fi
}

step fmt cargo fmt --check
step check cargo check --locked --all-targets

echo
echo "==> clippy (report-only): cargo clippy --locked --all-targets -- -D warnings"
clippy_log=$(mktemp "${TMPDIR:-/tmp}/rpool-clippy.XXXXXX")
if cargo clippy --locked --all-targets -- -D warnings >"$clippy_log" 2>&1; then
    echo "--> clippy: clean"
else
    count=$(grep -cE '^(error|warning)(\[|:)' "$clippy_log" || true)
    echo "--> clippy: $count diagnostics (not failing; full log: $clippy_log)"
fi

step test cargo test --locked --bin rpool

if [ "${RPOOL_CI_SKIP_IGNORED:-0}" != 1 ] && command -v rclone >/dev/null 2>&1; then
    # FUSE tests need /dev/fuse and mount privileges; e2e_* tests are driven
    # by the Docker/cloud scripts and need their prepared environment.
    skips=(--skip e2e_)
    [ "${RPOOL_CI_FUSE:-0}" = 1 ] || skips+=(--skip mount::frontend::fuse)
    if ! command -v age >/dev/null 2>&1 || ! command -v age-keygen >/dev/null 2>&1; then
        echo
        echo "(age/age-keygen not found: skipping the B6 config_sync tests)"
        skips+=(--skip config_sync::b6)
    fi
    step ignored-tests cargo test --locked --bin rpool -- --ignored "${skips[@]}"
else
    echo
    echo "(rclone not found or RPOOL_CI_SKIP_IGNORED=1: skipping ignored real-tool tests)"
fi

if rustup target list --installed 2>/dev/null | grep -qx x86_64-pc-windows-gnu; then
    step windows-check cargo check --locked --target x86_64-pc-windows-gnu --all-targets
else
    echo
    echo "(x86_64-pc-windows-gnu not installed: skipping the Windows cross check)"
fi

echo
if [ ${#failed[@]} -eq 0 ]; then
    echo "ci-local: all checks passed"
else
    echo "ci-local: FAILED: ${failed[*]}"
    exit 1
fi
