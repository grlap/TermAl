// Owns the table-driven witnesses of the pure exact-scope import-hold
// decision (engram_import_hold_decision.rs) over the import-reconciliation
// journal state (engram_import_reconciliation.rs).
// Does not cover the journal's codec or persistence, any dispatch or handoff
// path, or hold installation and release.
use super::*;

const STORE: &str = "store-association-1";
const PROJECT: &str = "github.com/example/project";
const BOOT: &str = "boot-1";

fn scope(store: &str, project: &str, owner_id: Option<&str>) -> EngramImportReconciliationScope {
    EngramImportReconciliationScope {
        store_association: store.to_owned(),
        project_id: project.to_owned(),
        association_id: "association-receipt-1".to_owned(),
        owner_id: owner_id.map(str::to_owned),
    }
}

fn exact(n: u32) -> EngramImportOriginalBinding {
    EngramImportOriginalBinding::Exact(EngramImportExactBinding {
        work_id: format!("01a13607-a31e-7ca2-bab8-0000000001{n:02}"),
        root_execution_id: None,
        run_id: format!("01a13607-a31e-7ca2-bab8-0000000002{n:02}"),
        run_generation: 1,
        claim_id: format!("01a13607-a31e-7ca2-bab8-0000000003{n:02}"),
        claim_fence: 1,
    })
}

fn owner(
    record_id: &str,
    owner_id: &str,
    binding: EngramImportOriginalBinding,
    association: EngramImportAssociationState,
    coverage: EngramImportCoverage,
) -> EngramImportReconciliationRecord {
    EngramImportReconciliationRecord::EnrolledResponsibilityOwner(EngramImportResponsibilityOwner {
        schema_version: ENGRAM_IMPORT_RECONCILIATION_RECORD_VERSION,
        record_id: record_id.to_owned(),
        scope: scope(STORE, PROJECT, Some(owner_id)),
        association,
        enrollment_receipt_ref: Some("enrollment-receipt-1".to_owned()),
        owner_id: owner_id.to_owned(),
        predecessor_association_ref: None,
        successor_association_ref: None,
        original_binding: binding,
        boot_incarnation: BOOT.to_owned(),
        enrollment_revision: 1,
        coverage_version: 1,
        coverage,
        settlement_refs: Vec::new(),
    })
}

fn covered_owner(
    record_id: &str,
    owner_id: &str,
    binding: EngramImportOriginalBinding,
) -> EngramImportReconciliationRecord {
    owner(
        record_id,
        owner_id,
        binding,
        EngramImportAssociationState::Admitted,
        EngramImportCoverage::Complete,
    )
}

struct FenceSpec<'a> {
    record_id: &'a str,
    store: &'a str,
    project: &'a str,
    owner_id: Option<&'a str>,
    incarnation: &'a str,
    generation: u64,
    phase: EngramImportFencePhase,
    terminal_decision_ref: Option<&'a str>,
}

fn whole_scope_fence(record_id: &str) -> FenceSpec<'_> {
    FenceSpec {
        record_id,
        store: STORE,
        project: PROJECT,
        owner_id: None,
        incarnation: BOOT,
        generation: 1,
        phase: EngramImportFencePhase::Installing,
        terminal_decision_ref: None,
    }
}

fn fence(spec: FenceSpec<'_>) -> EngramImportReconciliationRecord {
    EngramImportReconciliationRecord::LocalImportFence(EngramImportLocalFence {
        schema_version: ENGRAM_IMPORT_RECONCILIATION_RECORD_VERSION,
        record_id: spec.record_id.to_owned(),
        scope: scope(spec.store, spec.project, spec.owner_id),
        hold_id: format!("hold-{}", spec.record_id),
        candidate_id: "candidate-1".to_owned(),
        candidate_revision: 1,
        scope_revision: 1,
        hold_revision: 1,
        affected_scope_ref: "affected-scope-page-1".to_owned(),
        host_incarnation: spec.incarnation.to_owned(),
        fence_generation: spec.generation,
        phase: spec.phase,
        prepared_manifest_ref: None,
        attestation_ref: None,
        terminal_decision_ref: spec.terminal_decision_ref.map(str::to_owned),
    })
}

