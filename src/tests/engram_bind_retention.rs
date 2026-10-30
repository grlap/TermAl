//! A session_bind sent outside queued admission keeps its identity across a
//! lost answer (`src/engram_bind_retention.rs`): it is retained before it is
//! sent, replayed identically before the session is bound again, settled
//! only by its own answer, and given up, told, when its connection or
//! settings changed or Engram refused the replay itself.
//!
//! Owns these tests and the producer double they run against, which keeps one
//! control session row the way Engram's `bind_control_session_with_work`
//! does: a bind under the row's key reads the row back with its current
//! status, a different key replaces it. Does not own the bind's other steps
//! (`engram_host_adapter.rs` tests) or queued admission binds. New module.

use super::*;

/// One control session's row as Engram keeps it: the bind key and request
/// last applied, the routing token it minted, and how many binds wrote it.
#[derive(Default)]
struct BindRow {
    key: Option<String>,
    request: Option<Value>,
    token: Option<String>,
    writes: usize,
}

#[derive(Clone, Copy)]
enum BindAnswer {
    /// Engram applies the bind (or reads back the row its key matches) and
    /// its answer arrives.
    Answer,
    /// Engram applies it, and the answer is lost on the way back.
    ApplyThenLose,
    /// The call is lost before Engram applies anything; nothing proves it.
    LoseUnapplied,
    /// The transport proves the call never left this host.
    Unsent,
    /// Engram refuses with this code and writes nothing.
    Refuse(&'static str),
    /// Engram applies it, and an answer with no routing token arrives.
    ApplyThenInvalid,
    /// Engram applies it, and the answer's status names another session.
    ApplyOtherSession,
    /// Engram applies it, and the answer's status names no session.
    ApplyNoSession,
    /// Engram applies it, and the answer's status session is null.
    ApplyNullSession,
    /// Engram applies it, and the answer's status has an empty phase.
    ApplyEmptyPhase,
    /// Engram applies it (or reads it back) and answers that the session is
    /// in a turn.
    ApplyInTurn,
}

/// A producer of session binds behind the scripted transport, which still
/// answers every other call and every work-binding read.
struct BindProducer {
    base: Arc<ScriptedEngramControlTransport>,
    row: Mutex<BindRow>,
    answers: Mutex<VecDeque<BindAnswer>>,
    binds: Mutex<Vec<Value>>,
}

impl BindProducer {
    fn binds(&self) -> Vec<Value> {
        self.binds.lock().unwrap().clone()
    }

    fn writes(&self) -> usize {
        self.row.lock().unwrap().writes
    }

