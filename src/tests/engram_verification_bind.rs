// Tests for binding one natively credited full gate to the other items of a
// batch (`engram_verification_bind.rs`).
// Owns: the members a launch records, the settlement precheck (claim, root
// and newest sighting at the gate's revision, never a made-up sighting), the
// request's wire form and stable key, and how each Engram answer settles a
// member.
// Does not own: the gate's native credit (engram_carried_checks.rs tests),
// sending, persisting and replaying binds, or the bound record's citation,
// which Engram serves.
// A new module, declared from tests/mod.rs.
use super::*;

const ROOT: &str = r"\\?\C:\repo\.tmp\wt-batch";
const OTHER_ROOT: &str = r"\\?\C:\repo\.tmp\wt-other";
const COMMON: &str = "c:/repo/.git";
const SESSION: &str = "session-merger";
const REVISION: &str = "content-v1:batch";

fn store(project: &str) -> EngramAuthorityStoreKey {
    EngramAuthorityStoreKey {
        database_path: PathBuf::from(r"C:\store\engram.db"),
        project_id: project.to_owned(),
    }
}

fn entry(work: &str, root: &str, generation: u64) -> EngramWorkSourceRoot {
    EngramWorkSourceRoot {
        store: store("termal"),
        work_id: format!("work-{work}"),
        short_ref: format!("w-{work}"),
        claim_id: format!("claim-{work}"),
        claim_fence: 3,
        root: root.to_owned(),
        common_dir_key: COMMON.to_owned(),
        named_by_session: SESSION.to_owned(),
        named_at: "2026-10-18T23:00:00.000Z".to_owned(),
        generation,
    }
}

fn gate() -> EngramExecutionSourceBasis {
    EngramExecutionSourceBasis {
        workspace_id: ROOT.to_owned(),
        source_revision: REVISION.to_owned(),
        source_root_generation: Some(10),
        source_root_state: None,
    }
}

fn binding(work: &str, fence: i64) -> EngramControlWorkBinding {
    EngramControlWorkBinding {
        root_execution_id: format!("root-{work}"),
        work_id: format!("work-{work}"),
        run_id: format!("run-{work}"),
        work_revision: 5,
        claim_id: format!("claim-{work}"),
        claim_fence: fence,
    }
}

fn held(claims: &[(&str, i64)]) -> EngramHeldClaims {
    EngramHeldClaims {
        items: claims
            .iter()
            .map(|(work, fence)| EngramHeldClaim {
                work_id: format!("work-{work}"),
                short_ref: format!("w-{work}"),
                claim_id: format!("claim-{work}"),
                claim_fence: *fence,
                focused: false,
                control_binding: Some(binding(work, *fence)),
                ..Default::default()
            })
            .collect(),
        omitted: 0,
    }
}

fn sighting(work: &str, root: EngramSightingRoot) -> EngramNamedRootSightingRead {
    EngramNamedRootSightingRead {
        schema_version: 1,
        project_id: "termal".to_owned(),
        work_id: format!("work-{work}"),
        run_id: format!("run-{work}"),
        read_cut: 40,
        head_cut: 40,
        current_binding: None,
        binding_changed: false,
        root,
    }
}

fn sighted_at(revision: &str) -> EngramSightingRoot {
    EngramSightingRoot::Bound {
        workspace_id: ROOT.to_owned(),
        generation: 12,
        binding_event: "e".repeat(32),
        binding_position: 3,
        sighting: EngramRootSighting::Present {
            record: "a".repeat(32),
            position: 30,
            revision: revision.to_owned(),
        },
    }
}

