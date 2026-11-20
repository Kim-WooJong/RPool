//! Frozen object-level execution contract; physical additions never spend retire credit.
use crate::prelude::*;
use super::model::Action;
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub(crate) struct ExactObject { pub index:u32,pub group:u32,pub source:String,pub destination_root:String,pub size:u64,pub keep_ref:bool,pub blake3:String }
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub(crate) struct ExactExecutionSpec {
 pub capability:String,pub source_fingerprint:String,pub layout_digest:String,pub objects:Vec<ExactObject>,
 #[serde(default)] pub bindings:BTreeMap<String,(String,String,Option<String>)>,
 pub additional_bytes:u64,pub copied_bytes:u64,pub transfer_bytes:u64,pub local_scratch_bytes:u64,
}
pub(crate) fn freeze(source:&Manifest,target:&PoolDefinition,moves:&mut BTreeMap<u32,String>,minimal:bool,force_create:&BTreeSet<u32>)->Result<ExactExecutionSpec> {
 if source.archive_id.len()>256 || target.remotes.iter().any(|r|r.len()>1024) || source.shards.iter().any(|s|s.object.len()>4096 || s.remote.len()>1024) {bail!("metadata address length exceeds conservative exact bound");}
 let remotes=&target.remotes;
 if remotes.is_empty(){bail!("no execution destinations");}
 // Minimal mode never invents a destination at execution. Non-rebalance removed
 // roots need a new explicit plan instead of silently selecting a target.
 let mut objects=Vec::new();let mut extra=0u64;let mut copied=0u64;
 for s in &source.shards {
  let dst=moves.get(&s.index).cloned().unwrap_or_else(||s.remote.clone());
  if !remotes.contains(&dst){bail!("exact destination unavailable for shard {}; use explicit rebalance plan",s.index);}
  moves.insert(s.index,dst.clone());
  let keep=minimal && dst==s.remote && !force_create.contains(&s.index);
  if !keep { copied=copied.checked_add(s.size).context("copy byte overflow")?; extra=extra.checked_add(s.size.checked_add(32+(s.size/65536+1)*16).context("crypto overhead overflow")?).context("peak overflow")?; }
  objects.push(ExactObject{index:s.index,group:s.group,source:s.object.clone(),destination_root:dst,size:s.size,keep_ref:keep,blake3:s.blake3.clone()});
 }
 // Replicas + source fence + journal/publication metadata, conservatively per root.
 let manifest_bound=(serde_json::to_vec(source)?.len() as u64).checked_mul(8).and_then(|n|n.checked_add((source.shards.len() as u64).checked_mul(4096)?)).and_then(|n|n.checked_add(1024*1024)).context("metadata bound overflow")?;
 extra=extra.checked_add(manifest_bound.checked_mul(remotes.len() as u64).context("metadata peak overflow")?).context("metadata peak overflow")?;
 let fp=crate::manifest::manifest_fingerprint(source)?;
 let digest=blake3::hash(&serde_json::to_vec(&(target,&objects))?).to_hex().to_string();
 Ok(ExactExecutionSpec{bindings:BTreeMap::new(),capability:"exact-immutable-execution-v1".into(),source_fingerprint:fp,layout_digest:digest,objects,additional_bytes:extra,copied_bytes:copied,transfer_bytes:copied.checked_mul(3).and_then(|n|n.checked_add(source.shards.iter().try_fold(0u64,|a,s|a.checked_add(s.size))?)).context("transfer overflow")?,local_scratch_bytes:source.shards.iter().try_fold(0u64,|a,s|a.checked_add(s.size)).context("scratch overflow")?})
}
pub(crate) fn validate(entry:&super::model::Entry,source:&Manifest,target:&PoolDefinition)->Result<()> {
 if !matches!(entry.action,Action::RelocateMinimal|Action::Relocate){return Ok(());}
 let expected=entry.exact.as_ref().context("minimal transfer requires exact execution specification")?;
 let force_create=expected.objects.iter().filter(|o|!o.keep_ref).map(|o|o.index).collect();
 let mut actual=freeze(source,target,&mut entry.moves.clone(),entry.action==Action::RelocateMinimal,&force_create)?;
 actual.bindings=expected.bindings.clone();
 if &actual!=expected {bail!("execution specification/source/layout changed; replan");}
 Ok(())
}
pub(crate) fn ack(plan:&super::model::Plan)->Result<()> {
 if plan.entries.iter().any(|e|e.action==Action::RelocateMinimal) || plan.minimal_ack.is_some(){
  let a=plan.minimal_ack.as_ref().context("minimal transfer requires all-PC acknowledgement")?;
  if !a.all_pcs_upgraded || !a.all_pcs_stopped {bail!("all PCs must be upgraded and stopped; old clients must never reconnect");}
 }
 Ok(())
}
