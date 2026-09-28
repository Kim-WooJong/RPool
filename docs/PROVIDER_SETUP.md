# Provider connection and encryption

## Connect a cloud

Storage → Providers → **+ Connect cloud provider** opens the official `rclone config` wizard in a terminal. Choose New remote, select the service, finish its login, then quit the wizard. RPool detects wizard completion and refreshes providers automatically, including when a cancelled wizard may have saved partial changes. RPool does not collect cloud passwords or OAuth tokens. This is a terminal/browser flow, not an embedded cloud-login form. If completion is not detected after closing the wizard, use **Wizard already closed — refresh**.

## Automatic encryption

After provider discovery, the GUI automatically provisions encryption for base providers that do not already have a data-encrypting crypt remote. Setup waits until the external connection wizard closes. Existing crypt remotes and keys are retained; existing cloud files are not encrypted in place.

Automatic setup defaults to **1024-bit random password entropy**, Standard filename encryption and encrypted directory names. The new crypt points directly at the provider's remote default path, without adding a folder. Provider lists show base storage rather than crypt wrappers; pool selection still uses encrypted destinations. Each base provider shows configuration status: encryption configured, setup pending, setting up, incomplete/retry, or unknown. This is not a live cloud-health or recoverability check.

## Encryption defaults

**Settings → Encryption defaults** controls password entropy (128/256/512/1024 bits), filename protection and directory-name protection. Click **Save encryption defaults** to persist across restarts. As with other GUI defaults, edits apply in the current session before saving. Automatic setup and newly opened custom-encryption dialogs use these defaults. Defaults apply only to new crypt remotes; existing keys, cipher settings and stored data are never changed. The entropy setting does not change rclone's cipher key size. Filename Off also exposes directory names; Obfuscate is not strong filename secrecy.

The equivalent CLI operation is `rpool provider ensure-encryption`. Keys remain in the local rclone configuration: back it up securely before uploading data. Automatic provisioning does not automatically add destinations to existing pools.

Automatic attempts wait for the current job to finish and do not loop on an unchanged failing catalog. Successful additions survive partial failures. Use **Retry automatic encryption** after resolving a failure; already-covered providers are skipped. Encrypted configuration files still require the official rclone configuration wizard.

## Custom encryption

### Physical folder location

New crypt backing locations use the configured path exactly:

`provider:<per-remote default path>`

For example, provider `server` with remote default `/data` creates a crypt pointing at `server:/data`. No `rpool` or unique child folder is added. Without a per-provider default, the backing is `server:`; `/` produces `server:/`. Leading `/`, spaces and Unicode are preserved. Historical encryption `root` settings and the hidden legacy `--root` option are accepted but ignored.

The separate **Global crypt folder fallback** is a plaintext path inside an already-created crypt remote; it is not the physical backing base. Changing a remote default does not relocate previously created crypts or rotate their keys. Existing crypts created at an unwanted location need a separate, explicit migration; this fix only changes new creation.

Select **Set up encryption**, choose a connected base provider and a unique encrypted-provider name. The dialog displays its backing location. RPool creates a new crypt remote pointing at that location; it does not convert existing files or rotate existing keys.

- Random password entropy: 128, 256, 512 or 1024 (default) bits. This changes generated password entropy, not rclone's cipher strength.
- Filename protection: Standard (recommended), Obfuscate (reversible, not strong filename secrecy), or Off.
- Directory-name protection can be disabled; filename Off also exposes directory names.
- Keys are generated with OS randomness and stored in the local rclone configuration, never in command arguments or job logs. Back up that configuration securely before storing data. rclone password obscuring is not encryption-at-rest of the config itself.
- Existing encrypted rclone configuration is not decrypted automatically. Use the official rclone wizard to create crypt remotes in that case.
- Close other config editors while provisioning. RPool uses a private lock, validates a staged config, detects intervening changes and atomically replaces the config. An unrelated editor that ignores the lock can still race the final replacement.

## Select providers for a pool

Storage → Pools → **Choose encrypted providers…** opens a picker. Click checkboxes or Remove, then **Apply selection** and **Save pool**. Cancel or close leaves the pool selection unchanged. Removing here only removes a destination from the pool draft; it never deletes a provider or cloud data. Custom paths and unavailable saved destinations remain until explicitly removed.

The picker has Refresh and Set up provider actions. Leaving for setup discards unapplied picker edits. New pools inherit Settings defaults; no provider is silently added. Newly created encrypted providers are discovered automatically after successful setup, including providers without capacity reporting.

## Validation scope

Automated tests run on macOS. Windows/Linux terminal launching, real cloud authentication and live cloud operations require platform/runtime validation. Synthetic subprocess tests are not real-rclone integration evidence.