    fn script(&self, answers: impl IntoIterator<Item = BindAnswer>) {
        self.answers.lock().unwrap().extend(answers);
    }
}

impl EngramControlTransport for BindProducer {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        if !matches!(request, EngramControlRequest::SessionBind { .. }) {
            return self.base.request(connection, request, timeout);
        }
        let wire = serde_json::to_value(request).unwrap();
        self.binds.lock().unwrap().push(wire.clone());
        let answer = self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(BindAnswer::Answer);
        match answer {
            BindAnswer::Refuse(code) => {
                return Err(EngramTransportError::remote(EngramControlErrorBody {
                    code: code.to_owned(),
                    message: format!("Engram refused the bind ({code})"),
                }));
            }
            BindAnswer::LoseUnapplied => {
                return Err(EngramTransportError::transport("the bind call was lost"));
            }
            BindAnswer::Unsent => {
                return Err(EngramTransportError::transport("the control process was gone")
                    .request_never_sent());
            }
            BindAnswer::Answer
            | BindAnswer::ApplyThenLose
            | BindAnswer::ApplyThenInvalid
            | BindAnswer::ApplyOtherSession
            | BindAnswer::ApplyNoSession
            | BindAnswer::ApplyNullSession
            | BindAnswer::ApplyEmptyPhase
            | BindAnswer::ApplyInTurn => {}
        }
        let key = wire["idempotency_key"].as_str().unwrap().to_owned();
        let token = {
            let mut row = self.row.lock().unwrap();
            if row.key.as_deref() == Some(key.as_str()) {
                if row.request.as_ref() != Some(&wire) {
                    return Err(EngramTransportError::remote(EngramControlErrorBody {
                        code: "control_session_bind_conflict".to_owned(),
                        message: "the bind key holds another intent".to_owned(),
                    }));
                }
                // Read back: the current row, nothing written.
                row.token.clone().unwrap()
            } else {
                row.writes += 1;
                let token = format!("token-{}", row.writes);
                row.key = Some(key);
                row.request = Some(wire);
                row.token = Some(token.clone());
                token
            }
        };
        // As Engram does, the status names the session it describes, unless
        // the case scripts a malformed answer.
        let mut described = json!(connection.session_id);
        let (token, phase) = match answer {
            BindAnswer::ApplyThenLose => {
                return Err(EngramTransportError::transport(
                    "the bind's answer was lost after Engram applied it",
                ));
            }
            BindAnswer::ApplyThenInvalid => (String::new(), "ready"),
            BindAnswer::ApplyOtherSession => {
                described = json!("another-session");
                (token, "ready")
            }
            BindAnswer::ApplyNullSession => {
                described = Value::Null;
                (token, "ready")
            }
            BindAnswer::ApplyEmptyPhase => (token, ""),
            BindAnswer::ApplyInTurn => (token, "turn_open"),
            _ => (token, "ready"),
        };
        if matches!(answer, BindAnswer::ApplyNoSession) {
            return Ok(json!({ "routing_token": token, "status": { "phase": phase } }));
        }
        Ok(json!({
            "routing_token": token,
            "status": { "phase": phase, "session_id": described }
        }))
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.base.read_work_binding(connection, preference, timeout)
    }

    fn shutdown_session(&self, session_id: &str) {
        self.base.shutdown_session(session_id);
    }
}

/// A root session with Engram enabled behind a `BindProducer` whose focus
/// reads return `focus`, one per bind, then none, and whose other calls are
/// answered with `others` in order.
fn retention_fixture(
    label: &str,
    focus: Vec<Option<EngramControlWorkBinding>>,
    others: Vec<ScriptedEngramControlResponse>,
) -> (AppState, String, Arc<BindProducer>) {
    let (state, runtime_rx) =
        test_app_state_with_delegation_codex_runtime(&format!("engram-bind-retention-{label}"));
    // No turn runs here: drain what the runtime is sent.
    std::thread::spawn(move || while runtime_rx.recv().is_ok() {});
    state.select_test_scripted_engram_budget_clock();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join(format!("engram-bind-retention-{label}-project"));
    fs::create_dir_all(&root).expect("project root should exist");
    let project = create_test_project(&state, &root, "Engram bind retention");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    enable_test_project_engram(&state, &project, &root);
    let producer = Arc::new(BindProducer {
        base: ScriptedEngramControlTransport::new_with_work_bindings(
            others,
            focus.into_iter().map(Ok),
        ),
        row: Mutex::new(BindRow::default()),
        answers: Mutex::new(VecDeque::new()),
        binds: Mutex::new(Vec::new()),
    });
    state.install_control_test_transport(producer.clone());
    (state, session, producer)
}

/// Binds the session outside queued admission, as boot recovery or a rebind
/// does.
fn bind(state: &AppState, session: &str) -> std::result::Result<String, EngramTransportError> {
    let target = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        AppState::engram_binding_target_for_session_shape_locked(&inner, session, true)
            .expect("the session has a binding target")
            .expect("the session is Engram-enabled")
    };
    state.bind_engram_target_off_lock(target)
}

fn retained_key(state: &AppState, session: &str) -> Option<String> {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(session).expect("session exists");
    inner.sessions[index]
        .engram
        .retained_bind
        .as_ref()
        .map(|retained| retained.key().to_owned())
}

fn pending_line(state: &AppState, session: &str) -> String {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(session).expect("session exists");
    inner.sessions[index]
        .engram
        .pending_source_root_line
        .clone()
        .unwrap_or_default()
}

/// Forgets the session's routing token, so its next call binds again.
fn unbind_locally(state: &AppState, session: &str) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(session).expect("session exists");
    inner.sessions[index].engram.routing_token = None;
}

