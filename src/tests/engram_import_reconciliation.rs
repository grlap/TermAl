// Owns the strict codec and recording-rule witnesses of the host
// import-reconciliation journal (engram_import_reconciliation.rs). Does not
// cover its persistence, lazy activation, restore or any active path.
use super::*;

fn scope() -> EngramImportReconciliationScope {
    EngramImportReconciliationScope {
        store_association: "store-association-1".to_owned(),
        project_id: "github.com/example/project".to_owned(),
        association_id: "association-receipt-1".to_owned(),
        owner_id: None,
    }
}

fn exact_binding() -> EngramImportOriginalBinding {
    EngramImportOriginalBinding::Exact(EngramImportExactBinding {
        work_id: "01a13607-a31e-7ca2-bab8-000000000001".to_owned(),
        root_execution_id: None,
        run_id: "01a13607-a31e-7ca2-bab8-000000000002".to_owned(),
        run_generation: 1,
        claim_id: "01a13607-a31e-7ca2-bab8-000000000003".to_owned(),
        claim_fence: 1,
    })
}

fn owner(id: &str) -> EngramImportResponsibilityOwner {
    EngramImportResponsibilityOwner {
        schema_version: ENGRAM_IMPORT_RECONCILIATION_RECORD_VERSION,
        record_id: id.to_owned(),
        scope: scope(),
        association: EngramImportAssociationState::Admitted,
        enrollment_receipt_ref: Some("enrollment-receipt-1".to_owned()),
        owner_id: "owner-1".to_owned(),
        predecessor_association_ref: None,
        successor_association_ref: None,
        original_binding: exact_binding(),
        boot_incarnation: "boot-1".to_owned(),
        enrollment_revision: 1,
        coverage_version: 1,
        coverage: EngramImportCoverage::Complete,
        settlement_refs: Vec::new(),
    }
}

fn fence(id: &str, generation: u64) -> EngramImportLocalFence {
    EngramImportLocalFence {
        schema_version: ENGRAM_IMPORT_RECONCILIATION_RECORD_VERSION,
        record_id: id.to_owned(),
        scope: scope(),
        hold_id: "hold-1".to_owned(),
        candidate_id: "candidate-1".to_owned(),
        candidate_revision: 1,
        scope_revision: 1,
        hold_revision: 1,
        affected_scope_ref: "affected-scope-page-1".to_owned(),
        host_incarnation: "boot-1".to_owned(),
        fence_generation: generation,
        phase: EngramImportFencePhase::Installing,
        prepared_manifest_ref: None,
        attestation_ref: None,
        terminal_decision_ref: None,
    }
}

fn entry(id: &str, state: EngramImportEntryState) -> EngramImportResponsibilityEntry {
    EngramImportResponsibilityEntry {
        schema_version: ENGRAM_IMPORT_RECONCILIATION_RECORD_VERSION,
        record_id: id.to_owned(),
        scope: scope(),
        owner_id: "owner-1".to_owned(),
        class: EngramImportResponsibilityClass::Checkpoint,
        original_binding: exact_binding(),
        existing_ref: "session-obligation-1".to_owned(),
        revision: 1,
        state,
        terminal_evidence_ref: (state == EngramImportEntryState::Terminal)
            .then(|| "terminal-evidence-1".to_owned()),
    }
}

fn snapshot(id: &str, entry_refs: &[&str], disposition: EngramImportReadiness) -> EngramImportResponsibilitySnapshot {
    EngramImportResponsibilitySnapshot {
        schema_version: ENGRAM_IMPORT_RECONCILIATION_RECORD_VERSION,
        record_id: id.to_owned(),
        scope: scope(),
        owner_set_revision: 1,
        scope_revision: 1,
        enrollment_revision: 1,
        host_incarnation: "boot-1".to_owned(),
        fence_generation: 1,
        challenge_id: Some("challenge-1".to_owned()),
        inventory_version: 1,
        entry_refs: entry_refs.iter().map(|key| (*key).to_owned()).collect(),
        entry_count: entry_refs.len() as u64,
        entry_digest: "digest-1".to_owned(),
        coverage_gaps: Vec::new(),
        disposition,
        persisted_content_ref: None,
    }
}

