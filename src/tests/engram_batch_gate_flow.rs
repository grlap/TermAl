// Owns the end-to-end tests of a passed batch full gate bound to the other
// items its session named on the same worktree (src/engram_batch_binds.rs):
// the launch recording the member, the checkpoint's answer making the bind
// owed (or a refused checkpoint draining it), the owed-binds tick deciding it
// from the member's claim and newest sighting, confirming that decision
// stored before sending it, sending it, rebinding after Engram refuses the
// session's credentials, and telling the holder; and a restart that never
// leaves a member untold.
// Does not own the pure rules of those steps (src/tests/engram_batch_binds.rs,
// src/tests/engram_verification_bind.rs), the recovery checkpoint's settling
// (src/tests/engram_batch_recovery.rs), or the carried gate's own credit
// (src/tests/engram_carried_checks.rs, whose fixtures this child module uses).
// New module.
use super::*;

const MEMBER_GENERATION: u64 = 5;

/// A checkpoint answer naming `records`, one per verification input.
fn checkpoint_reply_naming(grant_id: &str, records: &[String]) -> ScriptedEngramControlResponse {
    ScriptedEngramControlResponse::Reply(Ok(json!({
        "decision": "checkpointed",
        "receipt": {
            "grant_id": grant_id,
            "checkpoint": "d".repeat(64),
            "cursor": 1,
            "confirmed_cursor": 1,
            "verification_evidence": records,
        }
    })))
}

/// Names a second item, `w-member`, for the turn's session on the same
/// worktree as the turn's own item.
fn name_member(turn: &CheckedTurn) -> EngramWorkSourceRoot {
    let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
    let launch = inner
        .engram_work_source_roots
        .iter()
        .find(|entry| entry.named_by_session == turn.session_id)
        .cloned()
        .expect("the turn's item names its root");
    let member = EngramWorkSourceRoot {
        work_id: "work-member".to_owned(),
        short_ref: "w-member".to_owned(),
        claim_id: "claim-member".to_owned(),
        claim_fence: 1,
        generation: MEMBER_GENERATION,
        ..launch
    };
    inner.engram_work_source_roots.push(member.clone());
    member
}

fn member_binding() -> EngramControlWorkBinding {
    EngramControlWorkBinding {
        root_execution_id: "root-member".to_owned(),
        work_id: "work-member".to_owned(),
        run_id: "run-member".to_owned(),
        work_revision: 1,
        claim_id: "claim-member".to_owned(),
        claim_fence: 1,
    }
}

/// Scripts the session's held claims: the turn's own item and `w-member`.
fn script_held(turn: &CheckedTurn) {
    let launch_binding = turn.record(|record| record.engram.work_binding.clone().unwrap());
    turn.transport.script_held_claims([Ok(EngramHeldClaims {
        items: vec![
            EngramHeldClaim {
                work_id: launch_binding.work_id.clone(),
                short_ref: "w-carried".to_owned(),
                claim_id: launch_binding.claim_id.clone(),
                claim_fence: launch_binding.claim_fence,
                focused: true,
                control_binding: Some(launch_binding.clone()),
                ..Default::default()
            },
            EngramHeldClaim {
                work_id: "work-member".to_owned(),
                short_ref: "w-member".to_owned(),
                claim_id: "claim-member".to_owned(),
                claim_fence: 1,
                control_binding: Some(member_binding()),
                ..Default::default()
            },
        ],
        omitted: 0,
    })]);
}

/// Replaces whatever is left in the scripted answers with `reply` alone.
fn answer_next(turn: &CheckedTurn, reply: std::result::Result<Value, EngramTransportError>) {
    let mut responses = turn.transport.responses.lock().unwrap();
    responses.clear();
    responses.push_back(ScriptedEngramControlResponse::Reply(reply));
}

fn requests_of(turn: &CheckedTurn, operation: &str) -> Vec<Value> {
    turn.transport
        .requests()
        .into_iter()
        .map(|recorded| recorded.request)
        .filter(|request| request["operation"] == operation)
        .collect()
}

fn tick(turn: &CheckedTurn) {
    turn.state
        .engram_owed_binds_tick(chrono::Utc::now(), DEADLOCK_GUARD);
}

/// A two-item batch gate launched, passed, credited and answered: its
/// member's bind is owed, its newest sighting scripted at the gate's
/// revision. Returns the turn, the gate's record and the member's root entry.
fn owed_batch_turn(label: &str) -> (CheckedTurn, String, EngramWorkSourceRoot) {
    // As Engram mints a verification record's id: 32 hex digits.
    let gate_record = "e".repeat(32);
    let turn = CheckedTurn::start_with_opening(
        label,
        true,
        None,
        vec![checkpoint_reply_naming(CHECK_GRANT, &[gate_record.clone()])],
        1,
        true,
    );
    let worktree = turn.root.join(".worktrees").join("wt");
    let member = name_member(&turn);
    script_held(&turn);

    // The launch records the member.
    launch_gate(&turn, &worktree, false);
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0].bind_targets.clone()),
        vec![EngramBindTarget {
            work_id: "work-member".to_owned(),
            short_ref: "w-member".to_owned(),
            claim_id: "claim-member".to_owned(),
            claim_fence: 1,
            root_generation: MEMBER_GENERATION,
        }]
    );

    // The gate passes and is credited; the checkpoint's answer names its own
    // record, so the member's bind is owed and persisted.
    finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let checkpoint = turn.finish();
    assert_eq!(
        checkpoint["verification_evidence"].as_array().map(Vec::len),
        Some(1),
        "{checkpoint:#}"
    );
    let owed = turn.record(|record| record.engram.owed_binds.clone());
    assert_eq!(owed.len(), 1, "{owed:?}");
    assert_eq!(owed[0].original, gate_record);
    assert_eq!(owed[0].target.short_ref, "w-member");
    let sighting = EngramNamedRootSightingRead {
        schema_version: 1,
        project_id: "github.com/example/source-root".to_owned(),
        work_id: "work-member".to_owned(),
        run_id: "run-member".to_owned(),
        read_cut: 9,
        head_cut: 9,
        current_binding: None,
        binding_changed: false,
        root: EngramSightingRoot::Bound {
            workspace_id: member.root.clone(),
            generation: MEMBER_GENERATION as i64,
            binding_event: "c".repeat(32),
            binding_position: 2,
            sighting: EngramRootSighting::Present {
                record: "a".repeat(32),
                position: 8,
                revision: owed[0].measurement.source_revision.clone(),
            },
        },
    };
    turn.transport
        .script_sighting_reads([Ok(serde_json::to_value(&sighting).unwrap())]);
    (turn, gate_record, member)
}

