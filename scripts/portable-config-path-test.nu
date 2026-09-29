#!/usr/bin/env nu
# Isolated real-CLI integration test. No user configuration or credentials.
def check [ok: bool label: string] {
    if not $ok { error make {msg: $label} }
    print ("PASS: " + $label)
}

def call [binary: path args: list<string>] {
    let result = (run-external $binary ...$args | complete)
    if $result.exit_code != 0 { error make {msg: $result.stderr} }
    $result.stdout
}

def suite [binary: path base: path] {
    let paths = (call $binary [config paths] | from json)
    let expected = if $nu.os-info.name == "windows" {
        $env.APPDATA | path join rpool gui.json
    } else if $nu.os-info.name == "macos" {
        $env.HOME | path join Library "Application Support" rpool gui.json
    } else {
        $env.XDG_CONFIG_HOME | path join rpool gui.json
    }
    check ($paths.gui == $expected) "CLI uses the platform's active gui.json"
    mkdir ($paths.gui | path dirname)
    {
        rclone: "source-local-rclone"
        workers: 7
        encryption: {entropy_bits: 512 filename_encryption: "off" directory_encryption: false root: ""}
    } | to json | save $paths.gui
    let bundle = ($base | path join portable.json)
    call $binary [config export $bundle] | ignore
    let exported = (open --raw $bundle | from json)
    check ($exported.gui.workers == 7 and $exported.gui.encryption.entropy_bits == 512) "Export reads GUI and encryption preferences from active file"
    check (not ("rclone" in ($exported.gui | columns))) "Machine-local executable path is not exported"
    {rclone: "destination-local-rclone" workers: 2 encryption: {entropy_bits: 256}} | to json | save --force $paths.gui
    let before = (open --raw $paths.gui)
    call $binary [config import $bundle --dry-run] | ignore
    check ((open --raw $paths.gui) == $before) "Dry run does not modify active GUI"
    call $binary [config import $bundle] | ignore
    let restored = (open --raw $paths.gui | from json)
    check ($restored.workers == 7 and $restored.encryption.entropy_bits == 512 and $restored.encryption.filename_encryption == "off") "Import restores active GUI preferences"
    check ($restored.rclone == "destination-local-rclone") "Import preserves destination executable path"
    check (($paths.pools | path exists) and ($paths.remote_roots | path exists)) "Import writes active pools and remote roots"
    $exported | update gui {|row| $row.gui | reject encryption } | to json | save --force $bundle
    call $binary [config import $bundle] | ignore
    check ((open --raw $paths.gui | from json).encryption.entropy_bits == 512) "Old bundles preserve local encryption preferences"
    $exported | update gui.encryption.entropy_bits 12 | to json | save --force $bundle
    let stable = (open --raw $paths.gui)
    let bad = (run-external $binary config import $bundle | complete)
    check ($bad.exit_code != 0 and (open --raw $paths.gui) == $stable) "Invalid encryption preferences fail without replacing GUI"
}

def main [binary: path] {
    let binary = ($binary | path expand)
    let base = (mktemp -d)
    let result = (try {
        with-env {
            HOME: ($base | path join home)
            APPDATA: ($base | path join "Roaming Test")
            XDG_CONFIG_HOME: ($base | path join "xdg test")
        } { suite $binary $base }
        null
    } catch {|err| $err})
    rm --recursive --force $base
    if $result != null { error make {msg: $result.msg} }
}
