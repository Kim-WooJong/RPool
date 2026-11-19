//! Abandoned-copy mode, intentionally separate from completed-original retire.
use super::state::Step;
use crate::gui::{i18n::{tr,trf},state::GuiState,task::TaskRunner,theme};
use crate::migration::discard::{Options,Report};
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver,TryRecvError};

/// Separate task name: never confused with completed-original cleanup.
const DELETE_TASK:&str="Abandoned migration new-copy deletion";

/// Preview and explicit destructive confirmation, scoped to an exact job/input.
#[derive(Debug,Default)]
pub(crate) struct Form {
    pub workspace_lines:String,
    pub report:Option<Report>,
    pub error:Option<String>,
    pub all_pcs_stopped:bool,
    pub all_workspaces_listed:bool,
    pub confirm_delete:bool,
    pub force_guard:bool,
    loading:Option<Receiver<(String,String,Result<Report,String>)>>,
    requested:Option<(String,String)>,
    watched:bool,
}

impl Form {
    /// Poll worker without blocking the GUI or accepting a stale report.
    pub(crate) fn poll(&mut self)->bool {
        if let Some(rx)=&self.loading {
            match rx.try_recv() {
                Ok((pool,id,result))=>{
                    self.loading=None;
                    if self.requested.as_ref()==Some(&(pool,id)) {
                        match result {Ok(r)=>self.report=Some(r),Err(e)=>self.error=Some(e)}
                    }
                }
                Err(TryRecvError::Disconnected)=>{self.loading=None;self.error=Some(tr("Copy preview stopped; no deletion was started.").into());}
                Err(TryRecvError::Empty)=>{}
            }
        }
        self.loading.is_some()
    }
    /// Read-only library call in an owned GUI worker; never auto-confirms.
    fn preview(&mut self,rclone:String,pool:String,id:String,workspaces:Vec<PathBuf>) {
        if self.loading.is_some(){return;}
        self.report=None;self.error=None;self.confirm_delete=false;
        self.requested=Some((pool.clone(),id.clone()));
        let(tx,rx)=std::sync::mpsc::channel();
        std::thread::spawn(move||{
            let result=crate::migration::discard::preview(&rclone,&pool,&id,&Options{workspaces,..Options::default()})
                .map_err(|e|format!("{e:#}"));
            let _=tx.send((pool,id,result));
        });
        self.loading=Some(rx);
    }
}