#[test]
fn a_passed_batch_gate_is_bound_to_each_other_member_on_its_own_run() {
    let (turn, gate_record, _) = owed_batch_turn("carried-batch");
    let measurement = turn.record(|record| record.engram.owed_binds[0].measurement.clone());

    // The first tick decides the member and confirms that decision stored;
    // it sends nothing yet.
    tick(&turn);
    assert!(requests_of(&turn, "verification_bind").is_empty());
    let decided = turn.record(|record| record.engram.owed_binds[0].clone());
    assert_eq!(
        decided.decided.as_ref().map(|decided| decided.binding.clone()),
        Some(member_binding())
    );
    assert!(!decided.decision_unconfirmed, "the decision was read back stored");
    let stored = turn.record(|record| {
        serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap()
    });
    assert_eq!(
        stored["engramOwedBinds"][0]["decided"],
        serde_json::to_value(&decided.decided).unwrap()
    );

    // The next tick sends it; Engram binds it.
    answer_next(
        &turn,
        Ok(json!({
            "original": gate_record,
            "replayed": false,
            "bound": [{ "work_id": "work-member", "work_ref": "w-member",
                "verification": "b".repeat(64) }],
        })),
    );
    tick(&turn);

    let binds = requests_of(&turn, "verification_bind");
    assert_eq!(binds.len(), 1, "{binds:#?}");
    let bind = &binds[0];
    assert_eq!(bind["original"], gate_record);
    assert_eq!(
        bind["idempotency_key"],
        format!("termal-verification-bind:{gate_record}:work-member")
    );
    assert_eq!(bind["measurement"], serde_json::to_value(&measurement).unwrap());
    assert_eq!(bind["targets"][0]["binding"], serde_json::to_value(member_binding()).unwrap());
    assert_eq!(bind["targets"][0]["sighting"]["observation"], "a".repeat(32));
    assert_eq!(
        bind["targets"][0]["sighting"]["source_revision"],
        measurement.source_revision
    );
    assert!(turn.record(|record| record.engram.owed_binds.is_empty()));
    let told = turn.record(|record| record.engram.pending_source_root_line.clone().unwrap_or_default());
    assert!(
        told.contains("the full gate is also recorded for w-member (bound), on its own run."),
        "{told}"
    );
}

#[test]
fn credentials_engram_refuses_are_refreshed_before_the_bind_is_sent_again() {
    let (turn, _, _) = owed_batch_turn("carried-batch-refresh");
    tick(&turn);
    let sessions_bound = requests_of(&turn, "session_bind").len();
    answer_next(
        &turn,
        Err(EngramTransportError::remote(EngramControlErrorBody {
            code: "control_connection_superseded".to_owned(),
            message: "another connection took over".to_owned(),
        })),
    );
    // The rebind this asks for is lost on the way, as in a transient outage:
    // the session stays under control, its credentials still stale.
    turn.transport
        .responses
        .lock()
        .unwrap()
        .push_back(ScriptedEngramControlResponse::Reply(Err(
            EngramTransportError::transport("the rebind call was lost"),
        )));
    tick(&turn);

    // Still owed, its attempt counted; the session's credentials are marked
    // stale and a fresh bind is asked for, rather than the same token sent
    // again.
    let owed = turn.record(|record| record.engram.owed_binds.clone());
    assert_eq!(owed.len(), 1, "{owed:?}");
    assert_eq!(owed[0].attempts, 1);
    assert!(
        owed[0].last_detail.as_deref().is_some_and(|detail| detail.contains("superseded")),
        "{owed:?}"
    );
    let rebound = requests_of(&turn, "session_bind").len() > sessions_bound;
    let marked = turn.record(|record| record.engram.rebind_required);
    assert!(rebound || marked, "a credential refusal must lead to a fresh bind");

    // Once a fresh token is there, the same request goes again under it.
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.session_id).unwrap();
        let engram = &mut inner.sessions[index].engram;
        engram.rebind_required = false;
        engram.routing_token = Some("fresh-token".to_owned());
    }
    answer_next(
        &turn,
        Ok(json!({ "original": "e".repeat(32), "replayed": true,
            "bound": [{ "work_id": "work-member", "verification": "b".repeat(64) }] })),
    );
    turn.state.engram_owed_binds_tick(
        chrono::Utc::now() + chrono::Duration::seconds(10),
        DEADLOCK_GUARD,
    );
    let binds = requests_of(&turn, "verification_bind");
    assert_eq!(binds.len(), 2, "{binds:#?}");
    assert_eq!(binds[1]["routing_token"], "fresh-token");
    assert_eq!(binds[1]["idempotency_key"], binds[0]["idempotency_key"]);
    let (mut first, mut second) = (binds[0].clone(), binds[1].clone());
    first["routing_token"].take();
    second["routing_token"].take();
    assert_eq!(first, second, "the same intent under the fresh token");
    assert!(turn.record(|record| record.engram.owed_binds.is_empty()));
}

