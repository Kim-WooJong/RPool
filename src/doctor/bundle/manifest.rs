//! `manifest.txt`: what the bundle contains, what it leaves out, and how it
//! was redacted.
use super::collect::{
    HISTORY_TAIL_BYTES, MOUNT_LOG_BYTES, OPERATION_HISTORY_BYTES, PREVIOUS_LOG_BYTES,
    TOTAL_LOG_BYTES,
};
use super::redact::RULES;
use super::Collected;

/// File name of the manifest inside the bundle (written first).
pub(crate) const NAME: &str = "manifest.txt";

/// Never put in a bundle, whatever the configuration.
const NEVER_INCLUDED: &[&str] = &[
    "rclone.conf itself (only `rclone config redacted` output, redacted again)",
    "crypt passwords and salts, OAuth tokens, API keys, rc credentials",
    ".env files, age identities, portable configuration packages",
    "inventory.json (file names), migrations/ and reprocess/ work folders",
    "file contents: spool, VFS cache and shard data",
    "speed test results: RPool does not store them (run a speed test again and copy its report)",
];

/// Render `manifest.txt`: included files with sizes, skipped files, the
/// never-included list, the size limits and the redaction rules.
pub(crate) fn render(collected: &Collected, now_unix: u64) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "RPool diagnostics bundle\nrpool {}\nexported_unix: {now_unix}\n\n",
        env!("CARGO_PKG_VERSION")
    ));
    out.push_str("Included files:\n");
    for entry in &collected.entries {
        out.push_str(&format!(
            "  {}  ({} bytes)  {}\n",
            entry.name,
            entry.bytes.len(),
            entry.note
        ));
    }
    if !collected.skipped.is_empty() {
        out.push_str("\nNot included:\n");
        for skipped in &collected.skipped {
            out.push_str(&format!("  {skipped}\n"));
        }
    }
    out.push_str("\nNever included:\n");
    for item in NEVER_INCLUDED {
        out.push_str(&format!("  {item}\n"));
    }
    out.push_str(&format!(
        "\nLimits: mount log last {} MiB, previous mount log last {} MiB, network history last {} KiB, operation history last {} KiB, {} MiB of logs in total.\n",
        MOUNT_LOG_BYTES >> 20,
        PREVIOUS_LOG_BYTES >> 20,
        HISTORY_TAIL_BYTES >> 10,
        OPERATION_HISTORY_BYTES >> 10,
        TOTAL_LOG_BYTES >> 20
    ));
    out.push_str("\nRedaction (applied to every file, after rclone's own redaction):\n");
    for rule in RULES {
        out.push_str(&format!("  - {rule}\n"));
    }
    out.push_str("\nReview the files before sharing them; the rules remove too much rather than too little, but cannot know every secret format.\n");
    out
}
