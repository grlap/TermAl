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
            EngramImportHoldDisposition::Unknown(
                EngramImportHoldUnknownReason::FenceOwnerUnproven
            ),
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