/// The session's row and its transcript rows, each as the JSON stored, read
/// from the SQLite store itself.
fn stored_session(turn: &CheckedTurn) -> (Value, Vec<String>) {
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path())
        .expect("the store opens");
    let row: String = connection
        .query_row(
            "SELECT value_json FROM sessions WHERE id = ?1",
            [&turn.session_id],
            |row| row.get(0),
        )
        .expect("the session row is stored");
    let mut statement = connection
        .prepare("SELECT value_json FROM messages WHERE session_id = ?1 ORDER BY position")
        .expect("the transcript query prepares");
    let messages: Vec<String> = statement
        .query_map([&turn.session_id], |row| row.get::<_, String>(0))
        .expect("the transcript rows read")
        .map(|row| row.expect("a transcript row reads"))
        .collect();
    (
        serde_json::from_str(&row).expect("the session row is JSON"),
        messages,
    )
}

/// Stored transcript rows that disclose an identity collision.
fn stored_collision_notices(turn: &CheckedTurn) -> Vec<String> {
    stored_session(turn)
        .1
        .into_iter()
        .filter(|message| message.contains(ENGRAM_BIND_COLLISION_PHRASE))
        .collect()
}

/// The owed bind collides: the next tick sends it and Engram answers with an
/// idempotency conflict. Returns the request sent.
fn collide(turn: &CheckedTurn) -> Value {
    answer_next(
        turn,
        Err(EngramTransportError::remote(EngramControlErrorBody {
            code: "control_operation_idempotency_conflict".to_owned(),
            message: "the idempotency key was used for another request".to_owned(),
        })),
    );
    tick(turn);
    let binds = requests_of(turn, "verification_bind");
    assert_eq!(binds.len(), 1, "{binds:#?}");
    binds[0].clone()
}

/// Engram answers the bind with an idempotency conflict: an earlier,
/// different operation holds the key, and this bind was rejected unapplied.
/// The member is told it is not recorded, as an identity collision with its
/// key, never as bound; the notice is written to the session's transcript by
/// the same commit that retires the owed bind; and nothing is sent again,
/// neither by this process nor by a host restarted from the store.
#[test]
fn an_identity_collision_retires_its_bind_with_a_durable_notice_and_no_resend() {
    let (turn, _, _) = owed_batch_turn("carried-batch-collision");
    tick(&turn);
    // Before the answer, the store holds the decided owed bind and no notice.
    let (row, _) = stored_session(&turn);
    assert_eq!(
        row["engramOwedBinds"].as_array().map(Vec::len),
        Some(1),
        "{row:#}"
    );
    assert!(stored_collision_notices(&turn).is_empty());
    let sent = collide(&turn);
    assert!(
        turn.record(|record| record.engram.owed_binds.is_empty()),
        "the collision retires the owed bind"
    );
    let notices: Vec<String> = turn.record(|record| {
        record
            .session
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::Text { author: Author::System, text, .. } => Some(text.clone()),
                _ => None,
            })
            .filter(|text| text.contains(ENGRAM_BIND_COLLISION_PHRASE))
            .collect()
    });
    assert_eq!(notices.len(), 1, "one durable transcript notice: {notices:?}");
    let notice = &notices[0];
    let key = sent["idempotency_key"].as_str().unwrap();
    assert!(notice.contains(key), "{notice}");
    assert!(notice.contains("not recorded"), "{notice}");
    assert!(!notice.contains("(bound)"), "{notice}");
    // Later ticks have nothing to send: never again under this key or a new one.
    turn.state.engram_owed_binds_tick(
        chrono::Utc::now() + chrono::Duration::minutes(10),
        DEADLOCK_GUARD,
    );
    assert_eq!(requests_of(&turn, "verification_bind").len(), 1);

    // The settlement is in the store: the row has no owed bind left, and the
    // transcript holds the one notice naming the key.
    let (row, _) = stored_session(&turn);
    assert!(row.get("engramOwedBinds").is_none(), "{row:#}");
    let stored = stored_collision_notices(&turn);
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert!(stored[0].contains(key), "{}", stored[0]);

    // A state loaded from that store has nothing to reschedule, and a host
    // restarted from it sends no bind.
    let path = turn.state.persistence_path.as_path();
    let loaded = load_state(path)
        .expect("the state loads")
        .expect("a state was stored");
    let index = loaded
        .find_session_index(&turn.session_id)
        .expect("the session loads");
    assert!(loaded.sessions[index].engram.owed_binds.is_empty());
    turn.state.trigger_shutdown_signal();
    turn.state.shutdown_persist_blocking();
    let transport = ScriptedEngramControlTransport::new(vec![]);
    let restarted = AppState::new_with_paths_engram_transport_and_clock_for_test(
        turn.root.to_string_lossy().into_owned(),
        path.to_path_buf(),
        turn.state.orchestrator_templates_path.as_path().to_path_buf(),
        transport.clone(),
        EngramBudgetClock::scripted(),
    )
    .expect("a host restarts from the store");
    restarted.engram_owed_binds_tick(
        chrono::Utc::now() + chrono::Duration::minutes(10),
        DEADLOCK_GUARD,
    );
    assert!(
        !transport
            .requests()
            .iter()
            .any(|recorded| recorded.request["operation"] == "verification_bind"),
        "a settled collision is never rescheduled after a restart"
    );
    restarted.trigger_shutdown_signal();
    restarted.shutdown_persist_blocking();
}