fn one_of_each() -> EngramImportReconciliationJournal {
    let mut journal = EngramImportReconciliationJournal::activated_empty();
    journal.record(EngramImportReconciliationRecord::EnrolledResponsibilityOwner(owner("owner-record-1"))).unwrap();
    journal.record(EngramImportReconciliationRecord::LocalImportFence(fence("fence-1", 1))).unwrap();
    journal.record(EngramImportReconciliationRecord::ResponsibilityEntry(
        entry("entry-1", EngramImportEntryState::Terminal))).unwrap();
    journal.record(EngramImportReconciliationRecord::ResponsibilitySnapshot(snapshot(
        "manifest-1", &["responsibility_entry/entry-1"], EngramImportReadiness::Pending))).unwrap();
    journal
}

fn owner_value() -> Value {
    serde_json::to_value(EngramImportReconciliationRecord::EnrolledResponsibilityOwner(owner("owner-record-1"))).unwrap()
}

fn decode_one(key: &str, record: Value) -> Result<EngramImportReconciliationJournal> {
    let mut journal = serde_json::Map::new();
    journal.insert(key.to_owned(), record);
    EngramImportReconciliationJournal::decode(&Value::Object(journal))
}

#[test]
fn every_variant_round_trips_exactly() {
    let journal = one_of_each();
    let encoded = journal.to_value().unwrap();
    let decoded = EngramImportReconciliationJournal::decode(&encoded).unwrap();
    assert_eq!(decoded, journal);
    assert_eq!(serde_json::to_vec(&decoded.to_value().unwrap()).unwrap(), serde_json::to_vec(&encoded).unwrap());
    assert_eq!(encoded.as_object().unwrap().keys().cloned().collect::<Vec<_>>(), vec![
        "enrolled_responsibility_owner/owner-record-1", "local_import_fence/fence-1",
        "responsibility_entry/entry-1", "responsibility_snapshot/manifest-1",
    ]);
}

#[test]
fn the_journal_projects_one_metadata_unit_per_record() {
    let journal = one_of_each();
    let units = metadata_value_to_units(json!({ "engramImportReconciliation": journal.to_value().unwrap() })).unwrap();
    let descriptor = &units[&("engramImportReconciliation".to_owned(), String::new())];
    assert_eq!(descriptor, &("object".to_owned(), "4".to_owned()));
    for key in journal.records.keys() {
        let (kind, _) = &units[&("engramImportReconciliation".to_owned(), format!("/{key}"))];
        assert_eq!(kind, "entry");
    }
    assert_eq!(units.len(), 5);
}

#[test]
fn decoding_refuses_an_unknown_variant_field_or_missing_field() {
    let key = "enrolled_responsibility_owner/owner-record-1";
    let mut unknown_variant = owner_value();
    unknown_variant["variant"] = json!("unknown_responsibility_kind");
    assert!(decode_one(key, unknown_variant).is_err(), "an unknown variant");
    let mut unknown_field = owner_value();
    unknown_field["routingToken"] = json!("must-never-be-carried");
    assert!(decode_one(key, unknown_field).is_err(), "an unknown field, such as a routing token");
    let mut nested_unknown = owner_value();
    nested_unknown["scope"]["credential"] = json!("must-never-be-carried");
    assert!(decode_one(key, nested_unknown).is_err(), "an unknown nested field");
    let mut missing = owner_value();
    missing.as_object_mut().unwrap().remove("coverage");
    assert!(decode_one(key, missing).is_err(), "a missing field");
}