fn entry(
    record_id: &str,
    owner_id: &str,
    binding: EngramImportOriginalBinding,
    state: EngramImportEntryState,
) -> EngramImportReconciliationRecord {
    entry_in(STORE, PROJECT, record_id, owner_id, binding, state)
}

fn entry_in(
    store: &str,
    project: &str,
    record_id: &str,
    owner_id: &str,
    binding: EngramImportOriginalBinding,
    state: EngramImportEntryState,
) -> EngramImportReconciliationRecord {
    EngramImportReconciliationRecord::ResponsibilityEntry(EngramImportResponsibilityEntry {
        schema_version: ENGRAM_IMPORT_RECONCILIATION_RECORD_VERSION,
        record_id: record_id.to_owned(),
        scope: scope(store, project, Some(owner_id)),
        owner_id: owner_id.to_owned(),
        class: EngramImportResponsibilityClass::RetainedHead,
        original_binding: binding,
        existing_ref: format!("existing-{record_id}"),
        revision: 1,
        state,
        terminal_evidence_ref: (state == EngramImportEntryState::Terminal)
            .then(|| format!("terminal-evidence-{record_id}")),
    })
}

/// Two fully covered owners and a fence of owner-2, which a candidate of
/// owner-1 passes, plus `entries`.
fn owner_two_fence_with(
    entries: Vec<EngramImportReconciliationRecord>,
) -> EngramImportReconciliationState {
    owner_two_fence(
        Some(covered_owner("owner-record-2", "owner-2", exact(2))),
        entries,
    )
}

/// A fully covered owner-1, a fence of owner-2, owner-2's enrollment if any,
/// and `entries`.
fn owner_two_fence(
    owner_two: Option<EngramImportReconciliationRecord>,
    entries: Vec<EngramImportReconciliationRecord>,
) -> EngramImportReconciliationState {
    let mut records = vec![
        covered_owner("owner-record-1", "owner-1", exact(1)),
        fence(FenceSpec {
            owner_id: Some("owner-2"),
            ..whole_scope_fence("fence-1")
        }),
    ];
    records.extend(owner_two);
    records.extend(entries);
    activated(records)
}

fn activated(records: Vec<EngramImportReconciliationRecord>) -> EngramImportReconciliationState {
    let mut journal = EngramImportReconciliationJournal::activated_empty();
    for record in records {
        journal.record(record).unwrap();
    }
    EngramImportReconciliationState::Activated(Arc::new(journal))
}

fn candidate(
    store: &str,
    project: &str,
    binding: EngramImportOriginalBinding,
) -> EngramImportDispatchCandidate {
    EngramImportDispatchCandidate {
        store_association: store.to_owned(),
        project_id: project.to_owned(),
        original_binding: binding,
        host_incarnation: BOOT.to_owned(),
    }
}

fn decide(
    state: &EngramImportReconciliationState,
    candidate: &EngramImportDispatchCandidate,
) -> EngramImportHoldDisposition {
    engram_import_hold_disposition::<anyhow::Error>(Ok(state), candidate)
}

fn held(fences: &[(&str, EngramImportFencePhase, bool)]) -> EngramImportHoldDisposition {
    EngramImportHoldDisposition::HeldByImportReconciliation(
        fences
            .iter()
            .map(
                |(record_id, phase, current_incarnation)| EngramImportHeldFence {
                    key: format!("local_import_fence/{record_id}"),
                    hold_id: format!("hold-{record_id}"),
                    phase: *phase,
                    current_incarnation: *current_incarnation,
                },
            )
            .collect(),
    )
}

#[test]
fn a_never_activated_host_admits() {
    let state = EngramImportReconciliationState::NotActivated;
    for binding in [
        exact(1),
        EngramImportOriginalBinding::Unknown,
        EngramImportOriginalBinding::Legacy,
    ] {
        assert_eq!(
            decide(&state, &candidate(STORE, PROJECT, binding)),
            EngramImportHoldDisposition::Admitted
        );
    }
}

