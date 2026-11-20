//! One physical ledger for archives, drive payloads and packs.
use crate::prelude::*;
use super::storage_budget::*;
use crate::storage::admin::budget::TargetBudget;

pub(crate) fn specs(entry:&super::model::Entry,quotas:&[TargetBudget],attempt:&str)->Result<Vec<OutputSpec>> {
 let exact=entry.exact.as_ref().context("entry has no frozen output-size/destination contract; replan (no full-reencode fallback)")?;
 for q in quotas {
  if exact.bindings.get(&q.remote) != Some(&(q.backing.clone(),q.capacity_domain.clone(),q.failure_domain.clone())) {bail!("frozen backing/capacity/failure binding changed or unavailable; replan");}
 }
 let mode=if entry.action==super::model::Action::RelocateMinimal {ExecutionMode::Minimal}else{ExecutionMode::Full};
 let mut out=Vec::new();
 let mut group_counts=BTreeMap::<(u32,String),usize>::new();
 for o in &exact.objects {
  let q=quotas.iter().find(|q|q.remote==o.destination_root).context("destination binding missing")?;
  let count=group_counts.entry((o.group,q.backing.clone())).or_default();*count+=1;
  if q.shard_cap.is_some_and(|cap|*count>cap){bail!("frozen account shard cap exceeded");}
 }
 for o in &exact.objects {
  let q=quotas.iter().find(|q|q.remote==o.destination_root).context("destination capacity binding unknown")?;
  let src=quotas.iter().find(|q|o.source.starts_with(&crate::utils::remote_join(&q.remote,""))).map(|q|q.capacity_domain.clone());
  let id=if o.keep_ref {o.source.clone()}else{format!("{attempt}/shard/{}",o.index)};
  let mut outgoing=BTreeMap::new();
  if !o.keep_ref && mode==ExecutionMode::Full || (!o.keep_ref && !o.source.starts_with(&crate::utils::remote_join(&o.destination_root,""))) {if let Some(src)=src {outgoing.insert(src,o.size);}}
  out.push(OutputSpec{entry_id:entry.archive_id.clone(),mode,destination_remote:o.destination_root.clone(),capacity_domain:q.capacity_domain.clone(),object_id:id.clone(),
   ownership:if o.keep_ref {OutputOwnership::BorrowedReference}else{OutputOwnership::Created},method:if o.keep_ref {CreationMethod::Reference}else{CreationMethod::ClientCopy},
   size:if o.keep_ref {ObjectSize::Stored{whole_object_bytes:o.size}}else{ObjectSize::Encrypted{plaintext_bytes:o.size,header_bytes:32,block_bytes:65536,overhead_per_block:16}},
   borrowed_from:o.keep_ref.then(||(q.capacity_domain.clone(),id)),logical_move_in_bytes:if o.keep_ref {0}else{o.size},logical_move_out_bytes:outgoing,local_scratch_bytes:Some(if o.keep_ref {0}else{exact.local_scratch_bytes.checked_mul(3).context("scratch bound overflow")?})});
 }
 for q in quotas {
  out.push(OutputSpec{entry_id:entry.archive_id.clone(),mode,destination_remote:q.remote.clone(),capacity_domain:q.capacity_domain.clone(),object_id:format!("{attempt}/metadata/{}",q.remote),ownership:OutputOwnership::Created,method:CreationMethod::Reencode,size:ObjectSize::Plain{whole_object_bytes:exact.additional_bytes.checked_sub(exact.copied_bytes).context("metadata bound invalid")?.div_ceil(quotas.len().max(1) as u64).max(1024*1024)},borrowed_from:None,logical_move_in_bytes:0,logical_move_out_bytes:BTreeMap::new(),local_scratch_bytes:Some(1024*1024)});
 }
 Ok(out)
}
pub(crate) fn preview(quotas:&[TargetBudget],entries:impl IntoIterator<Item=super::model::Entry>)->StoragePreview {
 let observations:Vec<_>=quotas.iter().map(|q|CapacityObservation{remote:q.remote.clone(),capacity_domain:q.capacity_domain.clone(),total_bytes:Some(q.total),free_bytes:Some(q.free),metadata_reserve_bytes:0,reserved_bytes:0}).collect();
 let mut all=Vec::new();let mut reasons=Vec::new();let mut seen=BTreeSet::new();
 for e in entries {
  if e.action==super::model::Action::ImmutableObjectCapability {continue;}
  if matches!(e.action,super::model::Action::Unaffected|super::model::Action::Lost){continue;}
  // A pack/archive payload is costed once even when several references name it.
  if e.fingerprint.is_empty(){reasons.push("unknown source fingerprint".into());continue;}
  if !seen.insert(e.fingerprint.clone()){continue;}
  match specs(&e,quotas,&format!("planned/{}",e.fingerprint)){Ok(s)=>all.extend(s),Err(e)=>reasons.push(e.to_string())}
 }
 let mut p=preview_storage(&observations,&all);p.blocked_reasons.extend(reasons);p.admitted &= p.blocked_reasons.is_empty();p
}
pub(crate) fn fresh(rclone:&str,target:&PoolDefinition)->Result<Vec<TargetBudget>> {
 use crate::storage::admin::{BackendAdmin,RcloneAdmin};
 let admin=RcloneAdmin::inherited(rclone);let catalog=admin.catalog()?;
 let b=crate::storage::admin::budget::BudgetSnapshot::query(&admin,&catalog,&target.remotes);
 if !b.rejected.is_empty() || b.targets.len()!=target.remotes.len(){bail!("fresh quota/capacity identity incomplete; admission blocked");}
 Ok(b.targets)
}