#[test]
fn decoding_refuses_an_unsupported_schema_version() {
    let mut future = owner_value();
    future["schemaVersion"] = json!(ENGRAM_IMPORT_RECONCILIATION_RECORD_VERSION + 1);
    assert!(decode_one("enrolled_responsibility_owner/owner-record-1", future).is_err());
}

#[test]
fn decoding_refuses_a_key_that_does_not_match_its_records_identity() {
    for key in ["enrolled_responsibility_owner/another-record", "local_import_fence/owner-record-1", "owner-record-1"] {
        assert!(decode_one(key, owner_value()).is_err(), "{key}");
    }
}

#[test]
fn decoding_refuses_an_empty_overlong_or_control_character_identity() {
    for id in [String::new(), "x".repeat(ENGRAM_IMPORT_RECONCILIATION_MAX_ID_CHARS + 1), "a\nb".to_owned()] {
        let record = EngramImportReconciliationRecord::EnrolledResponsibilityOwner(owner(&id));
        let value = serde_json::to_value(&record).unwrap();
        assert!(decode_one(&record.key(), value).is_err(), "{id:?}");
    }
}

#[test]
fn a_journal_over_its_record_limit_is_refused_and_recording_past_it_changes_nothing() {
    let mut journal = EngramImportReconciliationJournal::activated_empty();
    for number in 0..ENGRAM_IMPORT_RECONCILIATION_MAX_RECORDS {
        journal.record(EngramImportReconciliationRecord::ResponsibilityEntry(
            entry(&format!("entry-{number}"), EngramImportEntryState::Outstanding))).unwrap();
    }
    let full = journal.clone();
    assert!(journal.record(EngramImportReconciliationRecord::ResponsibilityEntry(
        entry("one-too-many", EngramImportEntryState::Outstanding))).is_err());
    assert_eq!(journal, full, "the refused record left the journal unchanged");
    let mut encoded = full.to_value().unwrap();
    let extra = EngramImportReconciliationRecord::ResponsibilityEntry(entry("one-too-many", EngramImportEntryState::Outstanding));
    encoded[extra.key()] = serde_json::to_value(&extra).unwrap();
    assert!(EngramImportReconciliationJournal::decode(&encoded).is_err());
}

#[test]
fn a_record_or_journal_over_its_byte_limit_is_refused_without_change() {
    let mut oversized = entry("oversized", EngramImportEntryState::Outstanding);
    oversized.existing_ref = "r".repeat(ENGRAM_IMPORT_RECONCILIATION_MAX_RECORD_BYTES);
    let mut journal = EngramImportReconciliationJournal::activated_empty();
    assert!(journal.record(EngramImportReconciliationRecord::ResponsibilityEntry(oversized)).is_err());
    assert!(journal.records.is_empty());
    // Equal-sized records, each within the per-record limit, reach the total
    // limit: exactly as many as fit are recorded, and the next is refused.
    let large = |number: usize| {
        let mut record = entry(&format!("large-{number:04}"), EngramImportEntryState::Outstanding);
        record.existing_ref = "r".repeat(60_000);
        EngramImportReconciliationRecord::ResponsibilityEntry(record)
    };
    let size = serde_json::to_vec(&large(0)).unwrap().len();
    assert!(size <= ENGRAM_IMPORT_RECONCILIATION_MAX_RECORD_BYTES);
    let fits = ENGRAM_IMPORT_RECONCILIATION_MAX_BYTES / size;
    for number in 0..fits {
        journal.record(large(number)).unwrap();
    }
    let before = journal.clone();
    assert!(journal.record(large(fits)).is_err(), "the total limit");
    assert_eq!(journal, before);
}

