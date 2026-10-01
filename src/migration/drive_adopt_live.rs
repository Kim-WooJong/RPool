//! Real side effects of an adoption (`drive_adopt::AdoptIo`) over rclone:
//! the cloud drive source, the new epoch's replicas, manifest reads, and the
//! catch-up run of files changed since planning.
use super::drive_adopt::AdoptIo;
use super::drive_model::DriveFile;
use super::drive_plan::{classify, extend_listings};
use super::drive_run::{as_entry, by_key, run_entries};
use super::drive_source::{CloudDrive, DriveSource};
use super::execute::{RunOptions, RunSummary};
use super::journal::Journal;
use super::model::{Action, Plan};
use super::plan::{list_all, PlanOptions, RcloneCloud};
use crate::mount::drive_generation_write::{CloudSink, Sink};
use crate::prelude::*;

pub(crate) struct LiveAdopt<'a> {
    rclone: &'a str,
    journal: &'a Journal,
    plan: &'a Plan,
    options: &'a RunOptions,
    source: CloudDrive,
}

impl<'a> LiveAdopt<'a> {
    pub(crate) fn new(
        rclone: &'a str,
        journal: &'a Journal,
        plan: &'a Plan,
        options: &'a RunOptions,
    ) -> Self {
        Self {
            rclone,
            journal,
            plan,
            options,
            source: CloudDrive::new(rclone, &plan.pool, &plan.target),
        }
    }
}

impl AdoptIo for LiveAdopt<'_> {
    fn source(&self) -> &dyn DriveSource {
        &self.source
    }
    fn sink(&self, epoch: &str) -> Result<Box<dyn Sink + '_>> {
        Ok(Box::new(CloudSink::new(
            self.rclone,
            &self.plan.pool,
            &self.plan.target,
            epoch,
        )?))
    }
    fn load_manifest(&self, location: &str) -> Result<Manifest> {
        crate::manifest::load_manifest(self.rclone, location)
    }
    fn catch_up(&self, files: &[DriveFile]) -> Result<(RunSummary, BTreeSet<String>)> {
        let target = &self.plan.target;
        let cloud = RcloneCloud::new(self.rclone, target.placement == Placement::Resilient);
        let target_set: BTreeSet<String> = target.remotes.iter().cloned().collect();
        let configured = super::enumerate::Cloud::configured(&cloud);
        let mut listings = list_all(&cloud, &target_set, configured.as_ref());
        extend_listings(&cloud, &mut listings, files);
        let workers = target.workers.max(1);
        let options = PlanOptions {
            workers,
            ..Default::default()
        };
        let classified = classify(
            &cloud,
            target,
            &target_set,
            &listings,
            &options,
            workers,
            files,
        )?;
        let kept = classified
            .iter()
            .filter(|(e, _)| e.action == Action::Unaffected)
            .map(|(e, _)| e.key.clone())
            .collect();
        let entries = classified
            .iter()
            .filter(|(e, _)| e.action != Action::Unaffected)
            .map(|(e, _)| as_entry(e))
            .collect();
        let summary = run_entries(
            self.rclone,
            self.journal,
            self.plan,
            entries,
            by_key(files.to_vec()),
            self.options,
            &super::execute::pc_id(),
        )?;
        Ok((summary, kept))
    }
    fn wait(&self, seconds: u64) -> Result<()> {
        for _ in 0..seconds {
            if self.options.stop_file.as_deref().is_some_and(Path::exists) {
                bail!("adoption stopped by the stop file; run it again to continue");
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        Ok(())
    }
}