#[test]
fn a_launch_records_the_other_items_this_session_named_on_the_same_worktree() {
    let launch = entry("launch", ROOT, 10);
    let mut foreign = entry("foreign-session", ROOT, 13);
    foreign.named_by_session = "session-other".to_owned();
    let mut other_store = entry("other-store", ROOT, 14);
    other_store.store = store("engram");
    let mut other_repo = entry("other-repo", ROOT, 15);
    other_repo.common_dir_key = "c:/elsewhere/.git".to_owned();
    let roots = vec![
        launch.clone(),
        entry("member-a", ROOT, 11),
        entry("member-b", ROOT, 12),
        entry("member-b", ROOT, 12),
        entry("elsewhere", OTHER_ROOT, 16),
        foreign,
        other_store,
        other_repo,
    ];
    // member-a was named at fence 3 and recovered to fence 4 before the
    // launch; its root entry keeps fence 3. member-b is not held at launch.
    let held_at_launch = held(&[("launch", 3), ("member-a", 4)]);
    let targets = engram_bind_targets_at_launch(&roots, &launch, SESSION, &held_at_launch);
    assert_eq!(
        targets
            .iter()
            .map(|target| (
                target.short_ref.as_str(),
                target.claim_fence,
                target.root_generation
            ))
            .collect::<Vec<_>>(),
        vec![("w-member-a", 4, 11), ("w-member-b", 3, 12)],
        "only this session's other items on the same root, repository and store, \
         once each, at the fence each claim has at launch"
    );
    // A single-item gate has nothing to bind.
    assert!(
        engram_bind_targets_at_launch(&[launch.clone()], &launch, SESSION, &held_at_launch)
            .is_empty()
    );
    // Each member is bound by its own call, so none is dropped for Engram's
    // limit on one request's targets.
    let many: Vec<EngramWorkSourceRoot> = (0..20)
        .map(|index| entry(&format!("many-{index}"), ROOT, 20 + index))
        .collect();
    assert_eq!(
        engram_bind_targets_at_launch(&many, &launch, SESSION, &held(&[])).len(),
        20
    );
}

#[test]
fn a_member_is_bound_only_with_its_newest_sighting_at_the_gate_revision() {
    let target = EngramBindTarget {
        work_id: "work-member".to_owned(),
        short_ref: "w-member".to_owned(),
        claim_id: "claim-member".to_owned(),
        claim_fence: 3,
        root_generation: 12,
    };
    let roots = vec![entry("member", ROOT, 12)];
    let claims = held(&[("member", 3)]);

    // Its newest sighting is at the gate's revision: bound, with that sighting.
    let current = sighting("member", sighted_at(REVISION));
    assert_eq!(
        engram_bind_decision(&target, &claims, &roots, Ok(&current), &gate()),
        EngramBindDecision::Bind {
            binding: binding("member", 3),
            sighting: EngramBindSighting {
                observation: "a".repeat(32),
                source_revision: REVISION.to_owned(),
            },
        }
    );

    let skipped = |read: std::result::Result<&EngramNamedRootSightingRead, &str>,
                   held: &EngramHeldClaims,
                   roots: &[EngramWorkSourceRoot]| {
        match engram_bind_decision(&target, held, roots, read, &gate()) {
            EngramBindDecision::Skip(reason) => reason,
            bound => panic!("expected a skip, got {bound:?}"),
        }
    };
    // Its newest sighting is older: never bound, and the holder is told how
    // to give it one. No sighting is made up.
    let older = sighting("member", sighted_at("content-v1:older"));
    let reason = skipped(Ok(&older), &claims, &roots);
    assert!(
        reason.contains("content-v1:older") && reason.contains(ENGRAM_BIND_SIGHTING_REMEDY),
        "{reason}"
    );
    let absent = sighting(
        "member",
        EngramSightingRoot::Bound {
            workspace_id: ROOT.to_owned(),
            generation: 12,
            binding_event: "e".repeat(32),
            binding_position: 3,
            sighting: EngramRootSighting::Absent,
        },
    );
    assert!(skipped(Ok(&absent), &claims, &roots).contains(ENGRAM_BIND_SIGHTING_REMEDY));
    // The read failed, or Engram's binding of the root moved.
    assert!(skipped(Err("timed out"), &claims, &roots).contains("timed out"));
    let mut moved = sighting("member", sighted_at(REVISION));
    moved.binding_changed = true;
    assert!(skipped(Ok(&moved), &claims, &roots).contains("binding"));
    // Engram binds it elsewhere.
    let mut elsewhere = sighting("member", sighted_at(REVISION));
    if let EngramSightingRoot::Bound { workspace_id, .. } = &mut elsewhere.root {
        *workspace_id = OTHER_ROOT.to_owned();
    }
    assert!(skipped(Ok(&elsewhere), &claims, &roots).contains("another root"));
    // Engram binds the same worktree at another generation.
    let mut regenerated = sighting("member", sighted_at(REVISION));
    if let EngramSightingRoot::Bound { generation, .. } = &mut regenerated.root {
        *generation = 13;
    }
    assert!(skipped(Ok(&regenerated), &claims, &roots).contains("another root or generation"));
    // Engram binds no root for it, or the read is of another run.
    let unbound = sighting("member", EngramSightingRoot::None);
    assert!(skipped(Ok(&unbound), &claims, &roots).contains("no named root"));
    let mut other_run = sighting("member", sighted_at(REVISION));
    other_run.run_id = "run-other".to_owned();
    assert!(skipped(Ok(&other_run), &claims, &roots).contains("another item or run"));
    // Its claim is gone, or moved since the launch.
    assert!(skipped(Ok(&current), &held(&[]), &roots).contains("no longer held"));
    // Missing from a list Engram cut short, it may still be held: nothing
    // final is said of it.
    let cut_short = EngramHeldClaims {
        items: Vec::new(),
        omitted: 1,
    };
    assert!(matches!(
        engram_bind_decision(&target, &cut_short, &roots, Ok(&current), &gate()),
        EngramBindDecision::Unknown(_)
    ));
    let mut offered = held(&[("member", 3)]);
    offered.items[0].control_binding = None;
    assert!(skipped(Ok(&current), &offered, &roots).contains("handoff offer pending"));
    assert!(skipped(Ok(&current), &held(&[("member", 4)]), &roots).contains("fence 3 then, 4 now"));
    // Its root was renamed (a new generation) or names another worktree.
    assert!(skipped(Ok(&current), &claims, &[entry("member", ROOT, 13)]).contains("renamed"));
    assert!(skipped(Ok(&current), &claims, &[entry("member", OTHER_ROOT, 12)]).contains("renamed"));
}