#[test]
fn fence_generation_overflow_refuses_and_a_scope_fence_never_moves_backwards() {
    assert_eq!(engram_import_next_fence_generation(1).unwrap(), 2);
    assert!(engram_import_next_fence_generation(u64::MAX).is_err(), "overflow refuses, never wraps");
    let mut journal = EngramImportReconciliationJournal::activated_empty();
    journal.record(EngramImportReconciliationRecord::LocalImportFence(fence("fence-2", 2))).unwrap();
    let before = journal.clone();
    assert!(journal.record(EngramImportReconciliationRecord::LocalImportFence(fence("fence-1", 1))).is_err(),
        "a lower generation for the same scope is refused");
    assert_eq!(journal, before);
}

#[test]
fn coverage_is_complete_only_for_an_admitted_exactly_bound_owner() {
    for (association, binding) in [
        (EngramImportAssociationState::Pending, exact_binding()),
        (EngramImportAssociationState::Legacy, exact_binding()),
        (EngramImportAssociationState::Foreign, exact_binding()),
        (EngramImportAssociationState::Admitted, EngramImportOriginalBinding::Unknown),
        (EngramImportAssociationState::Admitted, EngramImportOriginalBinding::Legacy),
    ] {
        let mut claimed = owner("owner-record-1");
        claimed.association = association;
        claimed.original_binding = binding.clone();
        let mut journal = EngramImportReconciliationJournal::activated_empty();
        assert!(journal.record(EngramImportReconciliationRecord::EnrolledResponsibilityOwner(claimed.clone())).is_err(),
            "{association:?} {binding:?}: complete coverage is refused");
        assert!(journal.records.is_empty());
        claimed.coverage = EngramImportCoverage::Unknown;
        assert_eq!(journal.record(EngramImportReconciliationRecord::EnrolledResponsibilityOwner(claimed)).unwrap(),
            EngramImportRecordOutcome::Recorded, "{association:?}: recorded with coverage unknown");
    }
    let mut admitted_without_receipt = owner("owner-record-1");
    admitted_without_receipt.enrollment_receipt_ref = None;
    assert!(EngramImportReconciliationJournal::activated_empty()
        .record(EngramImportReconciliationRecord::EnrolledResponsibilityOwner(admitted_without_receipt)).is_err(),
        "an admitted association carries its enrollment receipt");
}

#[test]
fn recording_the_same_record_again_is_idempotent() {
    let mut journal = one_of_each();
    let before = journal.clone();
    assert_eq!(journal.record(EngramImportReconciliationRecord::LocalImportFence(fence("fence-1", 1))).unwrap(),
        EngramImportRecordOutcome::Unchanged);
    assert_eq!(journal, before);
}

#[test]
fn a_changed_body_under_the_same_identity_conflicts_and_changes_nothing() {
    let mut changed_candidate = fence("fence-1", 1);
    changed_candidate.candidate_id = "candidate-2".to_owned();
    let mut changed_store = fence("fence-1", 1);
    changed_store.scope.store_association = "store-association-2".to_owned();
    let mut changed_project = fence("fence-1", 1);
    changed_project.scope.project_id = "github.com/example/other".to_owned();
    let mut changed_owner = owner("owner-record-1");
    changed_owner.owner_id = "owner-2".to_owned();
    for changed in [
        EngramImportReconciliationRecord::LocalImportFence(changed_candidate),
        EngramImportReconciliationRecord::LocalImportFence(changed_store),
        EngramImportReconciliationRecord::LocalImportFence(changed_project),
        EngramImportReconciliationRecord::EnrolledResponsibilityOwner(changed_owner),
    ] {
        let mut journal = one_of_each();
        let before = journal.clone();
        assert!(journal.record(changed.clone()).is_err(), "{}", changed.key());
        assert_eq!(journal, before, "{}", changed.key());
    }
}

