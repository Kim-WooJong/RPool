def main [
    --root: path = "."
    --dry-run
] {
    let project_root = ($root | path expand)
    let cargo_toml = ($project_root | path join "Cargo.toml")

    if not ($cargo_toml | path exists) {
        error make { msg: ("Cargo.toml not found under " + ($project_root | into string)) }
    }

    let cargo_text = (open --raw $cargo_toml)
    if not ($cargo_text | str contains 'name = "rpool"') {
        error make { msg: ("Refusing cleanup: " + ($project_root | into string) + " does not look like the rpool project") }
    }

    # Files retired by GUI migrations and next/unreleased storage pivot Step 11.
    # The list is explicit so cleanup never removes unknown user files.
    let obsolete_files = [
        "src/storage/cat.rs"
        "src/storage/copy.rs"
        "src/storage/download.rs"
        "src/storage/probe.rs"
        "src/storage/quota.rs"
        "src/storage/stat.rs"
        "src/storage/transfer.rs"
        "src/storage/upload.rs"
        "src/storage/verify.rs"
        "src/models/storage.rs"
        "src/models/sync_config.rs"
        "src/gui/state.rs"
        "src/gui/widgets/usage_card.rs"
        "src/gui/screens/dashboard.rs"
        "src/gui/screens/maintenance.rs"
        "src/gui/screens/integrity.rs"
        "src/gui/screens/manifest.rs"
        "src/gui/screens/pools.rs"
        "src/gui/screens/providers.rs"
        "src/gui/screens/storage/providers/actions.rs"
        "src/gui/screens/restore.rs"
        "src/gui/screens/status.rs"
        "src/gui/screens/upload.rs"
        "src/gui/screens/verify.rs"
        "src/gui/screens/files/inventory.rs"
        "src/gui/screens/files/upload.rs"
        "src/gui/task_runner.rs"
        "src/gui/screens/maintenance/integrity.rs"
        "src/gui/screens/maintenance/manifest.rs"
        "src/gui/screens/maintenance/system.rs"
        "src/gui/screens/maintenance/integrity/controls.rs"
        "src/gui/screens/maintenance/integrity/repair_legacy.rs"
    ]

    mut removed = 0

    for relative in $obsolete_files {
        let target = ($project_root | path join $relative)
        if ($target | path exists) {
            if $dry_run {
                print ("[dry-run] remove " + $relative)
            } else {
                rm --force $target
                print ("removed " + $relative)
            }
            $removed = $removed + 1
        }
    }

    if $dry_run {
        print ("cleanup check complete: " + ($removed | into string) + " obsolete files found")
    } else {
        print ("cleanup complete: " + ($removed | into string) + " obsolete files removed")
    }
}
