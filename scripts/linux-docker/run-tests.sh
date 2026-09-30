#!/bin/sh
# Linux verification of the RPool source copy mounted at /src (read-only).
set -e
cd /src
cargo fmt --check
RUSTFLAGS="-D warnings -A deprecated" cargo check --locked --all-targets
cargo test --locked --bin rpool frontend::fuse -- --ignored --test-threads=1
cargo test --locked --bin rpool