#[test]
fn a_lost_bind_answer_is_replayed_identically_and_writes_once() {
    let (state, session, producer) = retention_fixture("lost", Vec::new(), Vec::new());
    producer.script([BindAnswer::ApplyThenLose]);
    let error = bind(&state, &session).expect_err("the answer was lost");
    assert_eq!(error.kind, EngramTransportErrorKind::Transport);
    let original = producer.binds()[0].clone();
    assert_eq!(
        retained_key(&state, &session).as_deref(),
        original["idempotency_key"].as_str(),
        "retained before it was sent, unresolved after the lost answer"
    );

    let token = bind(&state, &session).expect("the replay reads the row back");
    let binds = producer.binds();
    assert_eq!(binds.len(), 2, "{binds:#?}");
    assert_eq!(binds[1], original, "the identical request under its key");
    assert_eq!(producer.writes(), 1, "no second bind mutation");
    assert_eq!(token, "token-1", "the token the original minted");
    assert_eq!(retained_key(&state, &session), None, "settled by its answer");
}

#[test]
fn a_moved_focus_first_recovers_the_original_then_binds_anew() {
    let first = test_control_work_binding("retained-first", 1);
    let moved = test_control_work_binding("retained-moved", 2);
    let (state, session, producer) =
        retention_fixture("moved", vec![Some(first.clone()), Some(moved.clone())], Vec::new());
    producer.script([BindAnswer::ApplyThenLose]);
    bind(&state, &session).expect_err("the answer was lost");
    let original = producer.binds()[0].clone();

    let token = bind(&state, &session).expect("recovered, then bound anew");
    let binds = producer.binds();
    assert_eq!(binds.len(), 3, "{binds:#?}");
    assert_eq!(binds[1], original, "the original first, exactly");
    assert_eq!(binds[2]["work_binding"], serde_json::to_value(&moved).unwrap());
    assert_ne!(binds[2]["idempotency_key"], original["idempotency_key"]);
    assert_eq!(producer.writes(), 2, "the original once, then the new focus");
    assert_eq!(token, "token-2");
    assert_eq!(retained_key(&state, &session), None);
}

#[test]
fn an_unresolved_replay_stays_retained_and_is_replayed_again() {
    let (state, session, producer) = retention_fixture("unresolved", Vec::new(), Vec::new());
    producer.script([BindAnswer::LoseUnapplied, BindAnswer::LoseUnapplied]);
    bind(&state, &session).expect_err("the first call was lost");
    let original = producer.binds()[0].clone();
    bind(&state, &session).expect_err("its replay was lost too");
    assert_eq!(
        retained_key(&state, &session).as_deref(),
        original["idempotency_key"].as_str(),
        "a lost replay settles nothing"
    );
    bind(&state, &session).expect("the third send is answered");
    let binds = producer.binds();
    assert_eq!(binds.len(), 3, "{binds:#?}");
    assert!(binds.iter().all(|bind| *bind == original), "{binds:#?}");
    assert_eq!(producer.writes(), 1);
    assert_eq!(retained_key(&state, &session), None);
}

/// Every answer that proves the original's one send wrote nothing settles
/// it: each of Engram's no-write refusals, a certified-not-applied answer,
/// and a call the transport proves never left. It is not retained and never
/// sent again.
#[test]
fn every_proof_that_the_original_wrote_nothing_settles_it() {
    let answers = ENGRAM_SESSION_BIND_NO_WRITE_REFUSALS
        .iter()
        .copied()
        .chain(["control_store_busy", "control_request_expired"])
        .map(BindAnswer::Refuse)
        .chain([BindAnswer::Unsent]);
    for (case, answer) in answers.enumerate() {
        let (state, session, producer) =
            retention_fixture(&format!("unapplied-original-{case}"), Vec::new(), Vec::new());
        producer.script([answer]);
        // A stale-fence refusal is retried once with a fresh bind; the others
        // fail the call.
        let _ = bind(&state, &session);
        let refused = producer.binds()[0].clone();
        assert_eq!(retained_key(&state, &session), None, "case {case}");
        unbind_locally(&state, &session);
        bind(&state, &session).expect("a later bind");
        assert_eq!(
            producer
                .binds()
                .iter()
                .filter(|bind| bind["idempotency_key"] == refused["idempotency_key"])
                .count(),
            1,
            "case {case}: the refused original is never sent again"
        );
    }
}