/// The commit that would settle a collision fails while the process lives:
/// the entry is already retired in memory, so nothing is sent again, and the
/// store still holds the owed bind unchanged, without a notice. Only that
/// unsettled store, loaded again, sends the bind once more, and then as the
/// identical request under the same key, which Engram rejects again unapplied.
#[test]
fn a_collision_whose_settlement_is_not_stored_is_never_resent_live_and_replays_identically_after_reload() {
    let (turn, _, _) = owed_batch_turn("carried-batch-collision-unstored");
    tick(&turn);
    let (before, _) = stored_session(&turn);
    assert_eq!(
        before["engramOwedBinds"].as_array().map(Vec::len),
        Some(1),
        "{before:#}"
    );
    // Fail exactly the write that would retire the stored owed bind.
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path())
        .expect("the store opens");
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_collision_settlement BEFORE UPDATE ON sessions
             WHEN NEW.id = '{}'
               AND instr(OLD.value_json, '\"engramOwedBinds\"') > 0
               AND instr(NEW.value_json, '\"engramOwedBinds\"') = 0
             BEGIN SELECT RAISE(ABORT, 'fixture collision settlement failure'); END;",
            turn.session_id
        ))
        .expect("the trigger installs");
    let sent = collide(&turn);
    assert!(turn.record(|record| record.engram.owed_binds.is_empty()));

    // While the process lives, nothing is sent again.
    turn.state.engram_owed_binds_tick(
        chrono::Utc::now() + chrono::Duration::minutes(10),
        DEADLOCK_GUARD,
    );
    assert_eq!(requests_of(&turn, "verification_bind").len(), 1);

    // The store is as before the answer: the owed bind unchanged, no notice.
    let (after, _) = stored_session(&turn);
    assert_eq!(after["engramOwedBinds"], before["engramOwedBinds"]);
    assert!(stored_collision_notices(&turn).is_empty());

    // Loaded again, the unsettled entry is sent as the identical request.
    connection
        .execute_batch("DROP TRIGGER fail_collision_settlement")
        .expect("the trigger drops");
    drop(connection);
    let loaded = load_state(turn.state.persistence_path.as_path())
        .expect("the state loads")
        .expect("a state was stored");
    let index = loaded
        .find_session_index(&turn.session_id)
        .expect("the session loads");
    let owed = &loaded.sessions[index].engram.owed_binds;
    assert_eq!(owed.len(), 1, "{owed:?}");
    assert_eq!(
        engram_owed_bind_step(&owed[0], chrono::Utc::now()),
        EngramOwedBindStep::Send
    );
    let token = sent["routing_token"].as_str().expect("the bind names its token");
    let replay = engram_owed_bind_request(&owed[0], token).expect("the bind is decided");
    assert_eq!(
        serde_json::to_value(replay).expect("the request serializes"),
        sent,
        "the identical request under the same key"
    );
}

#[test]
fn a_credential_refusal_never_rebinds_a_session_while_its_turn_is_open() {
    let (turn, _, _) = owed_batch_turn("carried-batch-busy-refresh");
    tick(&turn);
    // A later turn of the session is running, its grant open.
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = SessionStatus::Active;
        record.engram.active_grant_id = Some("grant-live".to_owned());
    }
    answer_next(
        &turn,
        Err(EngramTransportError::remote(EngramControlErrorBody {
            code: "control_connection_superseded".to_owned(),
            message: "another connection took over".to_owned(),
        })),
    );
    let statuses = requests_of(&turn, "session_status").len();
    let binds = requests_of(&turn, "session_bind").len();
    tick(&turn);

    // The refusal marks the credentials stale, but no rebind runs: its
    // recovery would close the running turn's grant without its report.
    assert_eq!(requests_of(&turn, "verification_bind").len(), 1);
    assert_eq!(requests_of(&turn, "session_status").len(), statuses);
    assert_eq!(requests_of(&turn, "session_bind").len(), binds);
    assert!(turn.record(|record| record.engram.rebind_required));
    assert_eq!(
        turn.record(|record| record.engram.active_grant_id.clone()),
        Some("grant-live".to_owned()),
        "the running turn keeps its grant"
    );
    assert_eq!(turn.record(|record| record.engram.owed_binds.len()), 1);

    // Once the turn has closed, the next tick asks for the rebind.
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = SessionStatus::Idle;
        record.engram.active_grant_id = None;
    }
    answer_next(
        &turn,
        Err(EngramTransportError::transport("the rebind call was lost")),
    );
    turn.state.engram_owed_binds_tick(
        chrono::Utc::now() + chrono::Duration::seconds(10),
        DEADLOCK_GUARD,
    );
    assert_eq!(
        requests_of(&turn, "verification_bind").len(),
        1,
        "nothing is sent with stale credentials"
    );
    assert!(
        requests_of(&turn, "session_status").len() + requests_of(&turn, "session_bind").len()
            > statuses + binds,
        "an idle session is rebound"
    );
}

#[test]
fn an_owed_bind_rebind_claims_the_bind_with_its_check_so_no_admission_comes_between() {
    let (turn, _, _) = owed_batch_turn("carried-batch-rebind-claim");
    // Engram refused the session's credentials: they are marked stale, and
    // the session is idle.
    let captured = {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = SessionStatus::Idle;
        record.engram.rebind_required = true;
        let target =
            AppState::engram_binding_target_for_session_shape_locked(&inner, &turn.session_id, true)
                .unwrap()
                .unwrap();
        engram_owed_bind_credentials(&target)
    };

    // Credentials a successor has since replaced claim nothing and send
    // nothing.
    let mut superseded = captured.clone();
    superseded.1 = Some("superseded-token".to_owned());
    let before = turn.transport.requests().len();
    turn.state
        .rebind_engram_session_for_owed_binds(&turn.session_id, &superseded);
    assert_eq!(turn.transport.requests().len(), before);
    assert!(!turn.record(|record| record.engram.bind_in_progress));

    // The rebind's first call is held open: by then the bind is claimed, in
    // the same critical section as its check, so an admission arriving now
    // finds it taken and sends nothing.
    let (status, gate) = gated_engram_step(
        "session_status",
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::transport(
            "the rebind call was lost",
        ))),
    );
    let transport = GatedEngramControlTransport::new([status]);
    turn.state.install_test_engram_transport(transport.clone());
    let state = turn.state.clone();
    let session_id = turn.session_id.clone();
    let rebind = std::thread::spawn(move || {
        state.rebind_engram_session_for_owed_binds(&session_id, &captured);
    });
    gate.wait();
    assert!(turn.record(|record| record.engram.bind_in_progress));
    let admission = {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        AppState::engram_binding_target_for_session_shape_locked(&inner, &turn.session_id, true)
            .unwrap()
            .unwrap()
    };
    let refused = turn
        .state
        .bind_engram_target_off_lock(admission)
        .expect_err("a second bind of the session is refused");
    assert!(
        refused.to_string().contains("already in progress"),
        "{refused}"
    );
    assert_eq!(transport.requests().len(), 1, "the admission sent nothing");
    gate.release();
    rebind.join().expect("the rebind finishes");
    assert!(
        !turn.record(|record| record.engram.bind_in_progress),
        "the claim is released when the rebind ends"
    );
}