#[test]
fn a_ready_snapshot_cannot_be_built_from_incomplete_coverage() {
    let ready = |journal: &mut EngramImportReconciliationJournal, snapshot| {
        journal.record(EngramImportReconciliationRecord::ResponsibilitySnapshot(snapshot))
    };
    // Control: complete coverage, every entry terminal.
    let mut journal = one_of_each();
    ready(&mut journal, snapshot("manifest-ready", &["responsibility_entry/entry-1"], EngramImportReadiness::Ready)).unwrap();

    let mut gaps = snapshot("manifest-gaps", &["responsibility_entry/entry-1"], EngramImportReadiness::Ready);
    gaps.coverage_gaps = vec!["legacy-unbound-owner".to_owned()];
    let mut miscounted = snapshot("manifest-count", &["responsibility_entry/entry-1"], EngramImportReadiness::Ready);
    miscounted.entry_count = 2;
    for (label, refused) in [
        ("a coverage gap", gaps),
        ("a miscounted manifest", miscounted),
        ("a missing entry", snapshot("manifest-missing", &["responsibility_entry/absent"], EngramImportReadiness::Ready)),
    ] {
        let mut journal = one_of_each();
        let before = journal.clone();
        assert!(ready(&mut journal, refused).is_err(), "{label}");
        assert_eq!(journal, before, "{label}");
    }
    let mut outstanding = one_of_each();
    outstanding.record(EngramImportReconciliationRecord::ResponsibilityEntry(
        entry("entry-2", EngramImportEntryState::Outstanding))).unwrap();
    assert!(ready(&mut outstanding, snapshot("manifest-outstanding",
        &["responsibility_entry/entry-1", "responsibility_entry/entry-2"], EngramImportReadiness::Ready)).is_err(),
        "an outstanding entry");
    let mut unknown_owner = EngramImportReconciliationJournal::activated_empty();
    let mut uncovered = owner("owner-record-1");
    uncovered.coverage = EngramImportCoverage::Unknown;
    unknown_owner.record(EngramImportReconciliationRecord::EnrolledResponsibilityOwner(uncovered)).unwrap();
    unknown_owner.record(EngramImportReconciliationRecord::ResponsibilityEntry(
        entry("entry-1", EngramImportEntryState::Terminal))).unwrap();
    assert!(ready(&mut unknown_owner, snapshot("manifest-unknown-owner",
        &["responsibility_entry/entry-1"], EngramImportReadiness::Ready)).is_err(),
        "an owner in scope whose coverage is unknown");
    // A pending snapshot records whatever the coverage.
    assert_eq!(ready(&mut unknown_owner, snapshot("manifest-pending",
        &["responsibility_entry/entry-1"], EngramImportReadiness::Pending)).unwrap(), EngramImportRecordOutcome::Recorded);
}

fn other_scope() -> EngramImportReconciliationScope {
    EngramImportReconciliationScope { association_id: "association-receipt-2".to_owned(), ..scope() }
}

#[test]
fn a_later_uncovered_owner_cannot_break_an_accepted_ready_snapshot() {
    let mut journal = one_of_each();
    journal.record(EngramImportReconciliationRecord::ResponsibilitySnapshot(snapshot(
        "manifest-ready", &["responsibility_entry/entry-1"], EngramImportReadiness::Ready))).unwrap();
    let mut uncovered = owner("owner-record-2");
    uncovered.owner_id = "owner-2".to_owned();
    uncovered.coverage = EngramImportCoverage::Unknown;
    let before = journal.clone();
    assert!(journal.record(EngramImportReconciliationRecord::EnrolledResponsibilityOwner(uncovered.clone())).is_err(),
        "an uncovered owner in the scope of a ready snapshot is refused");
    assert_eq!(journal, before, "the refusal left the journal unchanged");
    assert_eq!(EngramImportReconciliationJournal::decode(&journal.to_value().unwrap()).unwrap(), journal,
        "what recording accepted, a decoder accepts");
    // A decoder refuses the same combination.
    let mut encoded = journal.to_value().unwrap();
    let refused = EngramImportReconciliationRecord::EnrolledResponsibilityOwner(uncovered.clone());
    encoded[refused.key()] = serde_json::to_value(&refused).unwrap();
    assert!(EngramImportReconciliationJournal::decode(&encoded).is_err());
    // Controls: the same owner is recorded in another scope, and in a scope
    // with only a pending snapshot; both journals round-trip.
    let mut elsewhere = uncovered.clone();
    elsewhere.scope = other_scope();
    assert_eq!(journal.record(EngramImportReconciliationRecord::EnrolledResponsibilityOwner(elsewhere)).unwrap(),
        EngramImportRecordOutcome::Recorded);
    assert_eq!(EngramImportReconciliationJournal::decode(&journal.to_value().unwrap()).unwrap(), journal);
    let mut pending_only = one_of_each();
    assert_eq!(pending_only.record(EngramImportReconciliationRecord::EnrolledResponsibilityOwner(uncovered)).unwrap(),
        EngramImportRecordOutcome::Recorded);
    assert_eq!(EngramImportReconciliationJournal::decode(&pending_only.to_value().unwrap()).unwrap(), pending_only);
}

