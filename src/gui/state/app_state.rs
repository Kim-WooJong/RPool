//! `GuiState`: all state of the GUI window, one field per page or form.
//! Built at startup by `state::persistence` and passed to every screen.
use super::selection::{FilesSection, MaintenanceSection, Page, StorageSection};
use crate::gui::screens::dashboard::DashboardData;
use crate::gui::screens::files::{InventoryForm, RestoreForm, StatusForm, UploadForm, VerifyForm};
use crate::gui::screens::jobs::JobsForm;
use crate::gui::screens::maintenance::{IntegrityForm, ManifestForm, SystemForm};
use crate::gui::screens::storage::{PoolForm, ProviderForm};
use crate::gui::settings::GuiSettings;
use crate::models::{PoolDefinition, QuotaReport};
use std::collections::BTreeMap;

/// Whole GUI state, owned by `gui::app` and passed mutably to the screens.
pub(crate) struct GuiState {
    /// Page shown in the main area.
    pub(crate) page: Page,
    /// Selected section of the Files page.
    pub(crate) files_section: FilesSection,
    /// Selected section of the Storage page.
    pub(crate) storage_section: StorageSection,
    /// Selected section of the Maintenance page.
    pub(crate) maintenance_section: MaintenanceSection,
    /// Settings saved between runs (`gui::settings`).
    pub(crate) settings: GuiSettings,
    /// Overview page data.
    pub(crate) dashboard: DashboardData,
    /// Files › Library (archive inventory and drive view).
    pub(crate) inventory: InventoryForm,
    /// Files › Upload form.
    pub(crate) upload: UploadForm,
    /// Files › Restore form.
    pub(crate) restore: RestoreForm,
    /// Files › Verify form.
    pub(crate) verify: VerifyForm,
    /// Files › Status form.
    pub(crate) status: StatusForm,
    /// Maintenance › Integrity form.
    pub(crate) integrity: IntegrityForm,
    /// Storage › Providers state.
    pub(crate) providers: ProviderForm,
    /// Storage › Pools editor.
    pub(crate) pools: PoolForm,
    /// Speed test cards (Pools and Providers).
    pub(crate) speed_test: crate::gui::screens::storage::speed_test::SpeedTestForm,
    /// Drive metadata card results.
    pub(crate) metadata: crate::gui::screens::storage::metadata_card::MetadataForm,
    /// Drive page settings and running mount sessions.
    pub(crate) mount: crate::gui::screens::storage::mount::MountForm,
    /// Monitoring page state.
    pub(crate) monitoring: crate::gui::screens::monitoring::MonitoringState,
    /// Settings › Portable configuration form.
    pub(crate) portable: crate::gui::screens::portable_config::PortableForm,
    /// Settings › Network form.
    pub(crate) network: crate::gui::screens::network_settings::NetworkForm,
    /// Account changes › Reprocess card.
    pub(crate) reprocess: crate::gui::screens::storage::reprocess::ReprocessForm,
    /// Account changes › pool change migration wizard.
    pub(crate) migration: crate::gui::screens::storage::migration::MigrationForm,
    /// Maintenance › Manifest form.
    pub(crate) manifest: ManifestForm,
    /// Maintenance › System form.
    pub(crate) system: SystemForm,
    /// Jobs page state.
    pub(crate) jobs: JobsForm,
    /// Last quota report per remote from the usage refresh.
    pub(crate) usage_reports: Vec<QuotaReport>,
    /// Discovered non-crypt (backing) remotes, one provider card each.
    pub(crate) backing_remotes: Vec<String>,
    /// Backend types and crypts per backing remote.
    pub(crate) provider_details: crate::gui::usage_refresh::ProviderDetails,
    /// Discovered crypt remotes (pool destinations).
    pub(crate) crypt_remotes: Vec<String>,
    /// Error of the last usage refresh.
    pub(crate) usage_error: Option<String>,
    /// Last Settings save or remote-root edit message.
    pub(crate) settings_notice: Option<String>,
    /// Per-remote default paths (remote name → path).
    pub(crate) remote_roots: BTreeMap<String, String>,
    /// Saved pool names, sorted.
    pub(crate) pool_names: Vec<String>,
    /// Saved pool definitions by name.
    pub(crate) pool_definitions: BTreeMap<String, PoolDefinition>,
}

impl GuiState {
    /// Initial state from the loaded settings, remote roots and pools.
    pub(crate) fn new(
        settings: GuiSettings,
        remote_roots: BTreeMap<String, String>,
        pool_definitions: BTreeMap<String, PoolDefinition>,
    ) -> Self {
        let pool_names: Vec<String> = pool_definitions.keys().cloned().collect();
        let pools = PoolForm::from_settings(&settings);
        let mount = crate::gui::screens::storage::mount::MountForm::from_settings(&settings);
        Self {
            page: Page::default(),
            files_section: FilesSection::default(),
            storage_section: StorageSection::default(),
            maintenance_section: MaintenanceSection::default(),
            dashboard: DashboardData::load(),
            settings,
            inventory: InventoryForm::default(),
            upload: UploadForm::for_pools(&pool_names),
            restore: RestoreForm::default(),
            verify: VerifyForm::default(),
            status: StatusForm::default(),
            integrity: IntegrityForm::default(),
            providers: ProviderForm::default(),
            reprocess: crate::gui::screens::storage::reprocess::ReprocessForm::default(),
            migration: Default::default(),
            pools,
            speed_test: Default::default(),
            metadata: Default::default(),
            mount,
            monitoring: Default::default(),
            portable: Default::default(),
            network: Default::default(),
            manifest: ManifestForm::default(),
            system: SystemForm::default(),
            jobs: JobsForm::default(),
            usage_reports: Vec::new(),
            crypt_remotes: Vec::new(),
            backing_remotes: Vec::new(),
            provider_details: Default::default(),
            usage_error: None,
            settings_notice: None,
            remote_roots,
            pool_names,
            pool_definitions,
        }
    }
}
