//! Process entry point after `main`: parses the command line and dispatches each
//! `rpool` subcommand to its implementation in `commands`, `pool`, `mount`, etc.
//! Running without a subcommand opens the GUI. `main.rs` calls `run`.
use crate::cli::{
    Cli, Commands, ConfigCommands, HistoryCommands, InventoryCommands, ManifestCommands,
    PoolCommands, ProviderCommands, RemoteRootCommands,
};
use crate::{commands, gui, history, pool};
use anyhow::Result;
use clap::Parser;

/// Parses `argv` (the `mount monitor` form first, then the regular [`Cli`]),
/// launches the GUI when no subcommand is given, otherwise runs [`dispatch`] and
/// appends a history record for commands that `history::describe_command` tracks.
/// Called once from `main`.
pub(crate) fn run() -> Result<()> {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if let Some(parsed) = crate::cli::parse_monitor(&argv) {
        return crate::monitor::command::run(&parsed.unwrap_or_else(|error| error.exit()));
    }
    let cli = Cli::parse_from(argv);

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

/// Routes a parsed subcommand to its handler, resolving pool defaults for `put`
/// and printing small JSON/text reports inline for the provider/config commands.
/// Panics only if called without a command (the GUI case is handled by [`run`]).
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
            PoolCommands::Browse { name, json } => commands::pool::browse(&cli.rclone, &name, json),
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
                small_file_packing,
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
                small_file_packing,
            ),
            PoolCommands::Remove { name } => commands::pool::remove(&name),
            PoolCommands::Migrate(args) => commands::pool::migrate(&cli.rclone, args),
            PoolCommands::SpeedTest { name, size } => {
                crate::speedtest::run_pool(&cli.rclone, &name, size)
            }
            PoolCommands::Compact {
                name,
                dry_run,
                enable_deletion,
                json,
            } => commands::pool::compact(&cli.rclone, &name, dry_run, enable_deletion, json),
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
        Commands::Drive(args) => crate::drive_history::command::run(&cli.rclone, args),
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
            ProviderCommands::Limits(limits) => {
                commands::provider::limits(&cli.rclone, limits.command)
            }
            ProviderCommands::Keepalive { remotes, json } => {
                commands::provider::keepalive(&cli.rclone, remotes, json)
            }
            ProviderCommands::EnsureEncryption {
                json,
                entropy_bits,
                filename_encryption,
                directory_encryption,
                filename_encoding,
            } => {
                let report = crate::config_sync::provision::ensure_encryption(
                    std::path::Path::new(&cli.rclone),
                    &crate::config_sync::provision::EncryptionDefaults {
                        entropy_bits,
                        filename_encryption,
                        directory_encryption,
                        filename_encoding,
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
                entropy_bits,
                filename_encryption,
                directory_encryption,
                filename_encoding,
            } => {
                let backing = crate::config_sync::provision::create_crypt(
                    std::path::Path::new(&cli.rclone),
                    &crate::config_sync::provision::CryptSetup {
                        name,
                        provider,
                        entropy_bits,
                        filename_encryption,
                        directory_encryption,
                        filename_encoding,
                    },
                )?;
                println!("Encrypted provider created. Backing location: {backing}");
                println!("Back up the rclone configuration / encrypted secret vault before storing data. Losing these keys loses access to the data.");
                Ok(())
            }
            ProviderCommands::NameEncoding {
                remote,
                encoding,
                existing_files_ok,
                json,
            } => {
                let change = crate::config_sync::provision::set_name_encoding(
                    std::path::Path::new(&cli.rclone),
                    remote.trim_end_matches(':'),
                    &encoding,
                    existing_files_ok,
                )?;
                if json {
                    println!("{}", serde_json::to_string(&change)?);
                } else if change.changed {
                    println!(
                        "{}: names now use {} (were {}). {} file(s) already under its folder; those written with another encoding are not listed until you switch back.",
                        change.remote, change.to, change.from, change.existing_files
                    );
                } else {
                    println!("{}: names already use {}.", change.remote, change.to);
                }
                Ok(())
            }
            ProviderCommands::Location {
                remote,
                path,
                move_existing,
                json,
            } => {
                let change = crate::config_sync::relocate::set_provider_location(
                    std::path::Path::new(&cli.rclone),
                    &remote,
                    &path,
                    move_existing,
                )?;
                if json {
                    println!("{}", serde_json::to_string(&change)?);
                } else if !change.changed {
                    println!("{}: location is already {}.", change.provider, change.to);
                } else {
                    println!("{}: location {} -> {}", change.provider, change.from, change.to);
                    if change.crypts.is_empty() {
                        println!("No crypt remote used the old folder.");
                    } else {
                        println!("Crypt remotes now at the new folder: {}", change.crypts.join(", "));
                    }
                    if change.moved_files > 0 {
                        println!("Moved {} file(s), {} bytes.", change.moved_files, change.moved_bytes);
                    }
                    if !change.skipped.is_empty() {
                        println!("Left unchanged (another folder): {}", change.skipped.join(", "));
                    }
                }
                Ok(())
            }
            ProviderCommands::Health {
                remotes,
                pool,
                workers,
                json,
            } => commands::provider::health(&cli.rclone, remotes, pool.as_deref(), workers, json),
            ProviderCommands::SpeedTest { remotes, size } => {
                crate::speedtest::run_remotes(&cli.rclone, remotes, size)
            }
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
        Commands::Doctor(args) => commands::doctor(
            &cli.rclone,
            args.json,
            args.local_only,
            args.bundle.as_deref(),
        ),
    }
}
