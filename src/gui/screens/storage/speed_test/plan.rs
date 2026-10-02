//! What a speed test writes (size and file count per account) and the
//! `rpool … speed-test` command line for it.
use std::ffi::OsString;

/// Bytes per MiB.
pub(crate) const MIB: u64 = 1024 * 1024;
/// Limits shared with the CLI (`--size-mib`, `--files`).
pub(crate) const MAX_SIZE_MIB: u64 = 4096;
/// Most files per account (`--files`), same limit as the CLI.
pub(crate) const MAX_FILES: usize = 4096;
/// Smallest allowed file; plans whose files would be smaller are rejected.
pub(crate) const MIN_FILE_BYTES: u64 = 4 * 1024;
/// File size of the "Many small files" preset.
const SMALL_FILE_BYTES: u64 = 64 * 1024;
/// Size choices of the "Large file" preset, MiB per account.
pub(crate) const LARGE_SIZES_MIB: [u64; 4] = [256, 1024, 2048, 4096];
/// File count choices of the "Many small files" preset.
pub(crate) const SMALL_COUNTS: [usize; 3] = [64, 256, 1024];
/// From this size per account a run needs an extra confirmation click.
pub(crate) const CONFIRM_FROM_MIB: u64 = 1024;
/// From this size per account the duration warning is shown.
pub(crate) const WARN_FROM_MIB: u64 = 256;

/// Size preset of the speed test card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Preset {
    /// 4 files, 16 MiB per account: finds a slow account quickly.
    #[default]
    Quick,
    /// One file of a chosen size per account.
    LargeFile,
    /// Many 64 KiB files per account (round-trip bound).
    ManySmall,
    /// User-chosen size and file count.
    Custom,
}

/// Per account: `files` files adding up to `size_mib` MiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Total size written per account, MiB.
    pub(crate) size_mib: u64,
    /// Number of files the size is split into.
    pub(crate) files: usize,
}

/// Why a plan is invalid; shown by `plan_error_text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanError {
    /// Size outside 1..=`MAX_SIZE_MIB`.
    Size,
    /// File count outside 1..=`MAX_FILES`.
    Files,
    /// Each file would be smaller than `MIN_FILE_BYTES`.
    FileTooSmall,
}

impl Plan {
    /// Total bytes per account.
    pub(crate) fn bytes(self) -> u64 {
        self.size_mib * MIB
    }

    /// Bytes per file (the total split evenly).
    pub(crate) fn file_bytes(self) -> u64 {
        self.bytes() / self.files.max(1) as u64
    }

    /// The plan if its size, file count and per-file size are within limits.
    pub(crate) fn validate(self) -> Result<Self, PlanError> {
        if !(1..=MAX_SIZE_MIB).contains(&self.size_mib) {
            return Err(PlanError::Size);
        }
        if !(1..=MAX_FILES).contains(&self.files) {
            return Err(PlanError::Files);
        }
        if self.file_bytes() < MIN_FILE_BYTES {
            return Err(PlanError::FileTooSmall);
        }
        Ok(self)
    }

    /// Whether the run needs the extra "Start large test" click.
    pub(crate) fn needs_confirmation(self) -> bool {
        self.size_mib >= CONFIRM_FROM_MIB
    }

    /// Whether the duration warning is shown.
    pub(crate) fn is_large(self) -> bool {
        self.size_mib >= WARN_FROM_MIB
    }

    /// "1 x 4 GiB", "256 x 64 KiB".
    pub(crate) fn mode_label(self) -> String {
        mode_label(self.files, self.bytes())
    }
}

/// The plan of a preset; `large_mib` / `small_count` are the pickers of the
/// Large / Many small presets, `custom` the Custom fields.
pub(crate) fn preset_plan(
    preset: Preset,
    large_mib: u64,
    small_count: usize,
    custom: Plan,
) -> Plan {
    match preset {
        Preset::Quick => Plan {
            size_mib: 16,
            files: 4,
        },
        Preset::LargeFile => Plan {
            size_mib: large_mib,
            files: 1,
        },
        Preset::ManySmall => Plan {
            size_mib: (small_count as u64 * SMALL_FILE_BYTES).div_ceil(MIB),
            files: small_count,
        },
        Preset::Custom => custom,
    }
}

/// `{files} x {size of one file}`.
pub(crate) fn mode_label(files: usize, total_bytes: u64) -> String {
    let files = files.max(1);
    format!("{files} x {}", binary_size(total_bytes / files as u64))
}

/// Exact binary sizes without trailing zeros: "64 KiB", "4 GiB", "1.5 MiB".
pub(crate) fn binary_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let text = format!("{value:.1}");
    let text = text.strip_suffix(".0").unwrap_or(&text);
    format!("{text} {}", UNITS[unit])
}

/// `--size-mib N --files N --json`, shared by pool and provider runs.
fn size_args(plan: Plan) -> [OsString; 5] {
    [
        "--size-mib".into(),
        plan.size_mib.to_string().into(),
        "--files".into(),
        plan.files.to_string().into(),
        "--json".into(),
    ]
}

/// `pool speed-test --size-mib N --files N --json -- <NAME>`.
pub(crate) fn pool_args(pool: &str, plan: Plan) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["pool".into(), "speed-test".into()];
    args.extend(size_args(plan));
    args.extend(["--".into(), pool.into()]);
    args
}

/// `provider speed-test --remote=R … --size-mib N --files N --json`.
pub(crate) fn provider_args(remotes: &[String], plan: Plan) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["provider".into(), "speed-test".into()];
    // `--remote=R` keeps a remote that starts with '-' a value.
    args.extend(remotes.iter().map(|r| format!("--remote={r}").into()));
    args.extend(size_args(plan));
    args
}
