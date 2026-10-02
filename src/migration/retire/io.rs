//! Side-effect boundary of the cleanup: the cloud journal, the fresh
//! observation, deletion and the local inventory. Faked in tests.
use super::model::{RetireOptions, RetireRecord};
use super::plan::World;
use crate::migration::model::{Plan, Record};
use crate::prelude::*;

/// Everything `execute::retire` needs from the outside world; implemented by
/// `live::LiveIo` in production and by fakes in tests.
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
    /// Current Unix time in seconds.
    fn now(&self) -> u64 {
        crate::utils::now_unix()
    }
    /// Fresh random id for a new fossil (12 random bytes as hex).
    fn new_id(&self) -> Result<String> {
        crate::migration::execute::random_hex(12)
    }
    /// Identifier of this PC written into records.
    fn pc_id(&self) -> String {
        crate::migration::execute::pc_id()
    }
    /// Prints one progress/action line (stdout by default).
    fn say(&self, line: &str) {
        println!("{line}");
    }
}
