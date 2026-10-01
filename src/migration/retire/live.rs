//! Production side effects of the cleanup: the migration's cloud journal
//! (`retire/` records next to `records/`), live observation, object
//! deletion through the pool's storage writer, and the local inventory.
use super::io::RetireIo;
use super::model::{RetireOptions, RetireRecord};
use super::plan::World;
use crate::migration::journal::{Journal, RETIRE};
use crate::migration::model::{Plan, Record};
use crate::prelude::*;
use crate::storage::error::{StorageError, StorageErrorKind};
use crate::storage::writer::StorageWriter;

pub(crate) struct LiveIo {
    rclone: String,
    pool: String,
    migration_id: String,
    journal: Journal,
    native_crypt: bool,
}

impl LiveIo {
    pub(crate) fn open(rclone: &str, pool: &str, migration_id: &str) -> Result<Self> {
        let native_crypt = crate::pool::load_pool_store()?
            .pools
            .get(pool)
            .with_context(|| format!("pool {pool} is not configured"))?
            .native_crypt;
        Ok(Self {
            rclone: rclone.into(),
            pool: pool.into(),
            migration_id: migration_id.into(),
            journal: Journal::open(rclone, pool, migration_id)?,
            native_crypt,
        })
    }
}

impl RetireIo for LiveIo {
    fn plan(&self) -> Result<Plan> {
        let plan = self
            .journal
            .load_plan()?
            .ok_or_else(|| anyhow!("migration not found in the cloud: {}", self.migration_id))?;
        if plan.migration_id != self.migration_id || plan.pool != self.pool {
            bail!(
                "journal plan does not belong to {}/{}",
                self.pool,
                self.migration_id
            );
        }
        Ok(plan)
    }
    fn records(&self) -> Result<Vec<Record>> {
        self.journal.records()
    }
    fn retire_records(&self) -> Result<Vec<RetireRecord>> {
        self.journal.records_in(RETIRE)
    }
    fn append(&self, record: &RetireRecord) -> Result<()> {
        self.journal.append_in(RETIRE, record)
    }
    fn observe(&self, plan: &Plan, records: &[Record], options: &RetireOptions) -> Result<World> {
        super::observe::observe(&self.rclone, &self.pool, plan, records, options)
    }
    fn delete(&self, address: &str) -> Result<()> {
        let storage = StorageWriter::for_pool(&self.rclone, self.native_crypt);
        storage.ensure_destination(address)?;
        match storage.delete(address) {
            Ok(()) => Ok(()),
            Err(error)
                if error
                    .downcast_ref::<StorageError>()
                    .is_some_and(|e| e.kind() == StorageErrorKind::NotFound) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
    fn forget(&self, archive_id: &str) -> Result<()> {
        crate::inventory::remove_entry(archive_id).map(|_| ())
    }
}
