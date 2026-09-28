# ============================================================
# rpool v0.5.15 Nushell helpers
#
# Requires:
#   rpool
#   rclone
#
# No default cloud remote is assumed. Pass every destination
# explicitly with --remote, or use --manifest where supported.
# ============================================================

# Upload one local file as independent data shards.
# Add --data-shards K --parity-shards M to enable Reed-Solomon.
#
# Examples:
#   rpush ./model.gguf \
#       --remote gdrive-crypt:rpool \
#       --remote mega-crypt:rpool \
#       --remote box-crypt:rpool
#
#   rpush ./model.gguf \
#       --remote gdrive-crypt:rpool \
#       --remote mega-crypt:rpool \
#       --remote box-crypt:rpool \
#       --remote koofr-crypt:rpool \
#       --data-shards 6 \
#       --parity-shards 2 \
#       --placement free-ratio \
#       --workers 8
export def --wrapped rpush [
    source: path
    ...rest
] {
    let source = ($source | path expand --strict)
    ^rpool put $source ...$rest
}


# Restore a file from a local or remote manifest.
# With a v0.2 Reed-Solomon manifest, missing/corrupt data shards
# are reconstructed automatically when enough parity survives.
#
# Examples:
#   rpull ./model.gguf.rpool.json ./model.gguf
#   rpull "gdrive-crypt:rpool/model.gguf-abc123/manifest.json" ./model.gguf --workers 8
export def --wrapped rpull [
    manifest: string
    output: path
    ...rest
] {
    ^rpool get $manifest $output ...$rest
}


# Verify every physical data/parity shard.
# Default: existence + plaintext size.
# --full: full remote read + BLAKE3.
export def --wrapped rverify [
    manifest: string
    ...rest
] {
    ^rpool verify $manifest ...$rest
}


# Show physical shard availability and recovery status.
# Add --usage to append cloud quota information.
#
# Examples:
#   rstatus ./model.gguf.rpool.json
#   rstatus ./model.gguf.rpool.json --usage
export def --wrapped rstatus [
    manifest: string
    ...rest
] {
    ^rpool status $manifest ...$rest
}


# Show provider quota/usage information in a compact table.
# With no arguments, query physical capacity backends only.
#
# Examples:
#   rusage
#
#   rusage \
#       --remote gdrive-crypt:rpool \
#       --remote mega-crypt:rpool \
#       --remote box-crypt:rpool
#
#   rusage --manifest ./model.gguf.rpool.json
#   rusage --manifest ./model.gguf.rpool.json --json
export def --wrapped rusage [
    ...rest
] {
    ^rpool usage ...$rest
}


# Launch the native rpool GUI console.
export def --wrapped rgui [
    ...rest
] {
    ^rpool gui ...$rest
}


# Manage named storage pools.
export def --wrapped rpools [...rest] {
    ^rpool pool ...$rest
}

# Manage replicated manifests.
export def --wrapped rmanifest [...rest] {
    ^rpool manifest ...$rest
}

# Search or rebuild the local manifest-derived inventory.
export def --wrapped rinventory [...rest] {
    ^rpool inventory ...$rest
}

# Inspect or prune operation history.
export def --wrapped rhistory [...rest] {
    ^rpool history ...$rest
}

# Run maintenance diagnostics.
export def --wrapped rdoctor [...rest] {
    ^rpool doctor ...$rest
}


# Scrub archive integrity. Full BLAKE3 is the default; add --quick for size/existence only.
export def --wrapped rscrub [
    manifest: string
    ...rest
] {
    ^rpool scrub $manifest ...$rest
}

# Reconstruct and re-upload recoverable bad shards.
export def --wrapped rrepair [
    manifest: string
    ...rest
] {
    ^rpool repair $manifest ...$rest
}

# Provider health and drain/migration operations.
export def --wrapped rprovider [...rest] {
    ^rpool provider ...$rest
}


# Configure per-remote default/base paths.
# Example: rroots set Instance /data/crypt
export def --wrapped rroots [...rest] {
    ^rpool remote-root ...$rest
}

# Export a complete portable artifact tree. When crypt remotes exist, pass an
# age recipient; the resulting secret vault contains only rclone-obscured crypt
# values under age encryption.
# Example:
#   rexport ./managed/rpool --age-recipient age1...
export def --wrapped rexport [
    artifact_root: path
    ...rest
] {
    ^rpool export $artifact_root ...$rest
}

# Import/validate a complete portable artifact tree. Keep the age identity
# outside the artifact root.
# Examples:
#   rimport ./managed/rpool --age-identity ~/.config/age/rpool.key --dry-run
#   rimport ./managed/rpool --age-identity ~/.config/age/rpool.key
export def --wrapped rimport [
    artifact_root: path
    ...rest
] {
    ^rpool import $artifact_root ...$rest
}

# Legacy JSON-only portable settings commands. These intentionally exclude
# crypt secret portability; prefer rexport/rimport for full migration.
export def --wrapped rconfig [...rest] {
    ^rpool config ...$rest
}
