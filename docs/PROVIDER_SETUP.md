# Provider connection and encryption (r6)

## Connect a cloud

Storage → Providers → **+ Connect cloud provider** opens the official `rclone config` wizard in a terminal. Choose New remote, select the service, finish its login, quit the wizard, then click **Refresh providers**. RPool does not collect cloud passwords or OAuth tokens. This is a terminal/browser flow, not an embedded cloud-login form.

## Add encryption

Select **Set up encryption**, choose a connected non-crypt provider and a unique encrypted-provider name. Optionally choose a parent folder. RPool allocates a fresh child folder and creates a new crypt remote; it does not convert existing files or rotate existing keys.

- Random password entropy: 128, 256 (default), 512 or 1024 bits. This changes generated password entropy, not rclone's cipher strength.
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
