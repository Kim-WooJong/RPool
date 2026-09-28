# rpool <-> dotpush/dotpull adapter
#
# Call rpool-dot-capture before dotpush copies/commits its managed payload.
# Call rpool-dot-restore after dotpull restores the managed payload.
#
# The age identity/private key is machine-local and must remain outside the
# managed artifact tree. No plaintext crypt password is written by these helpers.

export def --wrapped rpool-dot-capture [
    managed_root: path
    age_recipient: string
    ...rest
] {
    let target = ($managed_root | path join "rpool")
    mkdir $target
    ^rpool export $target --age-recipient $age_recipient ...$rest
    $target
}

export def --wrapped rpool-dot-restore [
    managed_root: path
    age_identity: path
    ...rest
] {
    let source = ($managed_root | path join "rpool")
    let portable = ($source | path join "config" "portable-config.json")
    if not ($portable | path exists) {
        error make { msg: ("rpool portable config not found: " + ($portable | into string)) }
    }
    ^rpool import $source --age-identity $age_identity ...$rest
    $source
}