#[test]
fn an_activated_journal_without_a_fence_admits() {
    let empty = activated(Vec::new());
    let owners_only = activated(vec![covered_owner("owner-record-1", "owner-1", exact(1))]);
    for state in [&empty, &owners_only] {
        assert_eq!(
            decide(state, &candidate(STORE, PROJECT, exact(1))),
            EngramImportHoldDisposition::Admitted
        );
    }
}

#[test]
fn a_covering_fence_holds_in_every_phase() {
    let cases = [
        (EngramImportFencePhase::Installing, None),
        (EngramImportFencePhase::Settling, None),
        (EngramImportFencePhase::Prepared, None),
        (EngramImportFencePhase::DecisionPending, None),
        (EngramImportFencePhase::Reconciled, None),
        (
            EngramImportFencePhase::Reconciled,
            Some("terminal-decision-1"),
        ),
    ];
    for (phase, terminal_decision_ref) in cases {
        let state = activated(vec![
            covered_owner("owner-record-1", "owner-1", exact(1)),
            fence(FenceSpec {
                phase,
                terminal_decision_ref,
                ..whole_scope_fence("fence-1")
            }),
        ]);
        assert_eq!(
            decide(&state, &candidate(STORE, PROJECT, exact(1))),
            held(&[("fence-1", phase, true)]),
            "phase {phase:?}, terminal decision {terminal_decision_ref:?}",
        );
    }
}

#[test]
fn a_fence_from_another_host_incarnation_stays_held() {
    let state = activated(vec![
        covered_owner("owner-record-1", "owner-1", exact(1)),
        fence(FenceSpec {
            incarnation: "boot-0",
            ..whole_scope_fence("fence-1")
        }),
    ]);
    assert_eq!(
        decide(&state, &candidate(STORE, PROJECT, exact(1))),
        held(&[("fence-1", EngramImportFencePhase::Installing, false)]),
    );
}

#[test]
fn two_covering_fences_hold_naming_both_in_key_order() {
    // Recorded in the reverse of key order; a later fence of one scope needs a
    // higher generation.
    let state = activated(vec![
        fence(FenceSpec {
            phase: EngramImportFencePhase::Prepared,
            ..whole_scope_fence("fence-b")
        }),
        fence(FenceSpec {
            generation: 2,
            ..whole_scope_fence("fence-a")
        }),
        covered_owner("owner-record-1", "owner-1", exact(1)),
    ]);
    assert_eq!(
        decide(&state, &candidate(STORE, PROJECT, exact(1))),
        held(&[
            ("fence-a", EngramImportFencePhase::Installing, true),
            ("fence-b", EngramImportFencePhase::Prepared, true),
        ]),
    );
}

#[test]
fn a_sibling_in_another_project_or_store_association_is_admitted() {
    let state = activated(vec![
        covered_owner("owner-record-1", "owner-1", exact(1)),
        fence(whole_scope_fence("fence-1")),
    ]);
    let siblings = [
        candidate(STORE, "github.com/example/other-project", exact(2)),
        candidate("store-association-2", PROJECT, exact(2)),
        candidate(
            "store-association-2",
            PROJECT,
            EngramImportOriginalBinding::Unknown,
        ),
    ];
    for sibling in siblings {
        assert_eq!(
            decide(&state, &sibling),
            EngramImportHoldDisposition::Admitted,
            "{sibling:?}"
        );
    }
}

#[test]
fn an_owner_scoped_fence_holds_only_its_own_owner() {
    let state = activated(vec![
        covered_owner("owner-record-1", "owner-1", exact(1)),
        covered_owner("owner-record-2", "owner-2", exact(2)),
        fence(FenceSpec {
            owner_id: Some("owner-1"),
            ..whole_scope_fence("fence-1")
        }),
    ]);
    assert_eq!(
        decide(&state, &candidate(STORE, PROJECT, exact(1))),
        held(&[("fence-1", EngramImportFencePhase::Installing, true)]),
    );
    assert_eq!(
        decide(&state, &candidate(STORE, PROJECT, exact(2))),
        EngramImportHoldDisposition::Admitted
    );
}

