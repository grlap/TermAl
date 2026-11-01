// Owns the strict, versioned host import-reconciliation journal: its four
// record variants, their identity, bounds and the rules for recording one.
// Does not persist or activate the journal (the lazy successor reader floor
// belongs to the persistence side), and adds no endpoint, enrollment, hold,
// challenge or Ready path. Every record holds references and fingerprints
// only, never routing tokens, credentials or raw request bodies.

#[cfg_attr(not(test), allow(dead_code))]
const ENGRAM_IMPORT_RECONCILIATION_RECORD_VERSION: u32 = 1;
#[cfg_attr(not(test), allow(dead_code))]
/// Local journal limits, declared before any enrollment and stricter than
/// the wire's page and manifest totals.
const ENGRAM_IMPORT_RECONCILIATION_MAX_RECORDS: usize = 4096;
#[cfg_attr(not(test), allow(dead_code))]
const ENGRAM_IMPORT_RECONCILIATION_MAX_RECORD_BYTES: usize = 64 * 1024;
#[cfg_attr(not(test), allow(dead_code))]
const ENGRAM_IMPORT_RECONCILIATION_MAX_BYTES: usize = 16 * 1024 * 1024;
#[cfg_attr(not(test), allow(dead_code))]
const ENGRAM_IMPORT_RECONCILIATION_MAX_ID_CHARS: usize = 256;

#[cfg_attr(not(test), allow(dead_code))]
/// The exact authority scope a record belongs to.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramImportReconciliationScope {
    store_association: String,
    project_id: String,
    association_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_id: Option<String>,
}

