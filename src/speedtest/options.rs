//! Test volume: total bytes per remote, how they are split into files, and
//! how many transfers run at once.
use crate::cli::pool::SpeedTestSizeArgs;
use crate::prelude::*;

pub(crate) const MIB: u64 = 1024 * 1024;
/// Largest total per remote (`--size-mib`).
pub(crate) const MAX_SIZE_MIB: u64 = 4096;
/// Most files per remote (`--files`).
pub(crate) const MAX_FILES: u64 = 4096;
/// Smallest test file; smaller files measure little but request overhead.
pub(crate) const MIN_FILE_BYTES: u64 = 4 * 1024;
/// Default file size when `--files` is not given.
pub(crate) const DEFAULT_FILE_BYTES: u64 = 4 * MIB;
/// Parallel transfers per remote without a pool.
pub(crate) const DEFAULT_PARALLEL: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestPlan {
    pub bytes_per_remote: u64,
    /// Size of every file; they add up to `bytes_per_remote`.
    pub file_sizes: Vec<u64>,
    pub parallel: usize,
    /// `--tune-uploads`: also find the best number of simultaneous uploads.
    pub tune_uploads: bool,
    /// `--tune-downloads`: also find the best number of simultaneous reads.
    pub tune_downloads: bool,
    /// Size of one tuning upload: the pool's shard size (default 64 MiB
    /// without a pool), so a count of uploads is a count of shards.
    pub shard_bytes: u64,
}

impl TestPlan {
    /// Validates `--size-mib` / `--files` and splits the total into files
    /// whose sizes differ by at most one byte. `workers` is the pool's worker
    /// count (None without a pool, giving the default of 4).
    pub(crate) fn new(args: &SpeedTestSizeArgs, workers: Option<usize>) -> Result<Self> {
        if !(1..=MAX_SIZE_MIB).contains(&args.size_mib) {
            bail!("--size-mib must be between 1 and {MAX_SIZE_MIB}");
        }
        let bytes = args.size_mib * MIB;
        let files = args
            .files
            .unwrap_or_else(|| bytes.div_ceil(DEFAULT_FILE_BYTES));
        if !(1..=MAX_FILES).contains(&files) {
            bail!("--files must be between 1 and {MAX_FILES}");
        }
        let base = bytes / files;
        if base < MIN_FILE_BYTES {
            bail!(
                "{files} files of {} MiB are {base} bytes each; each file must be at least {} KiB \
                 (use fewer files or a larger size)",
                args.size_mib,
                MIN_FILE_BYTES / 1024
            );
        }
        let extra = bytes % files;
        let file_sizes = (0..files).map(|i| base + u64::from(i < extra)).collect();
        let files = usize::try_from(files)?;
        let parallel = workers.unwrap_or(DEFAULT_PARALLEL).clamp(1, files);
        Ok(Self {
            bytes_per_remote: bytes,
            file_sizes,
            parallel,
            tune_uploads: args.tune_uploads,
            tune_downloads: args.tune_downloads,
            shard_bytes: crate::config::constants::DEFAULT_SHARD_MIB * MIB,
        })
    }

    pub(crate) fn files(&self) -> usize {
        self.file_sizes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(size_mib: u64, files: Option<u64>) -> SpeedTestSizeArgs {
        SpeedTestSizeArgs {
            size_mib,
            files,
            tune_uploads: false,
            tune_downloads: false,
            json: false,
        }
    }

    #[test]
    fn default_split_is_one_file_per_4_mib() {
        let plan = TestPlan::new(&args(16, None), None).unwrap();
        assert_eq!(plan.bytes_per_remote, 16 * MIB);
        assert_eq!(plan.file_sizes, vec![4 * MIB; 4]);
        assert_eq!(plan.parallel, 4);
        let plan = TestPlan::new(&args(1, None), None).unwrap();
        assert_eq!(plan.file_sizes, vec![MIB]);
        assert_eq!(plan.parallel, 1, "never more transfers than files");
        let plan = TestPlan::new(&args(10, None), Some(8)).unwrap();
        assert_eq!(plan.files(), 3);
        assert_eq!(plan.file_sizes.iter().sum::<u64>(), 10 * MIB);
        assert_eq!(plan.parallel, 3);
        let plan = TestPlan::new(&args(4096, None), Some(16)).unwrap();
        assert_eq!(plan.files(), 1024);
        assert_eq!(plan.parallel, 16);
    }

    #[test]
    fn explicit_file_counts_split_evenly() {
        let plan = TestPlan::new(&args(16, Some(1)), Some(4)).unwrap();
        assert_eq!(plan.file_sizes, vec![16 * MIB]);
        assert_eq!(plan.parallel, 1);
        let plan = TestPlan::new(&args(16, Some(256)), Some(6)).unwrap();
        assert_eq!(plan.file_sizes, vec![64 * 1024; 256]);
        assert_eq!(plan.parallel, 6);
        let plan = TestPlan::new(&args(1, Some(3)), None).unwrap();
        assert_eq!(plan.file_sizes.iter().sum::<u64>(), MIB);
        let (min, max) = (
            plan.file_sizes.iter().min().unwrap(),
            plan.file_sizes.iter().max().unwrap(),
        );
        assert!(max - min <= 1);
        let plan = TestPlan::new(&args(4096, Some(1)), None).unwrap();
        assert_eq!(plan.file_sizes, vec![4096 * MIB]);
        assert_eq!(
            TestPlan::new(&args(16, Some(4096)), None).unwrap().files(),
            4096
        );
    }

    #[test]
    fn invalid_sizes_and_tiny_files_are_refused() {
        for bad in [
            args(0, None),
            args(4097, None),
            args(16, Some(0)),
            args(16, Some(4097)),
        ] {
            assert!(TestPlan::new(&bad, None).is_err(), "{bad:?}");
        }
        // 1 MiB / 512 = 2 KiB per file.
        let error = TestPlan::new(&args(1, Some(512)), None).unwrap_err();
        assert!(error.to_string().contains("at least 4 KiB"), "{error}");
        // Exactly 4 KiB is fine.
        assert!(TestPlan::new(&args(1, Some(256)), None).is_ok());
        assert!(TestPlan::new(&args(1, Some(257)), None).is_err());
    }

    #[test]
    fn workers_zero_still_runs_one_transfer() {
        assert_eq!(TestPlan::new(&args(16, None), Some(0)).unwrap().parallel, 1);
    }
}