fn routing_token_of(state: &AppState, session: &str) -> Option<String> {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(session).expect("session exists");
    inner.sessions[index].engram.routing_token.clone()
}

/// Each answer that cannot be this session's binding (no routing token, a
/// status naming another session, an empty phase) settles nothing and its
/// token is never used, whether it answers the original or the replay. The
/// original stays retained, and a later valid answer to its replay binds the
/// session.
#[test]
fn an_invalid_answer_settles_nothing_and_is_never_adopted() {
    for (case, invalid) in [
        BindAnswer::ApplyThenInvalid,
        BindAnswer::ApplyOtherSession,
        BindAnswer::ApplyNoSession,
        BindAnswer::ApplyNullSession,
        BindAnswer::ApplyEmptyPhase,
    ]
    .into_iter()
    .enumerate()
    {
        // To the original.
        let (state, session, producer) =
            retention_fixture(&format!("invalid-original-{case}"), Vec::new(), Vec::new());
        producer.script([invalid]);
        let error = bind(&state, &session).expect_err("the answer is invalid");
        assert_eq!(error.kind, EngramTransportErrorKind::Protocol, "case {case}");
        let original = producer.binds()[0].clone();
        assert_eq!(
            retained_key(&state, &session).as_deref(),
            original["idempotency_key"].as_str(),
            "case {case}"
        );
        assert_eq!(routing_token_of(&state, &session), None, "case {case}");
        let token = bind(&state, &session).expect("the replay is answered");
        assert_eq!(producer.binds()[1], original, "case {case}");
        assert_eq!(token, "token-1", "case {case}");
        assert_eq!(retained_key(&state, &session), None, "case {case}");

        // To the replay.
        let (state, session, producer) =
            retention_fixture(&format!("invalid-replay-{case}"), Vec::new(), Vec::new());
        producer.script([BindAnswer::LoseUnapplied, invalid]);
        bind(&state, &session).expect_err("the first call was lost");
        let original = producer.binds()[0].clone();
        let error = bind(&state, &session).expect_err("the replay's answer is invalid");
        assert_eq!(error.kind, EngramTransportErrorKind::Protocol, "case {case}");
        assert_eq!(producer.binds().len(), 2, "case {case}: no replacement is sent");
        assert_eq!(
            retained_key(&state, &session).as_deref(),
            original["idempotency_key"].as_str(),
            "case {case}"
        );
        assert_eq!(routing_token_of(&state, &session), None, "case {case}");
        let token = bind(&state, &session).expect("the next replay is answered");
        assert!(producer.binds().iter().all(|bind| *bind == original), "case {case}");
        assert_eq!(token, "token-1", "case {case}");
        assert_eq!(retained_key(&state, &session), None, "case {case}");
    }
}

/// How each answer to the original's one send settles the retained bind, at
/// the settlement itself, without the binder's stale-fence retry in between:
/// a proof that nothing was written releases it; anything else keeps it.
#[test]
fn the_originals_own_answer_alone_decides_its_settlement() {
    let (state, session, _producer) = retention_fixture("settlement", Vec::new(), Vec::new());
    let target = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
            .expect("the session has a binding target")
            .expect("the session is Engram-enabled")
    };
    let request = EngramControlRequest::SessionBind {
        external_ref: target.external_ref.clone(),
        title: target.title.clone(),
        assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
        mediated_effects: target.effects.clone(),
        capability_map_revision: ENGRAM_CAPABILITY_MAP_REVISION,
        work_binding: None,
        idempotency_key: "termal-bind:settlement-case".to_owned(),
    };
    let remote = |code: &str| {
        EngramTransportError::remote(EngramControlErrorBody {
            code: code.to_owned(),
            message: format!("Engram answered {code}"),
        })
    };
    let settles = ENGRAM_SESSION_BIND_NO_WRITE_REFUSALS
        .iter()
        .copied()
        .chain(["control_store_busy", "control_request_expired"])
        .map(remote)
        .chain([EngramTransportError::transport("gone").request_never_sent()]);
    for error in settles {
        let label = format!("{:?} {:?}", error.kind, error.code);
        state.retain_engram_bind(&target, &request).unwrap();
        state.settle_retained_engram_bind::<()>(&session, &request, &Err(error));
        assert_eq!(retained_key(&state, &session), None, "{label} settles it");
    }
    for error in [
        EngramTransportError::transport("the answer was lost"),
        EngramTransportError::deadline("no answer within the call bound"),
        EngramTransportError::protocol("an invalid answer"),
        remote("storage_error"),
        remote("a_code_from_a_later_engram"),
    ] {
        let label = format!("{:?} {:?}", error.kind, error.code);
        state.retain_engram_bind(&target, &request).unwrap();
        state.settle_retained_engram_bind::<()>(&session, &request, &Err(error));
        assert_eq!(
            retained_key(&state, &session).as_deref(),
            Some("termal-bind:settlement-case"),
            "{label} leaves it unresolved"
        );
    }
}