/// The tick's snapshot of `turn`'s session: its target and owed binds, as
/// the tick copies them before working them off the lock.
fn owed_bind_snapshot(turn: &CheckedTurn) -> EngramOwedBindWork {
    let inner = turn.state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(&turn.session_id).unwrap();
    EngramOwedBindWork {
        session_id: turn.session_id.clone(),
        target: AppState::engram_binding_target_for_session_shape_locked(
            &inner,
            &turn.session_id,
            true,
        )
        .unwrap()
        .unwrap(),
        roots: inner.engram_work_source_roots.clone(),
        owed: inner.sessions[index].engram.owed_binds.clone(),
    }
}

#[test]
fn a_settings_reset_between_the_ticks_snapshot_and_its_send_stops_the_send() {
    let (turn, gate_record, _) = owed_batch_turn("carried-batch-reset-send");
    // Decided and confirmed stored: the next tick would send it.
    tick(&turn);
    let set = |change: &dyn Fn(&mut SessionRecord)| {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.session_id).unwrap();
        change(inner.session_mut_by_index(index).unwrap());
    };
    let send = |work| {
        turn.state.work_engram_owed_binds(
            work,
            chrono::Utc::now(),
            std::time::Instant::now() + DEADLOCK_GUARD,
        )
    };

    // A reset starts after the snapshot: nothing is sent.
    let work = owed_bind_snapshot(&turn);
    set(&|record| record.engram.project_reset_in_progress = true);
    let result = send(work);
    assert!(requests_of(&turn, "verification_bind").is_empty());
    assert!(result.entries.is_empty(), "left as stored: {:?}", result.entries);
    set(&|record| record.engram.project_reset_in_progress = false);

    // A reset that dropped the owed bind after the snapshot: nothing is sent.
    let work = owed_bind_snapshot(&turn);
    let owed = turn.record(|record| record.engram.owed_binds.clone());
    set(&|record| record.engram.owed_binds.clear());
    send(work);
    assert!(requests_of(&turn, "verification_bind").is_empty());
    let restore = owed.clone();
    set(&move |record| record.engram.owed_binds = restore.clone());

    // A send that goes holds the session's binding while it runs, which a
    // settings reset waits for, and releases it when it ends.
    let (bind, gate) = gated_engram_step(
        "verification_bind",
        ScriptedEngramControlResponse::Reply(Ok(json!({
            "original": gate_record,
            "replayed": false,
            "bound": [{ "work_id": "work-member", "work_ref": "w-member",
                "verification": "b".repeat(64) }],
        }))),
    );
    let transport = GatedEngramControlTransport::new([bind]);
    turn.state.install_test_engram_transport(transport.clone());
    let work = owed_bind_snapshot(&turn);
    let state = turn.state.clone();
    let sending = std::thread::spawn(move || {
        state.work_engram_owed_binds(
            work,
            chrono::Utc::now(),
            std::time::Instant::now() + DEADLOCK_GUARD,
        )
    });
    gate.wait();
    assert!(
        turn.record(|record| record.engram.bind_in_progress),
        "a settings reset waits for the send in flight"
    );
    gate.release();
    let result = sending.join().expect("the send finishes");
    assert_eq!(transport.requests().len(), 1);
    assert!(!turn.record(|record| record.engram.bind_in_progress));
    assert_eq!(result.entries.len(), 1, "{:?}", result.entries);
    assert!(result.entries[0].1.is_some(), "settled with its line");
}

#[test]
fn a_decision_whose_store_was_not_confirmed_is_confirmed_on_a_later_tick_then_sent() {
    let (turn, gate_record, _) = owed_batch_turn("carried-batch-confirm");
    tick(&turn);
    // As if that tick's confirmation timed out: decided, not confirmed.
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.session_id).unwrap();
        inner.sessions[index].engram.owed_binds[0].decision_unconfirmed = true;
    }
    tick(&turn);
    assert!(requests_of(&turn, "verification_bind").is_empty(), "never sent unconfirmed");
    assert!(
        !turn.record(|record| record.engram.owed_binds[0].decision_unconfirmed),
        "a later tick confirms the decision stored"
    );
    answer_next(
        &turn,
        Ok(json!({ "original": gate_record, "replayed": false,
            "bound": [{ "work_id": "work-member", "verification": "b".repeat(64) }] })),
    );
    tick(&turn);
    assert_eq!(requests_of(&turn, "verification_bind").len(), 1);
    assert!(turn.record(|record| record.engram.owed_binds.is_empty()));
}

/// Scripts the session's held claims with `w-member` at `fence`.
fn script_held_at(turn: &CheckedTurn, fence: i64) {
    let launch_binding = turn.record(|record| record.engram.work_binding.clone().unwrap());
    let mut member = member_binding();
    member.claim_fence = fence;
    turn.transport.replace_held_claims([Ok(EngramHeldClaims {
        items: vec![
            EngramHeldClaim {
                work_id: launch_binding.work_id.clone(),
                claim_id: launch_binding.claim_id.clone(),
                claim_fence: launch_binding.claim_fence,
                control_binding: Some(launch_binding.clone()),
                ..Default::default()
            },
            EngramHeldClaim {
                work_id: "work-member".to_owned(),
                short_ref: "w-member".to_owned(),
                claim_id: "claim-member".to_owned(),
                claim_fence: fence,
                control_binding: Some(member),
                ..Default::default()
            },
        ],
        omitted: 0,
    })]);
}

