use crate::cli::{
    Cli, Commands, ConfigCommands, HistoryCommands, InventoryCommands, ManifestCommands,
    PoolCommands, ProviderCommands, RemoteRootCommands,
};
use crate::{commands, gui, history, pool};
use anyhow::Result;
use clap::Parser;

pub(crate) fn run() -> Result<()> {
    let cli = Cli::parse();

    let Some(command) = cli.command.as_ref() else {
        return gui::launch(&cli.rclone);
    };

    let pending = history::describe_command(command);
    let result = dispatch(cli);

    if let Some(pending) = pending {
        let record = history::finish_record(pending, &result);
        if let Err(error) = history::append_record(&record) {
            eprintln!("[history] failed to record operation: {error:#}");
        }
    }

    result
}

fn dispatch(cli: Cli) -> Result<()> {
    match cli
        .command
        .expect("CLI command must be present after default-GUI handling")
    {
        Commands::Gui => gui::launch(&cli.rclone),
        Commands::Mount(args) => crate::mount::run(&cli.rclone, args),
        Commands::Put {
            source,
            remotes,
            pool,
            shard_mib,
            workers,
            placement,
            retries,
            data_shards,
            parity_shards,
            id,
        } => {
            let resolved = pool::resolve_put_options(
                pool.as_deref(),
                remotes,
                shard_mib,
                workers,
                placement,
                retries,
                data_shards,
                parity_shards,
            )?;
            commands::put(
                &cli.rclone,
                &source,
                resolved.remotes,
                resolved.shard_mib,
                resolved.workers,
                resolved.placement,
                resolved.retries,
                resolved.data_shards,
                resolved.parity_shards,
                id,
                resolved.pool_name,
                resolved.native_crypt,
            )
        }
        Commands::Get {
            manifest,
            output,
            workers,
            retries,
        } => commands::get(&cli.rclone, &manifest, &output, workers, retries),
        Commands::Verify {
            manifest,
            full,
            workers,
        } => commands::verify(&cli.rclone, &manifest, full, workers),
        Commands::Status {
            manifest,
            workers,
            usage,
        } => commands::status(&cli.rclone, &manifest, workers, usage),
        Commands::Usage {
            remotes,
            pool,
            manifest,
            json,
            workers,
        } => commands::usage(
            &cli.rclone,
            remotes,
            pool.as_deref(),
            manifest.as_deref(),
            json,
            workers,
        ),
        Commands::Export(args) => commands::config_sync::export::run_package(&cli.rclone, &args),
        Commands::Import(args) => commands::config_sync::import::run_package(&cli.rclone, &args),
        Commands::Config(args) => match args.command {
            ConfigCommands::Paths => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "config_dir": crate::config::app_config_dir()?,
                        "gui": crate::config::gui_settings_path()?,
                        "pools": crate::config::pools_path()?,
                        "remote_roots": crate::config::remote_roots_path()?,
                        "portable_encryption_preferences": true
                    }))?
                );
                Ok(())
            }
            ConfigCommands::Export { output } => commands::config_sync::export::run(&output),
            ConfigCommands::Import { input, dry_run } => {
                commands::config_sync::import::run(&input, dry_run)
            }
        },
        Commands::Pool(args) => match args.command {
            PoolCommands::Capacity(args) => pool::capacity::run(&cli.rclone, args),
            PoolCommands::PlanReprocess {
                name,
                manifests,
                download_mib_s,
                upload_mib_s,
            } => {
                let target = pool::load_pool_store()?
                    .pools
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("pool not found: {name}"))?;
                let plan =
                    pool::build_plan(&cli.rclone, manifests, target, download_mib_s, upload_mib_s)?;
                println!("{}", serde_json::to_string_pretty(&plan)?);
                Ok(())
            }
            PoolCommands::Reprocess { plan } => pool::execute_plan(&cli.rclone, &plan),
            PoolCommands::List { json } => commands::pool::list(json),
            PoolCommands::Show { name, json } => commands::pool::show(&name, json),
            PoolCommands::Set {
                name,
                remotes,
                shard_mib,
                workers,
                retries,
                placement,
                data_shards,
                parity_shards,
                max_object_bytes,
                native_crypt,
            } => commands::pool::set(
                &cli.rclone,
                name,
                remotes,
                shard_mib,
                workers,
                retries,
                placement,
                data_shards,
                parity_shards,
                max_object_bytes,
                native_crypt,
            ),
            PoolCommands::Remove { name } => commands::pool::remove(&name),
        },
        Commands::Manifest(args) => match args.command {
            ManifestCommands::Replicate {
                manifest,
                remotes,
                pool,
                retries,
            } => commands::manifest_ops::replicate(
                &cli.rclone,
                &manifest,
                pool.as_deref(),
                remotes,
                retries,
            ),
            ManifestCommands::Verify {
                manifest,
                remotes,
                pool,
                json,
            } => commands::manifest_ops::verify(
                &cli.rclone,
                &manifest,
                pool.as_deref(),
                remotes,
                json,
            ),
            ManifestCommands::Recover {
                archive_id,
                remotes,
                pool,
                output,
            } => commands::manifest_ops::recover(
                &cli.rclone,
                &archive_id,
                pool.as_deref(),
                remotes,
                output,
            ),
        },
        Commands::Inventory(args) => match args.command {
            InventoryCommands::Add { manifest } => commands::inventory::add(&cli.rclone, &manifest),
            InventoryCommands::Rebuild { directory } => commands::inventory::rebuild(&directory),
            InventoryCommands::List { json } => commands::inventory::list(json),
            InventoryCommands::Find { pattern, json } => commands::inventory::find(&pattern, json),
            InventoryCommands::Info { archive_id, json } => {
                commands::inventory::info(&archive_id, json)
            }
        },
        Commands::History(args) => match args.command {
            HistoryCommands::List { limit, json } => commands::history::list(limit, json),
            HistoryCommands::Prune { keep } => commands::history::prune(keep),
        },
        Commands::Scrub(args) => commands::scrub(
            &cli.rclone,
            &args.manifest,
            args.quick,
            args.repair,
            args.dry_run,
            args.workers,
            args.retries,
            args.json,
        ),
        Commands::Repair(args) => commands::repair(
            &cli.rclone,
            &args.manifest,
            args.quick,
            args.workers,
            args.retries,
            args.dry_run,
            args.groups,
        ),
        Commands::Provider(args) => match args.command {
            ProviderCommands::EnsureEncryption {
                json,
                root,
                entropy_bits,
                filename_encryption,
                directory_encryption,
            } => {
                let report = crate::config_sync::provision::ensure_encryption(
                    std::path::Path::new(&cli.rclone),
                    &crate::config_sync::provision::EncryptionDefaults {
                        root,
                        entropy_bits,
                        filename_encryption,
                        directory_encryption,
                    },
                )?;
                if json {
                    println!("{}", serde_json::to_string(&report)?);
                } else {
                    println!(
                        "Encryption ready: {} created, {} already covered, {} failed.",
                        report.created.len(),
                        report.existing.len(),
                        report.failed.len()
                    );
                }
                if !report.failed.is_empty() {
                    anyhow::bail!("Some providers could not be encrypted. Successful additions were preserved; retry or use rclone config.");
                }
                Ok(())
            }
            ProviderCommands::Encrypt {
                name,
                provider,
                root: _,
                entropy_bits,
                filename_encryption,
                directory_encryption,
            } => {
                let backing = crate::config_sync::provision::create_crypt(
                    std::path::Path::new(&cli.rclone),
                    &crate::config_sync::provision::CryptSetup {
                        name,
                        provider,
                        entropy_bits,
                        filename_encryption,
                        directory_encryption,
                    },
                )?;
                println!("Encrypted provider created. Backing location: {backing}");
                println!("Back up the rclone configuration / encrypted secret vault before storing data. Losing these keys loses access to the data.");
                Ok(())
            }
            ProviderCommands::Health {
                remotes,
                pool,
                workers,
                json,
            } => commands::provider::health(&cli.rclone, remotes, pool.as_deref(), workers, json),
            ProviderCommands::Drain {
                manifest,
                from,
                to,
                output,
                workers,
                retries,
                dry_run,
                delete_source,
                allow_risky,
            } => commands::provider::drain(
                &cli.rclone,
                &manifest,
                &from,
                &to,
                output,
                workers,
                retries,
                dry_run,
                delete_source,
                allow_risky,
            ),
        },
        Commands::RemoteRoot(args) => match args.command {
            RemoteRootCommands::List { json } => commands::remote_root::list::run(json),
            RemoteRootCommands::Set { remote, path } => {
                commands::remote_root::set::run(&remote, &path)
            }
            RemoteRootCommands::Remove { remote } => commands::remote_root::remove::run(&remote),
        },
        Commands::Doctor(args) => commands::doctor(&cli.rclone, args.json, args.local_only),
    }
}