/// The recovered original's current status says the session is in a turn:
/// the original is settled, and the bind for the moved focus is not sent, so
/// nothing replaces the session's binding while its turn is open.
#[test]
fn a_recovered_session_in_a_turn_is_not_rebound() {
    let first = test_control_work_binding("retained-turn-first", 1);
    let moved = test_control_work_binding("retained-turn-moved", 2);
    let (state, session, producer) =
        retention_fixture("in-turn", vec![Some(first), Some(moved)], Vec::new());
    producer.script([BindAnswer::ApplyThenLose, BindAnswer::ApplyInTurn]);
    bind(&state, &session).expect_err("the answer was lost");
    let error = bind(&state, &session).expect_err("no rebind while in a turn");
    assert_eq!(error.kind, EngramTransportErrorKind::LocalState);
    let binds = producer.binds();
    assert_eq!(binds.len(), 2, "only the original's replay: {binds:#?}");
    assert_eq!(binds[1], binds[0]);
    assert_eq!(producer.writes(), 1);
    assert_eq!(retained_key(&state, &session), None, "settled by its valid answer");
}

/// A replay answered certified-not-applied, or proven never sent, settles
/// nothing about the earlier send: the original stays retained and is sent
/// again, unchanged.
#[test]
fn an_unapplied_replay_keeps_the_original() {
    for (case, answer) in [
        BindAnswer::Refuse("control_store_busy"),
        BindAnswer::Refuse("control_request_expired"),
        BindAnswer::Unsent,
    ]
    .into_iter()
    .enumerate()
    {
        let (state, session, producer) =
            retention_fixture(&format!("unapplied-replay-{case}"), Vec::new(), Vec::new());
        producer.script([BindAnswer::LoseUnapplied, answer]);
        bind(&state, &session).expect_err("the first call was lost");
        let original = producer.binds()[0].clone();
        bind(&state, &session).expect_err("the replay settled nothing");
        assert_eq!(
            retained_key(&state, &session).as_deref(),
            original["idempotency_key"].as_str(),
            "case {case}"
        );
        bind(&state, &session).expect("the third send is answered");
        let binds = producer.binds();
        assert_eq!(binds.len(), 3, "case {case}: {binds:#?}");
        assert!(binds.iter().all(|bind| *bind == original), "case {case}");
    }
}

/// Every no-write refusal of the replay gives the original up, unresolved
/// and told, and a new bind follows.
#[test]
fn every_no_write_refusal_of_a_replay_gives_the_original_up() {
    for (case, code) in ENGRAM_SESSION_BIND_NO_WRITE_REFUSALS.iter().copied().enumerate() {
        let (state, session, producer) =
            retention_fixture(&format!("refused-replay-{case}"), Vec::new(), Vec::new());
        producer.script([BindAnswer::LoseUnapplied, BindAnswer::Refuse(code)]);
        bind(&state, &session).expect_err("the first call was lost");
        let original = producer.binds()[0].clone();
        let key = original["idempotency_key"].as_str().unwrap().to_owned();
        bind(&state, &session).expect("a new bind after the refused replay");
        let binds = producer.binds();
        assert_eq!(binds.len(), 3, "{code}: {binds:#?}");
        assert_eq!(binds[1], original, "{code}");
        assert_ne!(binds[2]["idempotency_key"], original["idempotency_key"], "{code}");
        assert_eq!(retained_key(&state, &session), None, "{code}");
        let told = pending_line(&state, &session);
        assert!(told.contains(&key) && told.contains(code), "{code}: {told}");
    }
}

