use crate::manifest::{load_manifest, validate_manifest};
use crate::prelude::*;
use crate::provider::drain_manifest;
use crate::remote_root::apply_remote_root;
use crate::utils::append_suffix;

#[allow(clippy::too_many_arguments)]
pub(crate) fn run(
    rclone: &str,
    manifest_src: &str,
    from: &str,
    to: &str,
    output: Option<PathBuf>,
    workers: usize,
    retries: u32,
    dry_run: bool,
    delete_source: bool,
    allow_risky: bool,
) -> Result<()> {
    if workers == 0 {
        bail!("workers must be greater than zero");
    }
    let manifest = load_manifest(rclone, manifest_src)?;
    validate_manifest(&manifest)?;
    let from = apply_remote_root(from)?;
    let to = apply_remote_root(to)?;

    let source_path = Path::new(manifest_src);
    let output = match output {
        Some(path) => path,
        None if source_path.exists() => source_path.to_path_buf(),
        None if dry_run => append_suffix(Path::new(&manifest.original_name), ".drained.rpool.json"),
        None => bail!("--output is required when draining a manifest loaded from a remote path"),
    };

    let updated = drain_manifest(
        rclone,
        &manifest,
        &from,
        &to,
        &output,
        workers,
        retries,
        dry_run,
        delete_source,
        allow_risky,
    )?;

    if dry_run {
        println!("status=dry-run");
        return Ok(());
    }

    let source = output.to_string_lossy().into_owned();
    if let Err(error) = crate::inventory::add_manifest(rclone, &source) {
        eprintln!("[inventory] provider drain completed but index update failed: {error:#}");
    }
    println!("archive_id={}", updated.archive_id);
    println!("manifest={}", output.display());
    println!("status=drained");
    Ok(())
}