#[test]
fn a_members_fence_is_the_one_its_claim_had_at_the_gates_launch() {
    let turn = CheckedTurn::start_with_opening(
        "carried-batch-fence",
        true,
        None,
        vec![checkpoint_reply_naming(CHECK_GRANT, &["e".repeat(32)])],
        1,
        true,
    );
    let worktree = turn.root.join(".worktrees").join("wt");
    name_member(&turn); // its root entry keeps fence 1
    // Recovered before the launch: fence 2 when the gate launches.
    script_held_at(&turn, 2);
    launch_gate(&turn, &worktree, false);
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0].bind_targets[0].claim_fence),
        2
    );
    // Moved again after the launch: the launch's fence stands, so settlement
    // sees the move.
    script_held_at(&turn, 3);
    finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    turn.finish();
    let owed = turn.record(|record| record.engram.owed_binds.clone());
    assert_eq!(owed.len(), 1, "{owed:?}");
    assert_eq!(owed[0].target.claim_fence, 2);
}

#[test]
fn a_launch_whose_held_claims_cannot_be_read_binds_no_member_and_says_so() {
    let turn = CheckedTurn::start_with_opening(
        "carried-batch-unread",
        true,
        None,
        vec![checkpoint_reply(CHECK_GRANT)],
        1,
        true,
    );
    let worktree = turn.root.join(".worktrees").join("wt");
    name_member(&turn);
    // No held claims scripted: the launch's read fails.
    launch_gate(&turn, &worktree, false);
    assert!(turn.record(|record| record.engram.carried_checks[0].bind_targets.is_empty()));
    let told = turn.record(|record| record.engram.pending_source_root_line.clone().unwrap_or_default());
    assert!(
        told.contains(
            "the full gate is not recorded for w-member: its claim's fence at the batch gate's \
             launch could not be read"
        ),
        "{told}"
    );
}

#[test]
fn stale_credentials_ask_for_a_rebind_again_under_the_binds_own_backoff() {
    let (turn, _, _) = owed_batch_turn("carried-batch-stale");
    let work = {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.session_id).unwrap();
        let mut target =
            AppState::engram_binding_target_for_session_shape_locked(&inner, &turn.session_id, true)
                .unwrap()
                .unwrap();
        // An earlier rebind failed: the credentials are still marked stale.
        target.rebind_required = true;
        EngramOwedBindWork {
            session_id: turn.session_id.clone(),
            target,
            roots: inner.engram_work_source_roots.clone(),
            owed: inner.sessions[index].engram.owed_binds.clone(),
        }
    };
    let before = turn.transport.requests().len();
    let result = turn.state.work_engram_owed_binds(
        work,
        chrono::Utc::now(),
        std::time::Instant::now() + DEADLOCK_GUARD,
    );
    assert!(result.refresh, "the rebind is asked for again");
    assert_eq!(turn.transport.requests().len(), before, "nothing sent with stale credentials");
    let (entry, line) = &result.entries[0];
    assert_eq!(line, &None);
    // Nothing was sent: the wait counts no attempt, so a bind owed after a
    // restart keeps the attempt it was given. Its time cap bounds the wait.
    assert_eq!(entry.attempts, 0);
    assert!(entry.next_attempt_at.is_some(), "asked again after a wait");
    assert_eq!(entry.last_detail.as_deref(), Some("waiting for fresh Engram credentials"));
}

#[test]
fn owed_binds_wait_while_boot_recovery_restores_the_sessions_credentials() {
    let (turn, _, _) = owed_batch_turn("carried-batch-boot");
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.session_id).unwrap();
        inner.sessions[index].engram_boot_recovery_pending = true;
    }
    let before = turn.transport.requests().len();
    let owed = turn.record(|record| record.engram.owed_binds.clone());
    tick(&turn);
    assert_eq!(turn.transport.requests().len(), before, "no call races boot recovery");
    assert_eq!(turn.record(|record| record.engram.owed_binds.clone()), owed, "nothing counted");
}

#[test]
fn a_member_missing_from_a_cut_short_held_list_at_launch_is_not_recorded() {
    let turn = CheckedTurn::start_with_opening(
        "carried-batch-cut",
        true,
        None,
        vec![checkpoint_reply(CHECK_GRANT)],
        1,
        true,
    );
    let worktree = turn.root.join(".worktrees").join("wt");
    name_member(&turn);
    let launch_binding = turn.record(|record| record.engram.work_binding.clone().unwrap());
    // Engram listed only the newest held claims; the member is not among them.
    turn.transport.replace_held_claims([Ok(EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: launch_binding.work_id.clone(),
            claim_id: launch_binding.claim_id.clone(),
            claim_fence: launch_binding.claim_fence,
            control_binding: Some(launch_binding.clone()),
            ..Default::default()
        }],
        omitted: 1,
    })]);
    launch_gate(&turn, &worktree, false);
    assert!(turn.record(|record| record.engram.carried_checks[0].bind_targets.is_empty()));
    let told = turn.record(|record| record.engram.pending_source_root_line.clone().unwrap_or_default());
    assert!(told.contains("w-member") && told.contains("was cut short"), "{told}");
}

