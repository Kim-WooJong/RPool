use crate::cli::{
    Commands, ConfigCommands, InventoryCommands, ManifestCommands, PoolCommands, ProviderCommands,
    RemoteRootCommands,
};
use crate::history::redact_text;
use crate::models::TaskRecord;
use crate::utils::now_unix;

#[derive(Debug, Clone)]
pub(crate) struct PendingTaskRecord {
    pub(crate) id: String,
    pub(crate) operation: String,
    pub(crate) target: Option<String>,
    pub(crate) started_unix: u64,
}

pub(crate) fn describe_command(command: &Commands) -> Option<PendingTaskRecord> {
    let (operation, target) = match command {
        Commands::Gui | Commands::History(_) => return None,
        Commands::Mount(args) => ("mount-workspace".into(), Some(args.pool.clone())),
        Commands::Put { source, pool, .. } => (
            "put".to_string(),
            Some(match pool {
                Some(pool) => format!("{} via pool {}", source.display(), pool),
                None => source.display().to_string(),
            }),
        ),
        Commands::Get { output, .. } => ("get".to_string(), Some(output.display().to_string())),
        Commands::Verify { manifest, .. } => ("verify".to_string(), Some(manifest.clone())),
        Commands::Status { manifest, .. } => ("status".to_string(), Some(manifest.clone())),
        Commands::Usage { .. } => ("usage".to_string(), None),
        Commands::Export(args) => (
            "portable-export".to_string(),
            Some(args.artifact_root.display().to_string()),
        ),
        Commands::Import(args) => (
            if args.dry_run {
                "portable-import-dry-run"
            } else {
                "portable-import"
            }
            .to_string(),
            Some(args.artifact_root.display().to_string()),
        ),
        Commands::Config(args) => match &args.command {
            ConfigCommands::Paths => return None,
            ConfigCommands::Export { output } => (
                "config-export".to_string(),
                Some(output.display().to_string()),
            ),
            ConfigCommands::Import { input, dry_run } => (
                if *dry_run {
                    "config-import-dry-run"
                } else {
                    "config-import"
                }
                .to_string(),
                Some(input.display().to_string()),
            ),
        },
        Commands::Pool(args) => match &args.command {
            PoolCommands::Capacity(args) => ("pool-capacity".into(), args.name.clone()),
            PoolCommands::PlanReprocess { name, .. } => {
                ("pool-plan-reprocess".to_string(), Some(name.clone()))
            }
            PoolCommands::Reprocess { plan } => (
                "pool-reprocess".to_string(),
                Some(plan.display().to_string()),
            ),
            PoolCommands::List { .. } => ("pool-list".to_string(), None),
            PoolCommands::Browse { name, .. } => ("pool-browse".to_string(), Some(name.clone())),
            PoolCommands::Show { name, .. } => ("pool-show".to_string(), Some(name.clone())),
            PoolCommands::Set { name, .. } => ("pool-set".to_string(), Some(name.clone())),
            PoolCommands::Remove { name } => ("pool-remove".to_string(), Some(name.clone())),
            PoolCommands::Migrate(args) => crate::commands::pool::migrate_history(args),
        },
        Commands::Manifest(args) => match &args.command {
            ManifestCommands::Replicate { manifest, .. } => {
                ("manifest-replicate".to_string(), Some(manifest.clone()))
            }
            ManifestCommands::Verify { manifest, .. } => {
                ("manifest-verify".to_string(), Some(manifest.clone()))
            }
            ManifestCommands::Recover { archive_id, .. } => {
                ("manifest-recover".to_string(), Some(archive_id.clone()))
            }
        },
        Commands::Inventory(args) => match &args.command {
            InventoryCommands::Add { manifest } => {
                ("inventory-add".to_string(), Some(manifest.clone()))
            }
            InventoryCommands::Rebuild { directory } => (
                "inventory-rebuild".to_string(),
                Some(directory.display().to_string()),
            ),
            InventoryCommands::List { .. } => ("inventory-list".to_string(), None),
            InventoryCommands::Find { pattern, .. } => {
                ("inventory-find".to_string(), Some(pattern.clone()))
            }
            InventoryCommands::Info { archive_id, .. } => {
                ("inventory-info".to_string(), Some(archive_id.clone()))
            }
        },
        Commands::Scrub(args) => ("scrub".to_string(), Some(args.manifest.clone())),
        Commands::Repair(args) => ("repair".to_string(), Some(args.manifest.clone())),
        Commands::Provider(args) => match &args.command {
            ProviderCommands::EnsureEncryption { .. } => {
                ("provider-ensure-encryption".to_string(), None)
            }
            ProviderCommands::Encrypt { name, .. } => {
                ("provider-encrypt".to_string(), Some(name.clone()))
            }
            ProviderCommands::Health { pool, .. } => ("provider-health".to_string(), pool.clone()),
            ProviderCommands::Drain {
                manifest, from, to, ..
            } => (
                "provider-drain".to_string(),
                Some(format!("{manifest}: {from} -> {to}")),
            ),
        },
        Commands::RemoteRoot(args) => match &args.command {
            RemoteRootCommands::List { .. } => ("remote-root-list".to_string(), None),
            RemoteRootCommands::Set { remote, path } => (
                "remote-root-set".to_string(),
                Some(format!("{remote} -> {path}")),
            ),
            RemoteRootCommands::Remove { remote } => {
                ("remote-root-remove".to_string(), Some(remote.clone()))
            }
        },
        Commands::Doctor(_) => ("doctor".to_string(), None),
    };

    let target = target.map(|value| redact_text(&value, 300));
    let started_unix = now_unix();
    let seed = format!(
        "{}\0{}\0{}\0{}",
        operation,
        target.as_deref().unwrap_or(""),
        started_unix,
        std::process::id()
    );
    let hash = blake3::hash(seed.as_bytes()).to_hex().to_string();
    Some(PendingTaskRecord {
        id: hash[..16].to_string(),
        operation,
        target,
        started_unix,
    })
}

pub(crate) fn finish_record(pending: PendingTaskRecord, result: &anyhow::Result<()>) -> TaskRecord {
    TaskRecord {
        id: pending.id,
        operation: pending.operation,
        target: pending.target,
        started_unix: pending.started_unix,
        finished_unix: now_unix(),
        status: if result.is_ok() { "success" } else { "failed" }.to_string(),
        message: result
            .as_ref()
            .err()
            .map(|error| redact_text(&format!("{error:#}"), 500)),
    }
}