#[test]
fn a_ready_snapshot_cannot_be_covered_by_another_scopes_entries() {
    let mut journal = one_of_each();
    let mut foreign = entry("entry-other", EngramImportEntryState::Terminal);
    foreign.scope = other_scope();
    journal.record(EngramImportReconciliationRecord::ResponsibilityEntry(foreign)).unwrap();
    let cross = EngramImportReconciliationRecord::ResponsibilitySnapshot(snapshot(
        "manifest-cross", &["responsibility_entry/entry-other"], EngramImportReadiness::Ready));
    let before = journal.clone();
    assert!(journal.record(cross.clone()).is_err(), "a terminal entry of another scope is not coverage");
    assert_eq!(journal, before);
    let mut encoded = journal.to_value().unwrap();
    encoded[cross.key()] = serde_json::to_value(&cross).unwrap();
    assert!(EngramImportReconciliationJournal::decode(&encoded).is_err(), "a decoder refuses it too");
    // Control: the entry's own scope may cite it.
    let mut own = snapshot("manifest-own", &["responsibility_entry/entry-other"], EngramImportReadiness::Ready);
    own.scope = other_scope();
    assert_eq!(journal.record(EngramImportReconciliationRecord::ResponsibilitySnapshot(own)).unwrap(),
        EngramImportRecordOutcome::Recorded);
}

#[test]
fn the_cached_byte_total_matches_a_recomputation_after_recording() {
    let mut journal = one_of_each();
    // Recorded, unchanged and refused calls all leave the cache exact.
    journal.record(EngramImportReconciliationRecord::ResponsibilityEntry(
        entry("entry-2", EngramImportEntryState::Outstanding))).unwrap();
    assert_eq!(journal.record(EngramImportReconciliationRecord::LocalImportFence(fence("fence-1", 1))).unwrap(),
        EngramImportRecordOutcome::Unchanged);
    let mut conflicting = fence("fence-1", 1);
    conflicting.candidate_id = "candidate-2".to_owned();
    assert!(journal.record(EngramImportReconciliationRecord::LocalImportFence(conflicting)).is_err());
    assert!(journal.record(EngramImportReconciliationRecord::LocalImportFence(fence("fence-0", 0))).is_err());
    assert_eq!(journal.bytes, journal.measure().unwrap());
    journal.validate().unwrap();
    // A decoder recomputes the total from the records, never trusting a cache.
    let decoded = EngramImportReconciliationJournal::decode(&journal.to_value().unwrap()).unwrap();
    assert_eq!(decoded.bytes, journal.bytes);
    assert_eq!(decoded, journal);
}

#[test]
fn not_activated_is_not_an_empty_journal() {
    assert_ne!(EngramImportReconciliationState::NotActivated,
        EngramImportReconciliationState::Activated(Arc::new(EngramImportReconciliationJournal::activated_empty())));
}