/// Exact abandoned job preview/report and an explicit two-stage confirmation.
pub(super) fn show(ui:&mut egui::Ui,state:&mut GuiState,task:&mut TaskRunner) {
    let Some(id)=state.migration.active_id.clone() else {state.migration.step=Step::Plan;return;};
    let pool=state.migration.pool.clone();let rclone=state.settings.rclone.clone();
    let abandoned=state.migration.active_status().is_some_and(|s|s.abandoned);
    let mounted=crate::gui::screens::storage::mount::sessions::mounted_sessions(state)
        .iter().any(|s|s.pool==pool);
    let form=&mut state.migration.discard_copies;
    if form.watched && !task.is_running() {
        form.watched=false;form.report=None;form.confirm_delete=false;
        form.error=Some(tr("Deletion task ended. Check the console, then preview again; unknown outcomes require reconciliation.").into());
    }
    if form.report.as_ref().is_some_and(|r|r.pool!=pool||r.migration_id!=id) {form.report=None;form.confirm_delete=false;}
    ui.heading(tr("Discard unused NEW copies"));
    ui.monospace(format!("{pool} / {id}"));
    theme::hint(ui,tr("Only new objects proven to belong exclusively to this abandoned migration may be deleted. Originals, adopted/history references, other jobs and uncertain legacy writes are kept. This is not completed-original cleanup."));
    if ui.button(tr("Back")).clicked(){state.migration.step=Step::Run;return;}
    if !abandoned {theme::hint(ui,tr("This mode requires an already abandoned migration; it never abandons or stops jobs automatically."));return;}
    ui.label(tr("All local workspaces and backups (one path per line)"));
    if ui.add(egui::TextEdit::multiline(&mut form.workspace_lines).desired_rows(3)).changed(){form.report=None;form.confirm_delete=false;}
    let mut workspaces:Vec<PathBuf>=form.workspace_lines.lines().map(str::trim).filter(|s|!s.is_empty()).map(PathBuf::from).collect();
    if let Some(profile)=state.settings.mount_profiles.get(&pool) {
        if !profile.workspace.trim().is_empty(){workspaces.push(PathBuf::from(profile.workspace.trim()));}
    }
    workspaces.sort();workspaces.dedup();
    let idle=!task.is_running()&&form.loading.is_none();
    if ui.add_enabled(idle,egui::Button::new(tr("Preview unused copies (read-only)"))).clicked(){
        form.preview(rclone.clone(),pool.clone(),id.clone(),workspaces.clone());
    }
    if form.loading.is_some(){ui.spinner();ui.label(tr("Reading bounded namespace and reference views; nothing is being written or deleted."));}
    if let Some(error)=&form.error{ui.colored_label(ui.visuals().error_fg_color,error);}
    let Some(report)=form.report.clone() else{return;};
    ui.label(trf("{n} exact candidate objects ({bytes})",&[("n",&report.candidate_count),("bytes",&crate::presentation::format_bytes(report.candidate_bytes))]));
    ui.monospace(&report.preview_token);
    ui.label(tr("Preview expires after 15 minutes. Confirmation re-reads all state, references and exact object bytes. A seven-day abandonment grace is mandatory."));
    for reason in &report.uncertainties {ui.colored_label(ui.visuals().warn_fg_color,format!("{}: {reason}",tr("Deletion blocked")));}
    if let Some(why)=&report.guard_refusal {
        ui.colored_label(ui.visuals().warn_fg_color,format!("{}: {why}",tr("Numerical deletion guard")));
        ui.checkbox(&mut form.force_guard,tr("I checked count and bytes; override ONLY the numerical guard"));
    }
    egui::CollapsingHeader::new(tr("Exact objects and preservation reasons")).show(ui,|ui|{
        egui::ScrollArea::vertical().max_height(260.0).show(ui,|ui|{
            for copy in &report.copies {
                ui.strong(&copy.archive_id);
                for object in &copy.objects{ui.monospace(format!("{}  {}  {}",object.address,object.size,object.blake3));}
                for why in &copy.keep_reasons{ui.label(format!("{}: {why}",tr("Kept")));}
            }
            ui.monospace(format!("plan={}\nnamespace={:?}\ncomplete={}",report.plan_fingerprint,report.namespace_binding,report.bounded_completion));
            ui.label(&report.scope);
        });
    });
    ui.separator();
    if mounted {ui.colored_label(ui.visuals().warn_fg_color,tr("This pool is still mounted or busy in this GUI. Stop it first."));}
    ui.checkbox(&mut form.all_pcs_stopped,tr("All PCs: every mount, upload, migration and other writer is stopped and will remain stopped"));
    ui.checkbox(&mut form.all_workspaces_listed,tr("I supplied every local workspace and backup, including old copies (or none exist)"));
    ui.checkbox(&mut form.confirm_delete,tr("I reviewed this exact preview and confirm permanent deletion of only its unused NEW objects"));
    let enabled=idle&&!mounted&&report.bounded_completion&&!report.candidates.is_empty()
        &&(report.guard_refusal.is_none()||form.force_guard)&&form.all_pcs_stopped&&form.all_workspaces_listed&&form.confirm_delete;
    if theme::danger_button(ui,enabled,tr("Delete these unused NEW copies permanently")).clicked(){
        let mut args:Vec<std::ffi::OsString>=vec!["pool".into(),"migrate".into(),"discard-copies".into(),
            "--id".into(),id.into(),"--confirm".into(),"--all-pcs-stopped".into(),"--all-local-workspaces-listed".into(),
            "--preview-token".into(),report.preview_token.into(),"--json".into()];
        for workspace in &workspaces{args.extend(["--workspace".into(),workspace.as_os_str().to_owned()]);}
        if form.force_guard{args.push("--force".into());}
        args.extend(["--".into(),pool.into()]);
        match task.start_rpool(DELETE_TASK,&rclone,args){Ok(())=>{form.watched=true;form.confirm_delete=false;},Err(e)=>form.error=Some(e)}
    }
}