/// Whole-entry batch reservation: no bytes are released on failure/success.
pub(crate) fn admit_entry(rclone:&str,target:&PoolDefinition,entry:&super::model::Entry,attempt:&str,work:&Path)->Result<Admission> {
 let receipt=work.join("physical-admission-started-v1");
 // A crash/unknown write is not replay permission, even after process restart.
 if receipt.exists(){bail!("prior write admission outcome may be unknown; reconcile exact objects before retry");}
 let coordinator=process_storage_coordinator();
 let ticket=coordinator.begin_observation()?;
 let q=fresh(rclone,target)?;
 let observations=||q.iter().map(|q|CapacityObservation{remote:q.remote.clone(),capacity_domain:q.capacity_domain.clone(),total_bytes:Some(q.total),free_bytes:Some(q.free),metadata_reserve_bytes:0,reserved_bytes:0}).collect();
 coordinator.refresh(FreshQuota{ticket,quotas:observations(),accounted_outputs:Vec::new()})?;
 let outputs=specs(entry,&q,attempt)?.iter().map(calculate_output_cost).collect::<Result<Vec<_>>>()?;
 let scratch=outputs.iter().try_fold(0u64,|a,o|a.checked_add(o.local_scratch_bytes).context("scratch overflow"))?;
 let free=crate::mount::shard_cache::disk::available(work).context("local scratch free space unknown")?;
 let temp=crate::mount::shard_cache::disk::available(&std::env::temp_dir()).context("system temporary free space unknown")?;
 if free < scratch || temp < scratch {bail!("insufficient local work/temp scratch space; no writes admitted");}
 let keyed:Vec<_>=outputs.into_iter().map(|o|(ReservationKey{migration:target.remotes.join("|"),entry:entry.archive_id.clone(),attempt:attempt.into(),object:o.spec.object_id.clone()},o)).collect();
 coordinator.reserve_plan(&keyed)?;
 let mut keys=Vec::new();
 for (key,output) in keyed {
  let ticket=coordinator.begin_observation()?;let freshq=fresh(rclone,target)?;
  coordinator.admit(&key,&output,FreshQuota{ticket,quotas:freshq.into_iter().map(|q|CapacityObservation{remote:q.remote,capacity_domain:q.capacity_domain,total_bytes:Some(q.total),free_bytes:Some(q.free),metadata_reserve_bytes:0,reserved_bytes:0}).collect(),accounted_outputs:Vec::new()})?;
  keys.push(key);
 }
 let mut file=OpenOptions::new().write(true).create_new(true).open(receipt)?;
 file.write_all(b"exact physical outputs reserved; retain on unknown write")?;file.sync_all()?;
 Ok(Admission{coordinator,keys,done:false})
}
pub(crate) struct Admission {coordinator:StorageCoordinator,keys:Vec<ReservationKey>,done:bool}
impl Admission {pub(crate) fn written(mut self)->Result<()> {for key in &self.keys {self.coordinator.finish(key,WriteOutcome::Written)?;}self.done=true;Ok(())}}
impl Drop for Admission {fn drop(&mut self){if !self.done {for key in &self.keys {let _=self.coordinator.finish(key,WriteOutcome::Unknown);}}}}