#[test]
fn a_bind_request_carries_one_member_under_a_key_stable_per_gate_and_member() {
    let measurement = EngramBindMeasurement {
        workspace_id: ROOT.to_owned(),
        source_revision: REVISION.to_owned(),
        measured_at: "2026-10-18T23:10:00.000Z".to_owned(),
    };
    let sighting = EngramBindSighting {
        observation: "a".repeat(32),
        source_revision: REVISION.to_owned(),
    };
    let build = || {
        engram_verification_bind_request(
            "routing-merger",
            &"f".repeat(32),
            measurement.clone(),
            binding("member", 3),
            sighting.clone(),
        )
    };
    let wire = serde_json::to_value(build()).unwrap();
    assert_eq!(wire["operation"], "verification_bind");
    assert_eq!(wire["routing_token"], "routing-merger");
    assert_eq!(wire["original"], "f".repeat(32));
    assert_eq!(
        wire["idempotency_key"],
        format!("termal-verification-bind:{}:work-member", "f".repeat(32))
    );
    assert_eq!(wire["measurement"]["measured_at"], "2026-10-18T23:10:00.000Z");
    assert_eq!(wire["targets"].as_array().map(Vec::len), Some(1));
    assert_eq!(wire["targets"][0]["binding"]["claim_fence"], 3);
    assert_eq!(wire["targets"][0]["sighting"]["observation"], "a".repeat(32));
    assert!(wire["targets"][0].get("criteria").is_none(), "{wire}");
    // The same gate and member always give the same bytes, which a resend
    // sends again.
    assert_eq!(serde_json::to_vec(&build()).unwrap(), serde_json::to_vec(&build()).unwrap());
    assert!(engram_bind_idempotency_key(&"f".repeat(64), &"w".repeat(36)).len() <= 512);
}