#[test]
fn a_refusal_of_credentials_the_session_has_since_replaced_marks_nothing_stale() {
    let (turn, _, _) = owed_batch_turn("carried-batch-replaced");
    tick(&turn); // decided and confirmed
    // The worker's snapshot, taken under the old token...
    let work = {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.session_id).unwrap();
        EngramOwedBindWork {
            session_id: turn.session_id.clone(),
            target: AppState::engram_binding_target_for_session_shape_locked(
                &inner,
                &turn.session_id,
                true,
            )
            .unwrap()
            .unwrap(),
            roots: inner.engram_work_source_roots.clone(),
            owed: inner.sessions[index].engram.owed_binds.clone(),
        }
    };
    let replace_token = |token: &str| {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.session_id).unwrap();
        inner.sessions[index].engram.routing_token = Some(token.to_owned());
    };
    // ...then the session is rebound to a successor token before the send:
    // the old snapshot sends nothing, so nothing is refused or marked.
    replace_token("successor-token");
    let result = turn.state.work_engram_owed_binds(
        work,
        chrono::Utc::now(),
        std::time::Instant::now() + DEADLOCK_GUARD,
    );
    assert!(requests_of(&turn, "verification_bind").is_empty());
    assert!(result.entries.is_empty(), "left as stored: {:?}", result.entries);
    assert!(!result.refresh);
    assert!(!turn.record(|record| record.engram.rebind_required));

    // A send under the successor token, whose session is rebound again
    // while the call is out: the refusal answering the replaced token marks
    // nothing stale. The snapshot carries the transport it calls, so it is
    // taken once the gated one is installed.
    let (bind, gate) = gated_engram_step(
        "verification_bind",
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::remote(
            EngramControlErrorBody {
                code: "control_session_token_mismatch".to_owned(),
                message: "the routing token is not the session's".to_owned(),
            },
        ))),
    );
    let transport = GatedEngramControlTransport::new([bind]);
    turn.state.install_test_engram_transport(transport.clone());
    let work = owed_bind_snapshot(&turn);
    let state = turn.state.clone();
    let sending = std::thread::spawn(move || {
        state.work_engram_owed_binds(
            work,
            chrono::Utc::now(),
            std::time::Instant::now() + DEADLOCK_GUARD,
        )
    });
    gate.wait();
    replace_token("third-token");
    gate.release();
    let result = sending.join().expect("the send finishes");

    assert_eq!(transport.requests().len(), 1);
    assert!(!result.refresh, "no rebind for credentials already replaced");
    assert!(
        !turn.record(|record| record.engram.rebind_required),
        "the successor credentials stay healthy"
    );
    let (entry, line) = &result.entries[0];
    assert_eq!(line, &None, "kept owed, to go again under the newest token");
    assert_eq!(entry.attempts, 1);
}

#[test]
fn a_reset_tells_each_owed_member_whether_it_may_be_bound() {
    let turn = CheckedTurn::start_with_opening(
        "carried-batch-reset",
        true,
        None,
        vec![checkpoint_reply(CHECK_GRANT)],
        1,
        true,
    );
    let undecided = EngramOwedBind {
        original: "1".repeat(32),
        measurement: batch_measurement(),
        target: batch_target("undecided"),
        decided: None,
        decision_unconfirmed: false,
        attempts: 0,
        first_owed_at: "2026-10-18T23:30:00.000Z".to_owned(),
        last_detail: None,
        next_attempt_at: None,
    };
    let decided = EngramOwedBind {
        target: batch_target("decided"),
        decided: Some(EngramOwedBindDecided {
            binding: member_binding(),
            sighting: EngramBindSighting {
                observation: "a".repeat(32),
                source_revision: "content-v1:batch".to_owned(),
            },
        }),
        ..undecided.clone()
    };
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.session_id).unwrap();
        inner.sessions[index].engram.owed_binds = vec![undecided, decided];
    }
    let why = "the project's Engram settings changed";
    let lines = turn.record(|record| engram_carried_checks_reset_lines(record, why));
    assert!(lines.contains(&engram_batch_not_recorded_line("w-undecided", why)), "{lines:?}");
    assert!(lines.contains(&engram_batch_unconfirmed_line("w-decided", why)), "{lines:?}");
}

#[test]
fn a_tick_with_no_budget_left_calls_engram_for_no_owed_bind() {
    let (turn, _, _) = owed_batch_turn("carried-batch-budget");
    let before = turn.transport.requests().len();
    turn.state
        .engram_owed_binds_tick(chrono::Utc::now(), Duration::ZERO);
    assert_eq!(turn.transport.requests().len(), before, "no Engram call past the budget");
    let owed = turn.record(|record| record.engram.owed_binds.clone());
    assert_eq!((owed[0].attempts, owed[0].decided.is_none()), (0, true));
}

#[test]
fn a_refused_checkpoint_tells_each_member_its_batch_gate_is_not_recorded() {
    let turn = CheckedTurn::start_with_opening(
        "carried-batch-refused",
        true,
        None,
        vec![checkpoint_refusal_reply("stale_fence")],
        1,
        true,
    );
    let worktree = turn.root.join(".worktrees").join("wt");
    name_member(&turn);
    script_held(&turn);
    launch_gate(&turn, &worktree, false);
    finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    turn.finish();

    assert!(turn.record(|record| record.engram.pending_batches.is_empty()));
    assert!(turn.record(|record| record.engram.owed_binds.is_empty()));
    let told = turn.record(|record| record.engram.pending_source_root_line.clone().unwrap_or_default());
    assert!(
        told.contains(
            "the full gate is not recorded for w-member: Engram refused the checkpoint that \
             carried the batch gate (stale_fence)."
        ),
        "{told}"
    );
}

/// The session's mutation stamp: the persist delta writes a session only
/// once its stamp passes the last one written.
fn stamp(turn: &CheckedTurn) -> u64 {
    turn.record(|record| record.mutation_stamp)
}

