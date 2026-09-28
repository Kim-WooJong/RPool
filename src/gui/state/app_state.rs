use super::selection::{FilesSection, MaintenanceSection, Page, StorageSection};
use crate::gui::screens::dashboard::DashboardData;
use crate::gui::screens::files::{InventoryForm, RestoreForm, StatusForm, UploadForm, VerifyForm};
use crate::gui::screens::jobs::JobsForm;
use crate::gui::screens::maintenance::{IntegrityForm, ManifestForm, SystemForm};
use crate::gui::screens::storage::{PoolForm, ProviderForm};
use crate::gui::settings::GuiSettings;
use crate::models::{PoolDefinition, QuotaReport};
use std::collections::BTreeMap;

pub(crate) struct GuiState {
    pub(crate) page: Page,
    pub(crate) files_section: FilesSection,
    pub(crate) storage_section: StorageSection,
    pub(crate) maintenance_section: MaintenanceSection,
    pub(crate) settings: GuiSettings,
    pub(crate) dashboard: DashboardData,
    pub(crate) inventory: InventoryForm,
    pub(crate) upload: UploadForm,
    pub(crate) restore: RestoreForm,
    pub(crate) verify: VerifyForm,
    pub(crate) status: StatusForm,
    pub(crate) integrity: IntegrityForm,
    pub(crate) providers: ProviderForm,
    pub(crate) pools: PoolForm,
    pub(crate) reprocess: crate::gui::screens::storage::reprocess::ReprocessForm,
    pub(crate) manifest: ManifestForm,
    pub(crate) system: SystemForm,
    pub(crate) jobs: JobsForm,
    pub(crate) usage_reports: Vec<QuotaReport>,
    pub(crate) crypt_remotes: Vec<String>,
    pub(crate) usage_error: Option<String>,
    pub(crate) settings_notice: Option<String>,
    pub(crate) remote_roots: BTreeMap<String, String>,
    pub(crate) remote_root_name: String,
    pub(crate) remote_root_path: String,
    pub(crate) pool_names: Vec<String>,
    pub(crate) pool_definitions: BTreeMap<String, PoolDefinition>,
}

impl GuiState {
    pub(crate) fn new(
        settings: GuiSettings,
        remote_roots: BTreeMap<String, String>,
        pool_definitions: BTreeMap<String, PoolDefinition>,
    ) -> Self {
        let pool_names: Vec<String> = pool_definitions.keys().cloned().collect();
        let pools = PoolForm::from_settings(&settings);
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
            pools,
            manifest: ManifestForm::default(),
            system: SystemForm::default(),
            jobs: JobsForm::default(),
            usage_reports: Vec::new(),
            crypt_remotes: Vec::new(),
            usage_error: None,
            settings_notice: None,
            remote_roots,
            remote_root_name: String::new(),
            remote_root_path: String::new(),
            pool_names,
            pool_definitions,
        }
    }
}