#[test]
fn an_unresolved_or_unproven_candidate_under_a_covering_fence_is_unknown() {
    use EngramImportHoldUnknownReason::*;
    let owner_fence = || {
        fence(FenceSpec {
            owner_id: Some("owner-1"),
            ..whole_scope_fence("fence-1")
        })
    };
    let cases: Vec<(
        &str,
        Vec<EngramImportReconciliationRecord>,
        EngramImportOriginalBinding,
        EngramImportHoldUnknownReason,
    )> = vec![
        (
            "unknown binding",
            vec![covered_owner("owner-record-1", "owner-1", exact(1))],
            EngramImportOriginalBinding::Unknown,
            UnboundCandidate,
        ),
        (
            "legacy binding",
            vec![covered_owner("owner-record-1", "owner-1", exact(1))],
            EngramImportOriginalBinding::Legacy,
            UnboundCandidate,
        ),
        (
            "no matching owner",
            vec![covered_owner("owner-record-1", "owner-1", exact(1))],
            exact(3),
            OwnerUnresolved,
        ),
        (
            "two owners claim one binding",
            vec![
                covered_owner("owner-record-1", "owner-1", exact(1)),
                covered_owner("owner-record-2", "owner-2", exact(2)),
                covered_owner("owner-record-3", "owner-3", exact(2)),
            ],
            exact(2),
            OwnerUnresolved,
        ),
        (
            "owner coverage unknown",
            vec![
                covered_owner("owner-record-1", "owner-1", exact(1)),
                owner(
                    "owner-record-2",
                    "owner-2",
                    exact(2),
                    EngramImportAssociationState::Admitted,
                    EngramImportCoverage::Unknown,
                ),
            ],
            exact(2),
            OwnerCoverageUnknown,
        ),
        (
            "owner association not admitted",
            vec![
                covered_owner("owner-record-1", "owner-1", exact(1)),
                owner(
                    "owner-record-2",
                    "owner-2",
                    exact(2),
                    EngramImportAssociationState::Pending,
                    EngramImportCoverage::Unknown,
                ),
            ],
            exact(2),
            OwnerCoverageUnknown,
        ),
    ];
    for (label, mut records, binding, reason) in cases {
        records.push(owner_fence());
        let state = activated(records);
        assert_eq!(
            decide(&state, &candidate(STORE, PROJECT, binding)),
            EngramImportHoldDisposition::Unknown(reason),
            "{label}",
        );
    }
}

#[test]
fn a_fence_whose_owner_is_not_provably_another_does_not_admit() {
    // Owner-2's original work is unknown, so it may be exact(1): its fence
    // cannot be shown not to cover a candidate that resolves to owner-1.
    let unknown_binding_owner = activated(vec![
        covered_owner("owner-record-1", "owner-1", exact(1)),
        owner(
            "owner-record-2",
            "owner-2",
            EngramImportOriginalBinding::Unknown,
            EngramImportAssociationState::Pending,
            EngramImportCoverage::Unknown,
        ),
        fence(FenceSpec {
            owner_id: Some("owner-2"),
            ..whole_scope_fence("fence-1")
        }),
    ]);
    // Owner-9 has no enrolled record at all.
    let unenrolled_owner = activated(vec![
        covered_owner("owner-record-1", "owner-1", exact(1)),
        fence(FenceSpec {
            owner_id: Some("owner-9"),
            ..whole_scope_fence("fence-1")
        }),
    ]);
    for (label, state) in [
        ("unknown-binding fence owner", unknown_binding_owner),
        ("unenrolled fence owner", unenrolled_owner),
    ] {
        assert_eq!(
            decide(&state, &candidate(STORE, PROJECT, exact(1))),
            EngramImportHoldDisposition::Unknown(EngramImportHoldUnknownReason::FenceOwnerUnproven),
            "{label}",
        );
    }
}

