//! Side-effect boundary of the cleanup: the cloud journal, the fresh
//! observation, deletion and the local inventory. Faked in tests.
use super::model::{RetireOptions, RetireRecord};
use super::plan::World;
use crate::migration::model::{Plan, Record};
use crate::prelude::*;

pub(crate) trait RetireIo {
    /// The frozen migration plan.
    fn plan(&self) -> Result<Plan>;
    /// Migration records (`records/`), fresh from the cloud.
    fn records(&self) -> Result<Vec<Record>>;
    /// Cleanup records (`retire/`), fresh from the cloud.
    fn retire_records(&self) -> Result<Vec<RetireRecord>>;
    /// Appends one cleanup record to the cloud journal.
    fn append(&self, record: &RetireRecord) -> Result<()>;
    /// Fresh listings, manifests and references (read-only).
    fn observe(&self, plan: &Plan, records: &[Record], options: &RetireOptions) -> Result<World>;
    /// Deletes one object; an object that is already gone is not an error.
    fn delete(&self, address: &str) -> Result<()>;
    /// Drops `archive_id` from this PC's inventory (when indexed).
    fn forget(&self, archive_id: &str) -> Result<()>;
    fn now(&self) -> u64 {
        crate::utils::now_unix()
    }
    fn new_id(&self) -> Result<String> {
        crate::migration::execute::random_hex(12)
    }
    fn pc_id(&self) -> String {
        crate::migration::execute::pc_id()
    }
    fn say(&self, line: &str) {
        println!("{line}");
    }
}
