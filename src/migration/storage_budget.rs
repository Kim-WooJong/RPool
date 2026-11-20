//! Physical migration admission, independent of logical after-retirement scoring.
//! No data/settings/provider I/O and no placement decisions. Callers supply the
//! EXACT destinations and ownership that execution will use, including whole
//! blob copies (not merely changed shard slices), manifests and sidecars.
//! Original objects receive ZERO physical credit, even for serial execution.
//! Unknown full-reencode/blob sizes or encryption bounds fail closed.
//!
//! Integration: register this module; build OutputSpec from immutable planned
//! output ownership; use calculate_entry_cost/preview_storage for both archive
//! and drive paths; verify those exact specs again before execution. Never use
//! Rebalancer's logical source credits for physical admission.
use crate::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecutionMode { Full, Minimal, FullReencode }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutputOwnership { BorrowedReference, Created }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CreationMethod { Reference, ClientCopy, ServerCopy, Reencode }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ObjectSize {
    /// Whole output size already known on the destination (also useful for
    /// ciphertext-preserving server copies). Not a changed-shard byte count.
    Stored { whole_object_bytes: u64 },
    Plain { whole_object_bytes: u64 },
    /// Conservative encrypted object bound: header + plaintext + per-block
    /// expansion. Caller must supply the real backend/crypt format bound.
    Encrypted { plaintext_bytes: u64, header_bytes: u64, block_bytes: u64,
        overhead_per_block: u64 },
    Unknown { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OutputSpec {
    pub entry_id: String,
    pub mode: ExecutionMode,
    pub destination_remote: String,
    pub capacity_domain: String,
    /// Canonical physical object identity within the capacity domain. Aliases
    /// must use the same ID. Include the immutable planned version/attempt.
    pub object_id: String,
    pub ownership: OutputOwnership,
    pub method: CreationMethod,
    pub size: ObjectSize,
    /// Borrowed references require the SAME physical object, already present.
    pub borrowed_from: Option<(String, String)>,
    pub logical_move_in_bytes: u64,
    pub logical_move_out_bytes: BTreeMap<String, u64>,
    /// Conservative simultaneous scratch for this object; None is unknown.
    /// Include decoding/download/encryption temporary files, not just upload.
    pub local_scratch_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OutputObject {
    pub spec: OutputSpec,
    pub whole_object_bytes: u64,
    /// New remote allocation; borrowed references alone are zero.
    pub actual_created_bytes: u64,
    /// Copy amount, including server-side copies (network traffic may differ).
    pub actual_copy_bytes: u64,
    pub local_scratch_bytes: u64,
}

/// Versioned fingerprint of exact destination/ownership/mode/cost inputs.
/// Input order is significant; store it with the plan and compare the same
/// canonical ordered specs at execution (the caller owns persistence/checks).
pub(crate) fn output_specs_fingerprint(specs: &[OutputSpec]) -> Result<String> {
    let encoded = serde_json::to_vec(specs)?;
    let mut hash = Hasher::new();
    hash.update(b"rpool-storage-output-specs-v1\0");
    hash.update(&encoded);
    Ok(hash.finalize().to_hex().to_string())
}

fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).context("storage byte/count overflow")
}

fn add_to(map: &mut BTreeMap<String, u64>, key: &str, bytes: u64) -> Result<()> {
    let n = map.entry(key.to_owned()).or_default();
    *n = add(*n, bytes)?;
    Ok(())
}

/// Deterministic cost of one EXACT output. Does not select another remote.
pub(crate) fn calculate_output_cost(spec: &OutputSpec) -> Result<OutputObject> {
    if spec.entry_id.is_empty() || spec.destination_remote.is_empty()
        || spec.capacity_domain.is_empty() || spec.object_id.is_empty()
        || spec.logical_move_out_bytes.keys().any(|d| d.is_empty()) {
        bail!("output identity or logical source identity is unknown");
    }
    let borrowed = spec.ownership == OutputOwnership::BorrowedReference;
    if borrowed {
        if spec.method != CreationMethod::Reference
            || spec.borrowed_from.as_ref() != Some(&(spec.capacity_domain.clone(), spec.object_id.clone()))
            || spec.logical_move_in_bytes != 0
            || spec.logical_move_out_bytes.values().any(|n| *n != 0) {
            bail!("borrowed output must reference the same existing physical object without movement");
        }
    } else if spec.method == CreationMethod::Reference || spec.borrowed_from.is_some() {
        bail!("created output cannot be a borrowed reference");
    }
    let bytes = match &spec.size {
        ObjectSize::Stored { whole_object_bytes } | ObjectSize::Plain { whole_object_bytes } => *whole_object_bytes,
        ObjectSize::Encrypted { plaintext_bytes, header_bytes, block_bytes, overhead_per_block } => {
            if *block_bytes == 0 || *header_bytes == 0 || *overhead_per_block == 0 {
                bail!("encrypted output overhead bound is missing");
            }
            let blocks = plaintext_bytes / block_bytes + u64::from(plaintext_bytes % block_bytes != 0);
            let overhead = blocks.checked_mul(*overhead_per_block).context("encrypted size overflow")?;
            add(add(*plaintext_bytes, *header_bytes)?, overhead)?
        }
        ObjectSize::Unknown { reason } => bail!("output size is unknown: {reason}"),
    };
    let scratch = spec.local_scratch_bytes.context("local scratch bound is unknown")?;
    if borrowed && scratch != 0 { bail!("borrowed reference must not create scratch"); }
    Ok(OutputObject { spec: spec.clone(), whole_object_bytes: bytes,
        actual_created_bytes: if borrowed { 0 } else { bytes },
        actual_copy_bytes: if matches!(spec.method, CreationMethod::ClientCopy | CreationMethod::ServerCopy) { bytes } else { 0 },
        local_scratch_bytes: scratch })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EntryExecutionCost {
    pub entry_id: String,
    pub mode: ExecutionMode,
    pub outputs: Vec<OutputObject>,
    pub additional_physical_bytes: BTreeMap<String, u64>,
    pub logical_move_in_bytes: BTreeMap<String, u64>,
    pub logical_move_out_bytes: BTreeMap<String, u64>,
    pub actual_copy_bytes: u64,
    /// Sum within one entry (conservative if its outputs stream serially).
    pub local_scratch_peak_bytes: u64,
    pub moved_objects: u64,
    pub copied_objects: u64,
    pub reencoded_objects: u64,
}

/// No fallback estimate: an uncertain blob/full reencode is an Err, which
/// preview_storage exposes as None plus a blocking reason. Specs of metadata
/// objects must also be supplied; they are not silently estimated here.
pub(crate) fn calculate_entry_cost(specs: &[OutputSpec]) -> Result<EntryExecutionCost> {
    let first = specs.first().context("entry has no explicit output specs")?;
    let mut result = EntryExecutionCost { entry_id: first.entry_id.clone(), mode: first.mode,
        outputs: Vec::new(), additional_physical_bytes: BTreeMap::new(),
        logical_move_in_bytes: BTreeMap::new(), logical_move_out_bytes: BTreeMap::new(),
        actual_copy_bytes: 0, local_scratch_peak_bytes: 0, moved_objects: 0,
        copied_objects: 0, reencoded_objects: 0 };
    let mut seen = BTreeMap::new();
    for spec in specs {
        if spec.entry_id != first.entry_id || spec.mode != first.mode {
            bail!("entry cost mixes entry identities or execution modes");
        }
        if let Some(old) = seen.insert((&spec.capacity_domain, &spec.object_id), spec.ownership) {
            if old != OutputOwnership::BorrowedReference || spec.ownership != OutputOwnership::BorrowedReference {
                bail!("duplicate physical output in entry");
            }
        }
        let output = calculate_output_cost(spec)?;
        add_to(&mut result.additional_physical_bytes, &spec.capacity_domain, output.actual_created_bytes)?;
        add_to(&mut result.logical_move_in_bytes, &spec.capacity_domain, spec.logical_move_in_bytes)?;
        for (domain, bytes) in &spec.logical_move_out_bytes {
            add_to(&mut result.logical_move_out_bytes, domain, *bytes)?;
        }
        result.actual_copy_bytes = add(result.actual_copy_bytes, output.actual_copy_bytes)?;
        result.local_scratch_peak_bytes = add(result.local_scratch_peak_bytes, output.local_scratch_bytes)?;
        result.moved_objects = add(result.moved_objects, u64::from(spec.logical_move_in_bytes > 0 || spec.logical_move_out_bytes.values().any(|n| *n > 0)))?;
        result.copied_objects = add(result.copied_objects, u64::from(matches!(spec.method, CreationMethod::ClientCopy | CreationMethod::ServerCopy)))?;
        result.reencoded_objects = add(result.reencoded_objects, u64::from(spec.method == CreationMethod::Reencode))?;
        result.outputs.push(output);
    }
    Ok(result)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CapacityObservation {
    pub remote: String,
    pub capacity_domain: String,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    /// Reserve not ALREADY subtracted from total/free. For TargetBudget
    /// inputs (already net of metadata_reserve), pass zero here.
    pub metadata_reserve_bytes: u64,
    /// Other local reservations, excluding costs passed to this preview.
    pub reserved_bytes: u64,
}

fn validate_observation(q: &CapacityObservation) -> Result<(u64, u64)> {
    if q.remote.is_empty() || q.capacity_domain.is_empty() { bail!("quota identity is unknown"); }
    let total = q.total_bytes.context("total quota is unknown")?;
    let free = q.free_bytes.context("free quota is unknown")?;
    if total == 0 || free > total { bail!("quota is inconsistent"); }
    Ok((total, free))
}

/// Capacity aliases are counted ONCE. Conflicting totals/reserve contracts
/// fail closed; differing observations of free use the conservative minimum.
fn group_observations(quotas: &[CapacityObservation]) -> Result<BTreeMap<String, CapacityObservation>> {
    let mut groups = BTreeMap::<String, CapacityObservation>::new();
    let mut remotes = BTreeMap::new();
    for q in quotas {
        validate_observation(q)?;
        if remotes.insert(&q.remote, &q.capacity_domain).is_some_and(|d| d != &q.capacity_domain) {
            bail!("remote maps to conflicting capacity identities");
        }
        if let Some(old) = groups.get_mut(&q.capacity_domain) {
            if old.total_bytes != q.total_bytes || old.metadata_reserve_bytes != q.metadata_reserve_bytes {
                bail!("capacity aliases disagree on total/reserve contract");
            }
            old.free_bytes = old.free_bytes.min(q.free_bytes);
            old.reserved_bytes = old.reserved_bytes.max(q.reserved_bytes);
        } else { groups.insert(q.capacity_domain.clone(), q.clone()); }
    }
    Ok(groups)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct DiskStoragePreview {
    pub capacity_domain: String,
    pub remotes: Vec<String>,
    pub total_bytes: Option<u64>,
    pub current_used_bytes: Option<u64>,
    /// Logical balanced target AFTER originals are retired; never admission.
    pub logical_after_retire_used_bytes: Option<u64>,
    /// Current usage + reserved + cumulative created outputs (original keep).
    pub peak_used_bytes: Option<u64>,
    pub current_free_bytes: Option<u64>,
    pub metadata_reserve_bytes: u64,
    pub reserved_bytes: u64,
    pub logical_move_in_bytes: Option<u64>,
    pub logical_move_out_bytes: Option<u64>,
    pub additional_created_bytes: Option<u64>,
    pub actual_copy_bytes: Option<u64>,
    pub moved_objects: Option<u64>,
    pub copied_objects: Option<u64>,
    pub reencoded_objects: Option<u64>,
    /// Signed so an insufficient plan shows its deficit, never a clamped 0.
    pub headroom_bytes: Option<i128>,
    pub blocked_reasons: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct StoragePreview {
    pub admitted: bool,
    pub disks: Vec<DiskStoragePreview>,
    /// Serial entries reuse scratch, but NEVER release remote originals.
    pub local_scratch_peak_bytes: Option<u64>,
    pub blocked_reasons: Vec<String>,
}

/// Preview the exact plan. No estimates are substituted for unknown costs.
/// Local disk admission remains caller-owned: compare the scratch peak with
/// fresh free space on the actual scratch filesystem before execution.
pub(crate) fn preview_storage(quotas: &[CapacityObservation], specs: &[OutputSpec]) -> StoragePreview {
    let mut preview = StoragePreview::default();
    let groups = match group_observations(quotas) {
        Ok(groups) => groups,
        Err(e) => { preview.blocked_reasons.push(e.to_string()); return preview; }
    };
    if groups.is_empty() {
        preview.blocked_reasons.push("no known capacity domains".into());
        return preview;
    }
    let mut by_entry = BTreeMap::<String, Vec<OutputSpec>>::new();
    let mut seen = BTreeMap::new();
    let mut invalid_domains = BTreeSet::new();
    for spec in specs {
        if let Some(old) = seen.insert((&spec.capacity_domain, &spec.object_id), spec.ownership) {
            if old != OutputOwnership::BorrowedReference || spec.ownership != OutputOwnership::BorrowedReference {
                preview.blocked_reasons.push(format!("duplicate physical output {}:{}", spec.capacity_domain, spec.object_id));
                invalid_domains.insert(spec.capacity_domain.clone());
            }
        }
        if !quotas.iter().any(|q| q.remote == spec.destination_remote && q.capacity_domain == spec.capacity_domain) {
            preview.blocked_reasons.push(format!("unresolved destination identity: {}", spec.destination_remote));
            invalid_domains.insert(spec.capacity_domain.clone());
        }
        for domain in spec.logical_move_out_bytes.keys() {
            if !groups.contains_key(domain) {
                preview.blocked_reasons.push(format!("unknown logical source capacity: {domain}"));
            }
        }
        by_entry.entry(spec.entry_id.clone()).or_default().push(spec.clone());
    }
    let mut costs = Vec::new();
    let mut scratch = Some(0u64);
    for (entry, outputs) in by_entry {
        match calculate_entry_cost(&outputs) {
            Ok(cost) => {
                scratch = scratch.map(|n| n.max(cost.local_scratch_peak_bytes));
                costs.push(cost);
            }
            Err(e) => {
                scratch = None;
                preview.blocked_reasons.push(format!("entry {entry}: {e}"));
                for output in outputs {
                    invalid_domains.insert(output.capacity_domain);
                    invalid_domains.extend(output.logical_move_out_bytes.into_keys());
                }
            }
        }
    }
    preview.local_scratch_peak_bytes = scratch;
    for (domain, quota) in groups {
        let mut disk = DiskStoragePreview { capacity_domain: domain.clone(),
            remotes: quotas.iter().filter(|q| q.capacity_domain == domain).map(|q| q.remote.clone()).collect(),
            total_bytes: quota.total_bytes, current_free_bytes: quota.free_bytes,
            metadata_reserve_bytes: quota.metadata_reserve_bytes, reserved_bytes: quota.reserved_bytes,
            ..Default::default() };
        let result = (|| -> Result<()> {
            let (total, free) = validate_observation(&quota)?;
            disk.current_used_bytes = Some(total - free);
            if invalid_domains.contains(&domain) { bail!("output cost/identity is unknown or duplicated"); }
            let (mut created, mut incoming, mut outgoing, mut copied) = (0, 0, 0, 0);
            let (mut moved_n, mut copied_n, mut reencoded_n) = (0, 0, 0);
            for cost in &costs {
                incoming = add(incoming, cost.logical_move_in_bytes.get(&domain).copied().unwrap_or(0))?;
                outgoing = add(outgoing, cost.logical_move_out_bytes.get(&domain).copied().unwrap_or(0))?;
                for output in &cost.outputs {
                    if output.spec.capacity_domain != domain { continue; }
                    created = add(created, output.actual_created_bytes)?;
                    copied = add(copied, output.actual_copy_bytes)?;
                    moved_n = add(moved_n, u64::from(output.spec.logical_move_in_bytes > 0 || output.spec.logical_move_out_bytes.values().any(|n| *n > 0)))?;
                    copied_n = add(copied_n, u64::from(matches!(output.spec.method, CreationMethod::ClientCopy | CreationMethod::ServerCopy)))?;
                    reencoded_n = add(reencoded_n, u64::from(output.spec.method == CreationMethod::Reencode))?;
                }
            }
            disk.logical_move_in_bytes = Some(incoming); disk.logical_move_out_bytes = Some(outgoing);
            disk.additional_created_bytes = Some(created); disk.actual_copy_bytes = Some(copied);
            disk.moved_objects = Some(moved_n); disk.copied_objects = Some(copied_n);
            disk.reencoded_objects = Some(reencoded_n);
            disk.logical_after_retire_used_bytes = Some(add(total - free, incoming)?
                .checked_sub(outgoing).context("logical retire credits exceed known usage")?);
            disk.peak_used_bytes = Some(add(add(total - free, quota.reserved_bytes)?, created)?);
            let headroom = i128::from(free) - i128::from(quota.metadata_reserve_bytes)
                - i128::from(quota.reserved_bytes) - i128::from(created);
            disk.headroom_bytes = Some(headroom);
            if headroom < 0 { bail!("insufficient physical headroom: {headroom} bytes"); }
            Ok(())
        })();
        if let Err(e) = result { disk.blocked_reasons.push(e.to_string()); }
        preview.disks.push(disk);
    }
    preview.admitted = preview.blocked_reasons.is_empty()
        && preview.disks.iter().all(|d| d.blocked_reasons.is_empty());
    preview
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct ReservationKey {
    pub migration: String,
    pub entry: String,
    pub attempt: String,
    pub object: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReservationState { Planned, Writing, Written, UnknownWrite }

#[derive(Debug, Clone)]
struct Reservation {
    output: OutputObject,
    state: ReservationState,
    /// Only explicit observed-output reconciliation removes a write shadow.
    accounted: bool,
}

#[derive(Debug, Clone)]
struct ObservedCapacity {
    quota: CapacityObservation,
    generation: u64,
}

#[derive(Debug, Clone, Default)]
struct AdmissionLedger {
    next_observation: u64,
    capacities: BTreeMap<String, ObservedCapacity>,
    remotes: BTreeMap<String, String>,
    reservations: BTreeMap<ReservationKey, Reservation>,
}

/// Obtain BEFORE querying fresh quotas. An older in-flight response cannot
/// overwrite a newer observation. The private fields prevent invented epochs.
#[derive(Debug, Clone)]
pub(crate) struct ObservationTicket { generation: u64, coordinator_id: Arc<()> }

#[derive(Debug, Clone)]
pub(crate) struct FreshQuota {
    pub ticket: ObservationTicket,
    /// RAW free/total plus explicit metadata reserve, OR already-net
    /// TargetBudget free/total with metadata_reserve_bytes = 0.
    /// reserved_bytes excludes this coordinator's own reservations.
    pub quotas: Vec<CapacityObservation>,
    /// Explicit proof supplied by the caller that these completed/uncertain
    /// outputs are INCLUDED in this observation's used quota. Mere successful
    /// upload, elapsed time, or a quota refresh is NOT proof. Provider quota
    /// lag must leave this empty and keep the shadow. Re-attest on EVERY
    /// observation: omitted outputs regain their shadow (quota can regress).
    pub accounted_outputs: Vec<ReservationKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteOutcome { Written, Unknown }

/// In-process only. Archive execution and drive catch-up MUST use clones of
/// the SAME coordinator. No guarantees for another PC's writes/quota lag.
/// No automatic release on Drop, cancellation, timeout or successful writes.
#[derive(Debug, Clone, Default)]
pub(crate) struct StorageCoordinator {
    inner: Arc<Mutex<AdmissionLedger>>,
    coordinator_id: Arc<()>,
}

pub(crate) fn process_storage_coordinator() -> StorageCoordinator {
    static SHARED: std::sync::OnceLock<StorageCoordinator> = std::sync::OnceLock::new();
    SHARED.get_or_init(StorageCoordinator::default).clone()
}

impl AdmissionLedger {
    fn headroom(&self, domain: &str) -> Result<i128> {
        let q = &self.capacities.get(domain).context("no fresh quota for capacity domain")?.quota;
        let (_, free) = validate_observation(q)?;
        let mut shadow = 0;
        for r in self.reservations.values() {
            if r.output.spec.capacity_domain == domain && !r.accounted {
                shadow = add(shadow, r.output.actual_created_bytes)?;
            }
        }
        Ok(i128::from(free) - i128::from(q.metadata_reserve_bytes)
            - i128::from(q.reserved_bytes) - i128::from(shadow))
    }

    fn refresh(&mut self, fresh: &FreshQuota) -> Result<()> {
        let groups = group_observations(&fresh.quotas)?;
        if groups.is_empty() || fresh.ticket.generation == 0
            || fresh.ticket.generation > self.next_observation {
            bail!("fresh quota observation is missing or invalid");
        }
        for (domain, q) in &groups {
            if self.capacities.get(domain).is_some_and(|old| old.generation >= fresh.ticket.generation) {
                bail!("stale/out-of-order quota observation");
            }
            validate_observation(q)?;
        }
        for q in &fresh.quotas {
            if self.remotes.get(&q.remote).is_some_and(|d| d != &q.capacity_domain) {
                bail!("remote capacity identity changed during admission");
            }
        }
        for key in &fresh.accounted_outputs {
            let r = self.reservations.get(key).context("observed output has no reservation")?;
            if !groups.contains_key(&r.output.spec.capacity_domain)
                || !matches!(r.state, ReservationState::Written | ReservationState::UnknownWrite) {
                bail!("cannot reconcile an unobserved domain or an unfinished output");
            }
        }
        // All validation above; caller commits this candidate atomically.
        for q in &fresh.quotas { self.remotes.insert(q.remote.clone(), q.capacity_domain.clone()); }
        for (domain, quota) in groups {
            self.capacities.insert(domain, ObservedCapacity { quota, generation: fresh.ticket.generation });
        }
        for r in self.reservations.values_mut() {
            if fresh.quotas.iter().any(|q| q.capacity_domain == r.output.spec.capacity_domain) {
                r.accounted = false;
            }
        }
        for key in &fresh.accounted_outputs {
            self.reservations.get_mut(key).context("reservation disappeared")?.accounted = true;
        }
        Ok(())
    }

    fn reserve(&mut self, key: &ReservationKey, output: &OutputObject) -> Result<()> {
        if key.migration.is_empty() || key.entry.is_empty() || key.attempt.is_empty()
            || key.object.is_empty() || key.entry != output.spec.entry_id
            || key.object != output.spec.object_id {
            bail!("invalid reservation identity");
        }
        // Public serialized cost fields are evidence, not authority. Recompute.
        if calculate_output_cost(&output.spec)? != *output { bail!("output cost does not match immutable spec"); }
        if self.remotes.get(&output.spec.destination_remote) != Some(&output.spec.capacity_domain) {
            bail!("destination has no observed capacity binding");
        }
        if let Some(old) = self.reservations.get(key) {
            if old.output != *output { bail!("reservation key reused with different output"); }
            return Ok(()); // Planned and runtime are the SAME deduction.
        }
        if self.reservations.values().any(|r| r.output.spec.capacity_domain == output.spec.capacity_domain
            && r.output.spec.object_id == output.spec.object_id
            && (r.output.spec.ownership != OutputOwnership::BorrowedReference
                || output.spec.ownership != OutputOwnership::BorrowedReference)) {
            bail!("physical output already reserved under another attempt/key");
        }
        self.reservations.insert(key.clone(), Reservation { output: output.clone(),
            state: ReservationState::Planned, accounted: false });
        Ok(())
    }

    fn ensure_headroom(&self) -> Result<()> {
        for domain in self.capacities.keys() {
            let headroom = self.headroom(domain)?;
            if headroom < 0 { bail!("physical capacity {domain} lacks {} bytes", -headroom); }
        }
        Ok(())
    }
}

impl StorageCoordinator {
    pub(crate) fn begin_observation(&self) -> Result<ObservationTicket> {
        let mut ledger = self.inner.lock().map_err(|_| anyhow!("storage admission lock poisoned"))?;
        ledger.next_observation = add(ledger.next_observation, 1)?;
        Ok(ObservationTicket { generation: ledger.next_observation, coordinator_id: self.coordinator_id.clone() })
    }

    /// Install fresh provider data and explicitly reconcile observed outputs.
    /// A negative headroom is retained as fact; subsequent admission fails.
    pub(crate) fn refresh(&self, fresh: FreshQuota) -> Result<()> {
        if !Arc::ptr_eq(&self.coordinator_id, &fresh.ticket.coordinator_id) {
            bail!("quota ticket belongs to another coordinator");
        }
        let mut ledger = self.inner.lock().map_err(|_| anyhow!("storage admission lock poisoned"))?;
        let mut next = ledger.clone();
        next.refresh(&fresh)?;
        *ledger = next;
        Ok(())
    }

    /// Atomic cumulative plan reservation. Serial entries do not release
    /// previous entries' reservations. Repeat exact keys to resume; never
    /// reserve a plan AND subtract those same bytes in reserved_bytes.
    pub(crate) fn reserve_plan(&self, outputs: &[(ReservationKey, OutputObject)]) -> Result<()> {
        let mut ledger = self.inner.lock().map_err(|_| anyhow!("storage admission lock poisoned"))?;
        let mut next = ledger.clone();
        for (key, output) in outputs { next.reserve(key, output)?; }
        next.ensure_headroom()?;
        *ledger = next;
        Ok(())
    }

    /// Fresh quota admission immediately before starting this exact output.
    /// The planned reservation is promoted, NOT deducted a second time.
    /// Caller must not write when this returns Err, including already-writing
    /// or completed/uncertain outputs; reconcile/inspect those instead.
    pub(crate) fn admit(&self, key: &ReservationKey, output: &OutputObject, fresh: FreshQuota) -> Result<()> {
        if !Arc::ptr_eq(&self.coordinator_id, &fresh.ticket.coordinator_id) {
            bail!("quota ticket belongs to another coordinator");
        }
        let mut ledger = self.inner.lock().map_err(|_| anyhow!("storage admission lock poisoned"))?;
        if !fresh.quotas.iter().any(|q| q.remote == output.spec.destination_remote
            && q.capacity_domain == output.spec.capacity_domain) {
            bail!("admission needs a fresh quota for the exact destination");
        }
        let mut observed = ledger.clone();
        observed.refresh(&fresh)?;
        *ledger = observed; // Retain fresh facts even if this admission fails.
        let mut next = ledger.clone();
        next.reserve(key, output)?;
        let r = next.reservations.get_mut(key).context("reservation missing")?;
        if r.state != ReservationState::Planned { bail!("output already started; no duplicate write admitted"); }
        r.state = ReservationState::Writing;
        next.ensure_headroom()?;
        *ledger = next;
        Ok(())
    }

    /// Success still retains the shadow until quota explicitly accounts for
    /// it. Unknown/partially failed writes also retain their WHOLE reservation.
    pub(crate) fn finish(&self, key: &ReservationKey, outcome: WriteOutcome) -> Result<()> {
        let mut ledger = self.inner.lock().map_err(|_| anyhow!("storage admission lock poisoned"))?;
        let r = ledger.reservations.get_mut(key).context("reservation missing")?;
        let state = match outcome { WriteOutcome::Written => ReservationState::Written,
            WriteOutcome::Unknown => ReservationState::UnknownWrite };
        if r.state == state { return Ok(()); }
        if r.state != ReservationState::Writing { bail!("invalid write completion transition"); }
        r.state = state;
        Ok(())
    }

    /// Cancellation only frees a reservation that NEVER began writing.
    /// Cancellation of Writing/UnknownWrite/Written is deliberately refused.
    pub(crate) fn cancel_before_write(&self, key: &ReservationKey) -> Result<()> {
        let mut ledger = self.inner.lock().map_err(|_| anyhow!("storage admission lock poisoned"))?;
        let r = ledger.reservations.get(key).context("reservation missing")?;
        if r.state != ReservationState::Planned { bail!("started writes retain their reservation until observed"); }
        ledger.reservations.remove(key);
        Ok(())
    }

    pub(crate) fn headroom_bytes(&self, capacity_domain: &str) -> Result<i128> {
        self.inner.lock().map_err(|_| anyhow!("storage admission lock poisoned"))?.headroom(capacity_domain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(entry: &str, object: &str, bytes: u64) -> OutputSpec {
        OutputSpec { entry_id: entry.into(), mode: ExecutionMode::Minimal,
            destination_remote: "disk:".into(), capacity_domain: "disk".into(),
            object_id: object.into(), ownership: OutputOwnership::Created,
            method: CreationMethod::ServerCopy, size: ObjectSize::Stored { whole_object_bytes: bytes },
            borrowed_from: None, logical_move_in_bytes: bytes,
            logical_move_out_bytes: BTreeMap::from([("disk".into(), bytes)]),
            local_scratch_bytes: Some(0) }
    }

    fn quota(free: u64) -> CapacityObservation {
        CapacityObservation { remote: "disk:".into(), capacity_domain: "disk".into(),
            total_bytes: Some(1000), free_bytes: Some(free),
            metadata_reserve_bytes: 0, reserved_bytes: 0 }
    }

    fn key(s: &OutputSpec) -> ReservationKey {
        ReservationKey { migration: "migration".into(), entry: s.entry_id.clone(),
            attempt: "attempt".into(), object: s.object_id.clone() }
    }

    fn fresh(c: &StorageCoordinator, free: u64, accounted: Vec<ReservationKey>) -> FreshQuota {
        FreshQuota { ticket: c.begin_observation().unwrap(), quotas: vec![quota(free)],
            accounted_outputs: accounted }
    }

    #[test]
    fn cumulative_original_keep_has_no_logical_source_credit() {
        let a = spec("a", "a", 40);
        let b = spec("b", "b", 40);
        let mut alias = quota(70); alias.remote = "alias:".into();
        let p = preview_storage(&[quota(70), alias], &[a, b]);
        assert!(!p.admitted);
        assert_eq!(p.disks.len(), 1);
        assert_eq!(p.disks[0].additional_created_bytes, Some(80));
        assert_eq!(p.disks[0].logical_after_retire_used_bytes, Some(930));
        assert_eq!(p.disks[0].peak_used_bytes, Some(1010));
        assert_eq!(p.disks[0].headroom_bytes, Some(-10));
    }

    #[test]
    fn borrowed_references_add_zero_but_server_copy_allocates_whole_object() {
        let mut s = spec("a", "blob", 90);
        assert_eq!(calculate_output_cost(&s).unwrap().actual_created_bytes, 90);
        s.ownership = OutputOwnership::BorrowedReference; s.method = CreationMethod::Reference;
        s.borrowed_from = Some((s.capacity_domain.clone(), s.object_id.clone()));
        s.logical_move_in_bytes = 0; s.logical_move_out_bytes.clear();
        let mut other = s.clone(); other.entry_id = "b".into();
        let p = preview_storage(&[quota(0)], &[s, other]);
        assert!(p.admitted, "{p:?}");
        assert_eq!(p.disks[0].additional_created_bytes, Some(0));
    }

    #[test]
    fn unknown_blob_and_overflow_are_not_fabricated_estimates() {
        let mut s = spec("a", "blob", 90);
        s.size = ObjectSize::Unknown { reason: "reencoded blob layout not frozen".into() };
        let p = preview_storage(&[quota(100)], &[s.clone()]);
        assert!(!p.admitted);
        assert_eq!(p.disks[0].additional_created_bytes, None);
        assert_eq!(p.disks[0].headroom_bytes, None);
        s.size = ObjectSize::Encrypted { plaintext_bytes: u64::MAX, header_bytes: 32,
            block_bytes: 65536, overhead_per_block: 16 };
        assert!(calculate_output_cost(&s).is_err());
        s.size = ObjectSize::Encrypted { plaintext_bytes: 65537, header_bytes: 32,
            block_bytes: 65536, overhead_per_block: 16 };
        assert_eq!(calculate_output_cost(&s).unwrap().actual_created_bytes, 65601);
    }

    #[test]
    fn planned_runtime_deduction_is_once_and_unknown_cancel_keeps_shadow() {
        let c = StorageCoordinator::default();
        c.refresh(fresh(&c, 100, vec![])).unwrap();
        let s = spec("a", "a", 40); let k = key(&s);
        let output = calculate_output_cost(&s).unwrap();
        c.reserve_plan(&[(k.clone(), output.clone())]).unwrap();
        assert_eq!(c.headroom_bytes("disk").unwrap(), 60);
        c.admit(&k, &output, fresh(&c, 100, vec![])).unwrap();
        assert_eq!(c.headroom_bytes("disk").unwrap(), 60);
        c.finish(&k, WriteOutcome::Unknown).unwrap();
        assert!(c.cancel_before_write(&k).is_err());
        c.refresh(fresh(&c, 60, vec![])).unwrap();
        assert_eq!(c.headroom_bytes("disk").unwrap(), 20);
        c.refresh(fresh(&c, 60, vec![k.clone()])).unwrap();
        assert_eq!(c.headroom_bytes("disk").unwrap(), 60);
        // A later lagging quota response must not keep the previous credit.
        c.refresh(fresh(&c, 100, vec![])).unwrap();
        assert_eq!(c.headroom_bytes("disk").unwrap(), 60);
        assert!(c.admit(&k, &output, fresh(&c, 100, vec![])).is_err());
    }

    #[test]
    fn stale_observations_and_atomic_overbudget_reservations_fail_closed() {
        let c = StorageCoordinator::default();
        let old = fresh(&c, 100, vec![]);
        c.refresh(fresh(&c, 50, vec![])).unwrap();
        assert!(c.refresh(old).is_err());
        let a = spec("a", "a", 30); let b = spec("b", "b", 30);
        assert!(c.reserve_plan(&[(key(&a), calculate_output_cost(&a).unwrap()),
            (key(&b), calculate_output_cost(&b).unwrap())]).is_err());
        assert_eq!(c.headroom_bytes("disk").unwrap(), 50);
    }
}