#[test]
fn a_covering_fence_holds_over_an_unknown_one_in_either_key_order() {
    // An unbound candidate is unknown under the owner-scoped fence and held
    // by the whole-scope one; the fences are recorded in generation order and
    // their keys sort either way round.
    for (whole_id, owner_id) in [("fence-a", "fence-b"), ("fence-b", "fence-a")] {
        let (first, second) = if whole_id < owner_id {
            (
                whole_scope_fence(whole_id),
                FenceSpec {
                    owner_id: Some("owner-1"),
                    generation: 2,
                    ..whole_scope_fence(owner_id)
                },
            )
        } else {
            (
                FenceSpec {
                    owner_id: Some("owner-1"),
                    ..whole_scope_fence(owner_id)
                },
                FenceSpec {
                    generation: 2,
                    ..whole_scope_fence(whole_id)
                },
            )
        };
        let state = activated(vec![
            covered_owner("owner-record-1", "owner-1", exact(1)),
            fence(first),
            fence(second),
        ]);
        assert_eq!(
            decide(
                &state,
                &candidate(STORE, PROJECT, EngramImportOriginalBinding::Unknown)
            ),
            held(&[(whole_id, EngramImportFencePhase::Installing, true)]),
            "whole-scope {whole_id}, owner-scoped {owner_id}",
        );
    }
}

#[test]
fn a_whole_scope_fence_holds_even_an_unbound_candidate() {
    let state = activated(vec![fence(whole_scope_fence("fence-1"))]);
    for binding in [
        EngramImportOriginalBinding::Unknown,
        EngramImportOriginalBinding::Legacy,
        exact(9),
    ] {
        assert_eq!(
            decide(&state, &candidate(STORE, PROJECT, binding.clone())),
            held(&[("fence-1", EngramImportFencePhase::Installing, true)]),
            "{binding:?}",
        );
    }
}

#[test]
fn a_journal_that_cannot_be_established_is_unknown() {
    let refusal = anyhow!("import-reconciliation journal refused to load");
    for binding in [exact(1), EngramImportOriginalBinding::Unknown] {
        assert_eq!(
            engram_import_hold_disposition::<anyhow::Error>(
                Err(&refusal),
                &candidate(STORE, PROJECT, binding)
            ),
            EngramImportHoldDisposition::Unknown(
                EngramImportHoldUnknownReason::JournalNotEstablished
            ),
        );
    }
}

#[test]
fn a_live_entry_of_the_fence_owner_naming_the_candidate_holds_it() {
    let held_by_fence = held(&[("fence-1", EngramImportFencePhase::Installing, true)]);
    assert_eq!(
        decide(
            &owner_two_fence_with(Vec::new()),
            &candidate(STORE, PROJECT, exact(1))
        ),
        EngramImportHoldDisposition::Admitted,
        "without the entry, owner-1's candidate passes owner-2's fence",
    );
    for state in [
        EngramImportEntryState::Outstanding,
        EngramImportEntryState::Unknown,
    ] {
        let journal = owner_two_fence_with(vec![entry("entry-1", "owner-2", exact(1), state)]);
        assert_eq!(
            decide(&journal, &candidate(STORE, PROJECT, exact(1))),
            held_by_fence,
            "{state:?} entry of owner-2 naming exact(1)",
        );
    }
    // The entry ties the fence to the candidate even when the candidate's own
    // owner does not resolve.
    let journal = owner_two_fence_with(vec![entry(
        "entry-1",
        "owner-2",
        exact(3),
        EngramImportEntryState::Outstanding,
    )]);
    assert_eq!(
        decide(&journal, &candidate(STORE, PROJECT, exact(3))),
        held_by_fence,
    );
}

#[test]
fn a_live_unbound_entry_of_the_fence_owner_leaves_the_candidate_unknown() {
    for binding in [
        EngramImportOriginalBinding::Unknown,
        EngramImportOriginalBinding::Legacy,
    ] {
        for state in [
            EngramImportEntryState::Outstanding,
            EngramImportEntryState::Unknown,
        ] {
            let journal =
                owner_two_fence_with(vec![entry("entry-1", "owner-2", binding.clone(), state)]);
            assert_eq!(
                decide(&journal, &candidate(STORE, PROJECT, exact(1))),
                EngramImportHoldDisposition::Unknown(
                    EngramImportHoldUnknownReason::FenceOwnerUnproven
                ),
                "{state:?} entry of owner-2 with {binding:?}",
            );
        }
    }
}