#[test]
fn each_engram_answer_settles_a_member_by_its_outcome_class() {
    let original = "f".repeat(32);
    let outcome = |result| engram_bind_outcome(result, &original, "w-member", "work-member");
    let remote = |code: &str, message: &str| {
        Err(EngramTransportError::remote(EngramControlErrorBody {
            code: code.to_owned(),
            message: message.to_owned(),
        }))
    };
    // Bound, or replayed.
    assert_eq!(
        outcome(Ok(json!({"original": original, "replayed": true,
            "bound": [{"work_id": "work-member", "verification": "b".repeat(32)}]}))),
        EngramBindOutcome::Bound { verification: "b".repeat(32), replayed: true }
    );
    // A receipt that does not name the member, names another gate's record,
    // or no valid record id, says nothing certain: it is sent again, and
    // Engram's replay of the key settles it.
    for receipt in [
        json!({"original": original, "replayed": false, "bound": []}),
        json!({"original": "e".repeat(32), "replayed": false,
            "bound": [{"work_id": "work-member", "verification": "b".repeat(32)}]}),
        json!({"replayed": false,
            "bound": [{"work_id": "work-member", "verification": "b".repeat(32)}]}),
        json!({"original": original, "replayed": false,
            "bound": [{"work_id": "work-member", "verification": ""}]}),
    ] {
        assert!(matches!(outcome(Ok(receipt.clone())), EngramBindOutcome::Resend(_)), "{receipt}");
    }
    // Bound before under another key.
    assert_eq!(
        outcome(remote(
            "verification_bind_refused",
            "the verification bind was refused: target w-member already_bound"
        )),
        EngramBindOutcome::AlreadyBound
    );
    // A committed bind under this gate's and this member's key, with other
    // bytes: Engram keeps a key only when its bind commits, so it is bound.
    assert_eq!(
        outcome(remote(
            "control_operation_idempotency_conflict",
            "the idempotency key was used for another request"
        )),
        EngramBindOutcome::AlreadyBound
    );
    // Engram checked the member and refused it, or another item is the one
    // already bound: unbound for good.
    for message in [
        "the verification bind was refused: target w-member sighting_revision_differs",
        "the verification bind was refused: target w-member-b already_bound",
    ] {
        assert!(
            matches!(
                outcome(remote("verification_bind_refused", message)),
                EngramBindOutcome::Final(_)
            ),
            "{message}"
        );
    }
    // The connection was refused, not the bind; an earlier attempt may have
    // committed, so the same request goes again under refreshed credentials.
    for code in [
        "control_session_not_bound",
        "control_session_token_mismatch",
        "control_connection_superseded",
        "invalid_routing_token",
        "unknown_routing_token",
    ] {
        assert!(
            matches!(outcome(remote(code, "refused")), EngramBindOutcome::Refresh(_)),
            "{code}"
        );
    }
    // Any other code Engram sends is met again by the same request, so it is
    // final and its holder is told: an Engram without verification_bind, an
    // uninitialised store, a code this build does not know.
    for (code, message) in [
        ("invalid_request", "unknown variant `verification_bind`"),
        ("store_not_initialized", "the store is not initialised"),
        ("task_access_denied", "denied"),
        ("a_code_from_a_later_engram", "refused"),
    ] {
        match outcome(remote(code, message)) {
            EngramBindOutcome::Final(reason) => assert!(reason.contains(code), "{reason}"),
            other => panic!("{code}: {other:?}"),
        }
    }
    // Resent with the same bytes: an expiry Engram certifies wrote nothing,
    // a busy store, a storage error (a failed commit among them), and every
    // answer that did not reach Engram or came back unreadable.
    for result in [
        remote("control_request_expired", "the control request expired"),
        remote("control_store_busy", "the store is busy"),
        remote("storage_error", "database is locked"),
        Err(EngramTransportError::deadline("no answer within 10 s")),
        Err(EngramTransportError::transport("the control process exited")),
        Err(EngramTransportError::spawn_failed("engram not found")),
        Err(EngramTransportError::protocol("the answer was not JSON")),
    ] {
        assert!(matches!(outcome(result), EngramBindOutcome::Resend(_)));
    }
}

#[test]
fn the_holder_is_told_each_member_bound_or_not_with_its_reason() {
    assert_eq!(
        engram_bind_notice_line(
            "w-member",
            &EngramBindOutcome::Bound { verification: "b".repeat(32), replayed: false }
        )
        .as_deref(),
        Some(
            "[TermAl] Check credit: the full gate is also recorded for w-member (bound), on its \
             own run."
        )
    );
    assert_eq!(
        engram_bind_notice_line("w-member", &EngramBindOutcome::AlreadyBound).as_deref(),
        Some(
            "[TermAl] Check credit: the full gate is also recorded for w-member (bound), on its \
             own run."
        )
    );
    assert_eq!(
        engram_bind_notice_line("w-member", &EngramBindOutcome::Final("its claim moved".to_owned()))
            .as_deref(),
        Some("[TermAl] Check credit: the full gate is not recorded for w-member: its claim moved.")
    );
    assert_eq!(
        engram_bind_notice_line("w-member", &EngramBindOutcome::Resend("busy".to_owned())),
        None
    );
    assert_eq!(
        engram_bind_notice_line("w-member", &EngramBindOutcome::Refresh("superseded".to_owned())),
        None
    );
}