#[cfg_attr(not(test), allow(dead_code))]
/// The original work an owner or entry is bound to: exact, or explicitly
/// unknown or legacy. Never a guessed or current-focus binding.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "binding", rename_all = "snake_case")]
enum EngramImportOriginalBinding {
    Exact(EngramImportExactBinding),
    Unknown,
    Legacy,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramImportExactBinding {
    work_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root_execution_id: Option<String>,
    run_id: String,
    run_generation: i64,
    claim_id: String,
    claim_fence: u64,
}

#[cfg_attr(not(test), allow(dead_code))]
/// How an owner's store association stands. Only an admitted association
/// can carry complete coverage.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramImportAssociationState {
    Admitted,
    Pending,
    Legacy,
    Foreign,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramImportCoverage {
    Unknown,
    Complete,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramImportResponsibilityOwner {
    schema_version: u32,
    record_id: String,
    scope: EngramImportReconciliationScope,
    association: EngramImportAssociationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enrollment_receipt_ref: Option<String>,
    owner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    predecessor_association_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    successor_association_ref: Option<String>,
    original_binding: EngramImportOriginalBinding,
    boot_incarnation: String,
    enrollment_revision: u64,
    coverage_version: u32,
    coverage: EngramImportCoverage,
    settlement_refs: Vec<String>,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramImportFencePhase {
    Installing,
    Settling,
    Prepared,
    DecisionPending,
    Reconciled,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramImportLocalFence {
    schema_version: u32,
    record_id: String,
    scope: EngramImportReconciliationScope,
    hold_id: String,
    candidate_id: String,
    candidate_revision: u64,
    scope_revision: u64,
    hold_revision: u64,
    affected_scope_ref: String,
    host_incarnation: String,
    fence_generation: u64,
    phase: EngramImportFencePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prepared_manifest_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attestation_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_decision_ref: Option<String>,
}

#[cfg_attr(not(test), allow(dead_code))]
/// The known classes of host responsibility a journal entry can name.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramImportResponsibilityClass {
    Checkpoint,
    ControlOperation,
    RetainedHead,
    CarriedCheck,
    RunningProcess,
    SourceObservation,
    RuntimeChild,
    RootCleanup,
    HistoricOwner,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramImportEntryState {
    Outstanding,
    Unknown,
    Terminal,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramImportResponsibilityEntry {
    schema_version: u32,
    record_id: String,
    scope: EngramImportReconciliationScope,
    owner_id: String,
    class: EngramImportResponsibilityClass,
    original_binding: EngramImportOriginalBinding,
    existing_ref: String,
    revision: u64,
    state: EngramImportEntryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_evidence_ref: Option<String>,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramImportReadiness {
    Pending,
    Ready,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramImportResponsibilitySnapshot {
    schema_version: u32,
    record_id: String,
    scope: EngramImportReconciliationScope,
    owner_set_revision: u64,
    scope_revision: u64,
    enrollment_revision: u64,
    host_incarnation: String,
    fence_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    challenge_id: Option<String>,
    inventory_version: u64,
    entry_refs: Vec<String>,
    entry_count: u64,
    entry_digest: String,
    coverage_gaps: Vec<String>,
    disposition: EngramImportReadiness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    persisted_content_ref: Option<String>,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "variant", rename_all = "snake_case")]
enum EngramImportReconciliationRecord {
    EnrolledResponsibilityOwner(EngramImportResponsibilityOwner),
    LocalImportFence(EngramImportLocalFence),
    ResponsibilityEntry(EngramImportResponsibilityEntry),
    ResponsibilitySnapshot(EngramImportResponsibilitySnapshot),
}

#[cfg_attr(not(test), allow(dead_code))]
impl EngramImportReconciliationRecord {
    fn tag(&self) -> &'static str {
        match self {
            Self::EnrolledResponsibilityOwner(_) => "enrolled_responsibility_owner",
            Self::LocalImportFence(_) => "local_import_fence",
            Self::ResponsibilityEntry(_) => "responsibility_entry",
            Self::ResponsibilitySnapshot(_) => "responsibility_snapshot",
        }
    }

    fn record_id(&self) -> &str {
        match self {
            Self::EnrolledResponsibilityOwner(record) => &record.record_id,
            Self::LocalImportFence(record) => &record.record_id,
            Self::ResponsibilityEntry(record) => &record.record_id,
            Self::ResponsibilitySnapshot(record) => &record.record_id,
        }
    }

    /// The stable journal key: `<variant-tag>/<record_id>`.
    fn key(&self) -> String {
        format!("{}/{}", self.tag(), self.record_id())
    }
}

#[cfg_attr(not(test), allow(dead_code))]
/// What recording a record did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramImportRecordOutcome {
    Recorded,
    Unchanged,
}

#[cfg_attr(not(test), allow(dead_code))]
/// The journal: stable record keys to strict records. There is deliberately
/// no `Default`: an empty journal exists only once activated, and absence
/// never stands in for it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramImportReconciliationJournal {
    records: BTreeMap<String, EngramImportReconciliationRecord>,
    /// The sum of the records' serialized sizes, which the total limit
    /// bounds; kept so that recording one record serializes only that one.
    bytes: usize,
}

#[cfg_attr(not(test), allow(dead_code))]
/// The host's import-reconciliation state. `NotActivated` is the
/// predecessor format, with coverage unknown; it is not an empty journal.
/// An activated journal is shared, so a state snapshot taken under the state
/// lock costs a reference count, never a copy of the journal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum EngramImportReconciliationState {
    #[default]
    NotActivated,
    Activated(Arc<EngramImportReconciliationJournal>),
}

#[cfg_attr(not(test), allow(dead_code))]
impl EngramImportReconciliationJournal {
    /// The journal a first activation starts from.
    fn activated_empty() -> Self {
        Self { records: BTreeMap::new(), bytes: 0 }
    }

    /// Decode a persisted journal, refusing anything it does not exactly
    /// understand: an unknown variant, field or version, a missing field, a
    /// key that is not its record's identity, a broken relation between
    /// records, or more than the declared limits. Nothing is defaulted.
    fn decode(value: &Value) -> Result<Self> {
        let Value::Object(encoded) = value else {
            bail!("the import-reconciliation journal must be an object");
        };
        if encoded.len() > ENGRAM_IMPORT_RECONCILIATION_MAX_RECORDS {
            bail!("the import-reconciliation journal exceeds its record limit");
        }
        let mut records = BTreeMap::new();
        for (key, raw) in encoded {
            let record: EngramImportReconciliationRecord = serde_json::from_value(raw.clone())
                .context("unsupported import-reconciliation record")?;
            // A shape the decoder only half understood (a defaulted or
            // dropped field) would re-encode differently: refuse it.
            if &serde_json::to_value(&record)? != raw {
                bail!("unsupported or incomplete import-reconciliation record shape");
            }
            records.insert(key.clone(), record);
        }
        let mut journal = Self { records, bytes: 0 };
        journal.bytes = journal.measure()?;
        journal.validate()?;
        Ok(journal)
    }

    fn to_value(&self) -> Result<Value> {
        Ok(serde_json::to_value(&self.records)?)
    }

    /// Every record with its stable key, in key order, for read-only
    /// consumers such as an admission decision. Records change only through
    /// `record`.
    fn records(&self) -> impl Iterator<Item = (&str, &EngramImportReconciliationRecord)> {
        self.records.iter().map(|(key, record)| (key.as_str(), record))
    }

    /// Each record's own rules and its key, summing the records' serialized
    /// sizes against the total limit.
    fn measure(&self) -> Result<usize> {
        let mut bytes = 0usize;
        for (key, record) in &self.records {
            if record.key() != *key {
                bail!("an import-reconciliation record key does not match its record's identity");
            }
            bytes = bytes
                .checked_add(engram_import_validate_record(record)?)
                .filter(|total| *total <= ENGRAM_IMPORT_RECONCILIATION_MAX_BYTES)
                .ok_or_else(|| anyhow!("the import-reconciliation journal exceeds its byte limit"))?;
        }
        Ok(bytes)
    }

    /// Full validation, as a decoder applies it: the limits, every record's
    /// own rules and key, and the relations between records.
    fn validate(&self) -> Result<()> {
        if self.records.len() > ENGRAM_IMPORT_RECONCILIATION_MAX_RECORDS {
            bail!("the import-reconciliation journal exceeds its record limit");
        }
        if self.measure()? != self.bytes {
            bail!("the import-reconciliation journal's recorded size is inconsistent");
        }
        self.validate_relations()
    }

    /// The rules between records: no two fences of one scope share a
    /// generation, and every ready snapshot is fully covered.
    fn validate_relations(&self) -> Result<()> {
        let mut generations = BTreeSet::new();
        for record in self.records.values() {
            match record {
                EngramImportReconciliationRecord::LocalImportFence(fence) => {
                    if !generations.insert((engram_import_scope_identity(&fence.scope), fence.fence_generation)) {
                        bail!("two import fences of one scope share a fence generation");
                    }
                }
                EngramImportReconciliationRecord::ResponsibilitySnapshot(snapshot) => {
                    self.validate_snapshot_coverage(snapshot)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// A ready snapshot needs positively complete coverage: every entry it
    /// names present, of its own scope and terminal, and every owner of its
    /// scope complete.
    fn validate_snapshot_coverage(&self, snapshot: &EngramImportResponsibilitySnapshot) -> Result<()> {
        if snapshot.disposition != EngramImportReadiness::Ready {
            return Ok(());
        }
        let scope = engram_import_scope_identity(&snapshot.scope);
        for entry_ref in &snapshot.entry_refs {
            match self.records.get(entry_ref) {
                Some(EngramImportReconciliationRecord::ResponsibilityEntry(entry))
                    if engram_import_scope_identity(&entry.scope) != scope =>
                {
                    bail!("a ready snapshot names a responsibility of another scope");
                }
                Some(EngramImportReconciliationRecord::ResponsibilityEntry(entry))
                    if entry.state == EngramImportEntryState::Terminal => {}
                Some(EngramImportReconciliationRecord::ResponsibilityEntry(_)) => {
                    bail!("a ready snapshot names a responsibility that is not terminal");
                }
                _ => bail!("a ready snapshot names a responsibility the journal does not hold"),
            }
        }
        if self.records.values().any(|record| matches!(record,
            EngramImportReconciliationRecord::EnrolledResponsibilityOwner(owner)
                if engram_import_scope_identity(&owner.scope) == scope
                    && owner.coverage != EngramImportCoverage::Complete))
        {
            bail!("a ready snapshot's scope has an owner whose coverage is not complete");
        }
        Ok(())
    }

    /// The coverage of every ready snapshot of one scope, as the journal now
    /// stands.
    fn validate_ready_snapshots_of_scope(&self, scope: (&str, &str, &str)) -> Result<()> {
        for record in self.records.values() {
            match record {
                EngramImportReconciliationRecord::ResponsibilitySnapshot(snapshot)
                    if engram_import_scope_identity(&snapshot.scope) == scope =>
                {
                    self.validate_snapshot_coverage(snapshot)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Record `record` under its stable key. The same identity with the same
    /// body is unchanged; a different body under that identity conflicts. A
    /// refusal of any kind leaves the journal exactly as it was.
    fn record(&mut self, record: EngramImportReconciliationRecord) -> Result<EngramImportRecordOutcome> {
        let size = engram_import_validate_record(&record)?;
        let key = record.key();
        if let Some(existing) = self.records.get(&key) {
            if *existing == record {
                return Ok(EngramImportRecordOutcome::Unchanged);
            }
            bail!("import-reconciliation record `{key}` already holds a different body");
        }
        if self.records.len() >= ENGRAM_IMPORT_RECONCILIATION_MAX_RECORDS {
            bail!("the import-reconciliation journal is at its record limit");
        }
        let bytes = self
            .bytes
            .checked_add(size)
            .filter(|total| *total <= ENGRAM_IMPORT_RECONCILIATION_MAX_BYTES)
            .ok_or_else(|| anyhow!("the import-reconciliation journal is at its byte limit"))?;
        if let EngramImportReconciliationRecord::LocalImportFence(fence) = &record {
            let scope = engram_import_scope_identity(&fence.scope);
            if self.records.values().any(|existing| matches!(existing,
                EngramImportReconciliationRecord::LocalImportFence(held)
                    if engram_import_scope_identity(&held.scope) == scope
                        && held.fence_generation >= fence.fence_generation))
            {
                bail!("an import fence's generation must exceed every earlier fence of its scope");
            }
        }
        let scope = {
            let (store, project, association) = engram_import_scope_identity(engram_import_record_scope(&record));
            (store.to_owned(), project.to_owned(), association.to_owned())
        };
        // Any record can bear on the ready snapshots of its scope: a ready
        // snapshot needs its own coverage, and a later owner whose coverage is
        // not complete would leave an earlier ready snapshot uncovered. Check
        // them all with the record in place, and take it out again on refusal,
        // so recording never yields a journal the decoder would refuse.
        self.records.insert(key.clone(), record);
        if let Err(error) = self.validate_ready_snapshots_of_scope((scope.0.as_str(), scope.1.as_str(), scope.2.as_str())) {
            self.records.remove(&key);
            return Err(error);
        }
        self.bytes = bytes;
        Ok(EngramImportRecordOutcome::Recorded)
    }
}

#[cfg_attr(not(test), allow(dead_code))]
/// The scope a record belongs to.
fn engram_import_record_scope(record: &EngramImportReconciliationRecord) -> &EngramImportReconciliationScope {
    match record {
        EngramImportReconciliationRecord::EnrolledResponsibilityOwner(owner) => &owner.scope,
        EngramImportReconciliationRecord::LocalImportFence(fence) => &fence.scope,
        EngramImportReconciliationRecord::ResponsibilityEntry(entry) => &entry.scope,
        EngramImportReconciliationRecord::ResponsibilitySnapshot(snapshot) => &snapshot.scope,
    }
}

#[cfg_attr(not(test), allow(dead_code))]
/// The store, project and association a record's scope names: the part that
/// decides which fences and owners belong together.
fn engram_import_scope_identity(scope: &EngramImportReconciliationScope) -> (&str, &str, &str) {
    (&scope.store_association, &scope.project_id, &scope.association_id)
}

#[cfg_attr(not(test), allow(dead_code))]
/// A record's own rules, checked before it is decoded into or recorded in a
/// journal. Returns its serialized size.
fn engram_import_validate_record(record: &EngramImportReconciliationRecord) -> Result<usize> {
    let schema_version = match record {
        EngramImportReconciliationRecord::EnrolledResponsibilityOwner(owner) => owner.schema_version,
        EngramImportReconciliationRecord::LocalImportFence(fence) => fence.schema_version,
        EngramImportReconciliationRecord::ResponsibilityEntry(entry) => entry.schema_version,
        EngramImportReconciliationRecord::ResponsibilitySnapshot(snapshot) => snapshot.schema_version,
    };
    if schema_version != ENGRAM_IMPORT_RECONCILIATION_RECORD_VERSION {
        bail!("unsupported import-reconciliation record version {schema_version}");
    }
    let id = record.record_id();
    if id.is_empty()
        || id.chars().count() > ENGRAM_IMPORT_RECONCILIATION_MAX_ID_CHARS
        || id.chars().any(char::is_control)
    {
        bail!("an import-reconciliation record identity must be 1 to {} printable characters",
            ENGRAM_IMPORT_RECONCILIATION_MAX_ID_CHARS);
    }
    match record {
        EngramImportReconciliationRecord::EnrolledResponsibilityOwner(owner) => {
            if owner.association == EngramImportAssociationState::Admitted
                && owner.enrollment_receipt_ref.is_none()
            {
                bail!("an admitted owner association carries its enrollment receipt");
            }
            // Only an admitted, exactly bound owner can be complete: pending,
            // legacy and foreign associations, and unknown or legacy work,
            // stay coverage unknown.
            if owner.coverage == EngramImportCoverage::Complete
                && (owner.association != EngramImportAssociationState::Admitted
                    || !matches!(owner.original_binding, EngramImportOriginalBinding::Exact(_)))
            {
                bail!("an owner's coverage can be complete only for an admitted, exactly bound owner");
            }
        }
        EngramImportReconciliationRecord::ResponsibilityEntry(entry) => {
            if (entry.state == EngramImportEntryState::Terminal) != entry.terminal_evidence_ref.is_some() {
                bail!("a responsibility entry carries terminal evidence exactly when it is terminal");
            }
        }
        EngramImportReconciliationRecord::ResponsibilitySnapshot(snapshot) => {
            if snapshot.entry_count != snapshot.entry_refs.len() as u64 {
                bail!("a responsibility snapshot's entry count does not match its entry references");
            }
            if snapshot.disposition == EngramImportReadiness::Ready && !snapshot.coverage_gaps.is_empty() {
                bail!("a ready responsibility snapshot cannot have coverage gaps");
            }
        }
        EngramImportReconciliationRecord::LocalImportFence(_) => {}
    }
    let size = serde_json::to_vec(record)?.len();
    if size > ENGRAM_IMPORT_RECONCILIATION_MAX_RECORD_BYTES {
        bail!("an import-reconciliation record exceeds its byte limit");
    }
    Ok(size)
}

#[cfg_attr(not(test), allow(dead_code))]
/// The next fence generation of a scope. Exhaustion refuses; it never wraps.
fn engram_import_next_fence_generation(current: u64) -> Result<u64> {
    current
        .checked_add(1)
        .ok_or_else(|| anyhow!("the import fence generation is exhausted"))
}