#[test]
fn terminal_other_owner_and_other_binding_entries_change_nothing() {
    let cases = vec![
        (
            "terminal entry naming the candidate",
            entry(
                "entry-1",
                "owner-2",
                exact(1),
                EngramImportEntryState::Terminal,
            ),
        ),
        (
            "terminal unbound entry",
            entry(
                "entry-1",
                "owner-2",
                EngramImportOriginalBinding::Unknown,
                EngramImportEntryState::Terminal,
            ),
        ),
        (
            "another owner's entry naming the candidate",
            entry(
                "entry-1",
                "owner-3",
                exact(1),
                EngramImportEntryState::Outstanding,
            ),
        ),
        (
            "another owner's unbound entry",
            entry(
                "entry-1",
                "owner-3",
                EngramImportOriginalBinding::Legacy,
                EngramImportEntryState::Outstanding,
            ),
        ),
        (
            "the fence owner's entry with another exact binding",
            entry(
                "entry-1",
                "owner-2",
                exact(4),
                EngramImportEntryState::Outstanding,
            ),
        ),
        (
            "the fence owner's entry naming the candidate in another store association",
            entry_in(
                "store-association-2",
                PROJECT,
                "entry-1",
                "owner-2",
                exact(1),
                EngramImportEntryState::Outstanding,
            ),
        ),
        (
            "the fence owner's unbound entry in another project",
            entry_in(
                STORE,
                "github.com/example/other-project",
                "entry-1",
                "owner-2",
                EngramImportOriginalBinding::Unknown,
                EngramImportEntryState::Outstanding,
            ),
        ),
    ];
    for (label, record) in cases {
        assert_eq!(
            decide(
                &owner_two_fence_with(vec![record]),
                &candidate(STORE, PROJECT, exact(1))
            ),
            EngramImportHoldDisposition::Admitted,
            "{label}",
        );
    }
}

#[test]
fn the_fence_owners_coverage_and_association_are_not_consulted() {
    use EngramImportAssociationState::*;
    let held_by_fence = held(&[("fence-1", EngramImportFencePhase::Installing, true)]);
    let unproven =
        EngramImportHoldDisposition::Unknown(EngramImportHoldUnknownReason::FenceOwnerUnproven);
    let enrolled = |association| {
        Some(owner(
            "owner-record-2",
            "owner-2",
            exact(2),
            association,
            EngramImportCoverage::Unknown,
        ))
    };
    // Without entries, an enrolled owner-2 with another exact binding lets
    // owner-1's candidate through whatever its coverage and association; an
    // unenrolled owner-2 does not.
    let fence_owners = [
        (
            "admitted, coverage unknown",
            enrolled(Admitted),
            EngramImportHoldDisposition::Admitted,
        ),
        (
            "pending",
            enrolled(Pending),
            EngramImportHoldDisposition::Admitted,
        ),
        (
            "legacy association",
            enrolled(Legacy),
            EngramImportHoldDisposition::Admitted,
        ),
        (
            "foreign",
            enrolled(Foreign),
            EngramImportHoldDisposition::Admitted,
        ),
        ("not enrolled", None, unproven.clone()),
    ];
    for (label, owner_two, without_entries) in fence_owners {
        let with = |entries| owner_two_fence(owner_two.clone(), entries);
        assert_eq!(
            decide(&with(Vec::new()), &candidate(STORE, PROJECT, exact(1))),
            without_entries,
            "{label} fence owner without entries",
        );
        assert_eq!(
            decide(
                &with(vec![entry(
                    "entry-1",
                    "owner-2",
                    exact(1),
                    EngramImportEntryState::Outstanding,
                )]),
                &candidate(STORE, PROJECT, exact(1)),
            ),
            held_by_fence,
            "{label} fence owner with a live entry naming the candidate",
        );
        for binding in [
            EngramImportOriginalBinding::Unknown,
            EngramImportOriginalBinding::Legacy,
        ] {
            assert_eq!(
                decide(
                    &with(vec![entry(
                        "entry-1",
                        "owner-2",
                        binding.clone(),
                        EngramImportEntryState::Outstanding,
                    )]),
                    &candidate(STORE, PROJECT, exact(1)),
                ),
                unproven,
                "{label} fence owner with a live {binding:?} entry",
            );
        }
    }
}