#[test]
fn what_a_tick_settles_without_engram_is_stamped_for_the_store() {
    let turn = CheckedTurn::start_with_opening(
        "carried-batch-settled",
        true,
        None,
        vec![checkpoint_reply(CHECK_GRANT)],
        1,
        true,
    );
    let long_ago = (chrono::Utc::now() - chrono::Duration::minutes(31))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.session_id).unwrap();
        let engram = &mut inner.sessions[index].engram;
        // A batch whose checkpoint never answered, and a bind owed too long.
        engram.pending_batches.push(EngramPendingBatch {
            checkpoint_grant_id: "grant-unanswered".to_owned(),
            producer_observation_id: "9".repeat(64),
            measurement: batch_measurement(),
            targets: vec![batch_target("pending")],
            pended_at: long_ago.clone(),
        });
        engram.owed_binds.push(EngramOwedBind {
            original: "1".repeat(64),
            measurement: batch_measurement(),
            target: batch_target("owed"),
            decided: None,
            decision_unconfirmed: false,
            attempts: 3,
            first_owed_at: long_ago,
            last_detail: Some("busy".to_owned()),
            next_attempt_at: None,
        });
    }
    let before = stamp(&turn);
    tick(&turn);

    assert!(turn.record(|record| record.engram.pending_batches.is_empty()));
    assert!(turn.record(|record| record.engram.owed_binds.is_empty()));
    assert!(stamp(&turn) > before, "settled entries must reach the store");
    let told = turn.record(|record| record.engram.pending_source_root_line.clone().unwrap_or_default());
    assert!(told.contains("w-pending") && told.contains("not confirmed within 30 minutes"), "{told}");
    assert!(told.contains("w-owed") && told.contains("never sent"), "{told}");
}

#[test]
fn a_refused_checkpoints_drained_batches_are_stamped_for_the_store() {
    let turn = CheckedTurn::start_with_opening(
        "carried-batch-refused-stamp",
        true,
        None,
        vec![checkpoint_reply(CHECK_GRANT)],
        1,
        true,
    );
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.session_id).unwrap();
        inner.sessions[index].engram.pending_batches.push(EngramPendingBatch {
            checkpoint_grant_id: CHECK_GRANT.to_owned(),
            producer_observation_id: "9".repeat(64),
            measurement: batch_measurement(),
            targets: vec![batch_target("refused")],
            pended_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        });
    }
    let before = stamp(&turn);
    turn.state
        .forget_refused_engram_turn_report(&turn.session_id, CHECK_GRANT, Some("stale_fence"), true);

    assert!(turn.record(|record| record.engram.pending_batches.is_empty()));
    assert!(stamp(&turn) > before, "the drained batch must reach the store");
}

fn batch_target(work: &str) -> EngramBindTarget {
    EngramBindTarget {
        work_id: format!("work-{work}"),
        short_ref: format!("w-{work}"),
        claim_id: format!("claim-{work}"),
        claim_fence: 1,
        root_generation: MEMBER_GENERATION,
    }
}

fn batch_measurement() -> EngramBindMeasurement {
    EngramBindMeasurement {
        workspace_id: "workspace".to_owned(),
        source_revision: "content-v1:batch".to_owned(),
        measured_at: "2026-10-18T23:30:00.000Z".to_owned(),
    }
}

#[test]
fn a_restart_never_leaves_a_batch_member_untold() {
    let turn = CheckedTurn::start_with_opening(
        "carried-batch-restart",
        true,
        None,
        vec![checkpoint_reply(CHECK_GRANT)],
        1,
        true,
    );
    let worktree = turn.root.join(".worktrees").join("wt");
    name_member(&turn);
    // Its held claims are read at the launch, which records the member.
    script_held(&turn);
    // A batch gate still carried; an earlier one credited whose checkpoint
    // had not answered; and a bind owed after an answer.
    launch_gate(&turn, &worktree, false);
    let owed = EngramOwedBind {
        original: "1".repeat(64),
        measurement: batch_measurement(),
        target: batch_target("owed"),
        decided: None,
        decision_unconfirmed: false,
        attempts: 2,
        first_owed_at: "2026-10-18T23:30:00.000Z".to_owned(),
        last_detail: Some("busy".to_owned()),
        next_attempt_at: None,
    };
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.session_id).unwrap();
        let engram = &mut inner.sessions[index].engram;
        engram.pending_batches.push(EngramPendingBatch {
            checkpoint_grant_id: CHECK_GRANT.to_owned(),
            producer_observation_id: "9".repeat(64),
            measurement: batch_measurement(),
            targets: vec![batch_target("pending")],
            pended_at: "2026-10-18T23:30:00.000Z".to_owned(),
        });
        engram.owed_binds.push(owed.clone());
    }
    let persisted = turn.record(|record| {
        serde_json::to_value(PersistedSessionRecord::from_record(record))
            .expect("the record serializes")
    });
    assert_eq!(
        persisted["engramCarriedLaunches"][0]["bindMembers"],
        json!(["w-member"]),
        "{persisted:#}"
    );

    let loaded: PersistedSessionRecord =
        serde_json::from_value(persisted).expect("the record deserializes");
    let record = loaded.into_record().expect("the record loads");

    // The owed bind is sent again; every member whose bind was not owed yet
    // is told, definitely, to run its gate again.
    assert_eq!(record.engram.owed_binds.len(), 1);
    let loaded = &record.engram.owed_binds[0];
    assert_eq!(
        (&loaded.original, &loaded.target, loaded.attempts, &loaded.last_detail),
        (&owed.original, &owed.target, owed.attempts, &owed.last_detail)
    );
    assert_ne!(loaded.first_owed_at, owed.first_owed_at, "owed afresh from the load");
    assert!(record.engram.pending_batches.is_empty());
    let told = record
        .engram
        .pending_source_root_line
        .expect("the holder should be told");
    for member in ["w-member", "w-pending"] {
        assert!(
            told.contains(&format!("the full gate is not recorded for {member}: TermAl restarted")),
            "{told}\n missing: {member}"
        );
    }
}