/// A refusal of the replay says nothing of the earlier send: the original
/// is given up unresolved and told as unknown, never relabelled unsent, and
/// a new bind follows.
#[test]
fn a_refused_replay_gives_the_original_up_unresolved_and_tells_it() {
    let (state, session, producer) = retention_fixture("refused-replay", Vec::new(), Vec::new());
    producer.script([BindAnswer::LoseUnapplied, BindAnswer::Refuse("stale_fence")]);
    bind(&state, &session).expect_err("the first call was lost");
    let original = producer.binds()[0].clone();
    let key = original["idempotency_key"].as_str().unwrap().to_owned();

    bind(&state, &session).expect("a new bind after the refused replay");
    let binds = producer.binds();
    assert_eq!(binds.len(), 3, "{binds:#?}");
    assert_eq!(binds[1], original, "the replay was the original, exactly");
    assert_ne!(binds[2]["idempotency_key"], original["idempotency_key"]);
    assert_eq!(retained_key(&state, &session), None);
    let told = pending_line(&state, &session);
    assert!(told.contains(&key), "{told}");
    assert!(told.contains("is unknown"), "{told}");
    assert!(told.contains("stale_fence"), "{told}");
    // Never again, under any later bind.
    unbind_locally(&state, &session);
    bind(&state, &session).expect("a later bind");
    assert_eq!(
        producer
            .binds()
            .iter()
            .filter(|bind| bind["idempotency_key"] == original["idempotency_key"])
            .count(),
        2
    );
}

/// Saving changed Engram settings through the production update resets the
/// session's Engram state: the retained original is dropped, unresolved, and
/// its holder is told; nothing sends it again.
#[test]
fn a_settings_save_drops_the_retained_original_and_tells_it() {
    let (state, session, producer) = retention_fixture("settings-save", Vec::new(), Vec::new());
    producer.script([BindAnswer::LoseUnapplied]);
    bind(&state, &session).expect_err("the first call was lost");
    let key = producer.binds()[0]["idempotency_key"].as_str().unwrap().to_owned();
    let (project, mut settings) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let settings = inner
            .find_project(&project)
            .and_then(|record| record.engram.clone())
            .unwrap();
        (project, settings)
    };
    settings.enabled = false;
    state
        .update_project_engram_settings(&project, settings)
        .expect("the settings save is accepted");
    assert_eq!(retained_key(&state, &session), None);
    let told = pending_line(&state, &session);
    assert!(told.contains(&key), "{told}");
    assert!(told.contains("is unknown"), "{told}");
    assert!(told.contains("settings changed"), "{told}");
    assert_eq!(producer.binds().len(), 1, "nothing is sent again");
}

/// Removing the project drops the retained original with the rest of the
/// session's Engram state; it is logged, and nothing sends it again.
#[test]
fn a_project_removal_drops_the_retained_original() {
    let (state, session, producer) = retention_fixture("project-removed", Vec::new(), Vec::new());
    producer.script([BindAnswer::LoseUnapplied]);
    bind(&state, &session).expect_err("the first call was lost");
    let project = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].session.project_id.clone().unwrap()
    };
    state.delete_project(&project).expect("the project is removed");
    assert_eq!(retained_key(&state, &session), None);
    assert_eq!(producer.binds().len(), 1, "nothing is sent again");
}

/// A changed connection or settings that reach the binder before any reset
/// (the eligibility check) also never replay the retained original: it is
/// given up, told, and the bind that replaces it never brings it back.
#[test]
fn a_settings_change_never_replays_the_retained_original() {
    let (state, session, producer) = retention_fixture("settings", Vec::new(), Vec::new());
    producer.script([BindAnswer::LoseUnapplied]);
    bind(&state, &session).expect_err("the first call was lost");
    let original = producer.binds()[0].clone();
    let key = original["idempotency_key"].as_str().unwrap().to_owned();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let settings = inner
            .projects
            .iter_mut()
            .find(|candidate| candidate.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap();
        settings.deadline_ms = Some(settings.deadline_ms.unwrap_or(30_000) + 1_000);
    }

    bind(&state, &session).expect("a new bind under the new settings");
    let binds = producer.binds();
    assert_eq!(binds.len(), 2, "{binds:#?}");
    assert_ne!(binds[1]["idempotency_key"], original["idempotency_key"]);
    assert_eq!(retained_key(&state, &session), None);
    let told = pending_line(&state, &session);
    assert!(told.contains(&key), "{told}");
    assert!(told.contains("settings changed"), "{told}");
    unbind_locally(&state, &session);
    bind(&state, &session).expect("a later bind");
    assert!(
        producer.binds()[1..]
            .iter()
            .all(|bind| bind["idempotency_key"] != original["idempotency_key"]),
        "the replaced original is never replayed as a read-back"
    );
}

/// A queued admission's bind competes for the session's row: while a
/// retained original is unresolved, the admission recovers it first, and its
/// own bind goes out only after the original is settled.
#[test]
fn a_queued_admission_recovers_the_retained_original_before_its_own_bind() {
    let (state, session, producer) =
        retention_fixture("queued", Vec::new(), vec![grant_reply("retained-queued-grant")]);
    producer.script([BindAnswer::ApplyThenLose]);
    bind(&state, &session).expect_err("the answer was lost");
    let original = producer.binds()[0].clone();
    queue_test_engram_prompt(&state, &session, "Root turn", QueuedPromptSource::User, None);
    let _admitted = state
        .start_next_queued_turn_off_lock(&session, false, false)
        .expect("queued admission should not fail locally");
    let binds = producer.binds();
    assert_eq!(binds.len(), 3, "{binds:#?}");
    assert_eq!(binds[1], original, "the original is recovered first, exactly");
    assert_ne!(binds[2]["idempotency_key"], original["idempotency_key"]);
    assert_eq!(producer.writes(), 2, "the original once, then the admission's own bind");
    assert_eq!(retained_key(&state, &session), None);
}

/// A queued admission recovers the original first; when that answer says the
/// session is in a turn, the admission's own bind is not sent.
#[test]
fn a_queued_admission_does_not_rebind_a_recovered_session_in_a_turn() {
    let (state, session, producer) = retention_fixture(
        "queued-in-turn",
        Vec::new(),
        vec![grant_reply("retained-queued-turn-grant")],
    );
    producer.script([BindAnswer::ApplyThenLose, BindAnswer::ApplyInTurn]);
    bind(&state, &session).expect_err("the answer was lost");
    let original = producer.binds()[0].clone();
    queue_test_engram_prompt(&state, &session, "Root turn", QueuedPromptSource::User, None);
    // The admission's outcome is its own business; only what it sent is
    // asserted here.
    let _ = state.start_next_queued_turn_off_lock(&session, false, false);
    let binds = producer.binds();
    assert_eq!(binds.len(), 2, "the replay only, no admission bind: {binds:#?}");
    assert_eq!(binds[1], original);
    assert_eq!(producer.writes(), 1);
    assert_eq!(retained_key(&state, &session), None, "settled by its valid answer");
}

/// The retention is process memory only: a stored and reloaded session has
/// none, so no restart-safe recovery is claimed.
#[test]
fn the_retained_original_is_never_stored() {
    let (state, session, producer) = retention_fixture("restart", Vec::new(), Vec::new());
    producer.script([BindAnswer::LoseUnapplied]);
    bind(&state, &session).expect_err("the first call was lost");
    assert!(retained_key(&state, &session).is_some());
    let path = state.persistence_path.as_path();
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        persist_state(path, &inner).expect("the state persists");
    }
    let loaded = load_state(path)
        .expect("the state loads")
        .expect("a state was stored");
    let index = loaded.find_session_index(&session).expect("the session loads");
    assert!(loaded.sessions[index].engram.retained_bind.is_none());
}
