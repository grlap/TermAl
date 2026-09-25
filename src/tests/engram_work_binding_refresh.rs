//! The admission-time refresh of a bound session's Engram work binding
//! (Engram w-108a13d58018 criterion 3, tm-winf step 3): a claim the agent
//! took, released or moved on during a turn reaches Engram at the next
//! admission through one rebind, an unchanged binding is never rebound, a
//! failed read keeps the current binding, and a retained authorization is
//! replayed without a read. Also the breaker that keeps a binding Engram
//! refused as stale from being sent again while the read returns it
//! unchanged.
//!
//! Owns the refresh and breaker tests. Does not own the reader's CLI protocol
//! (`real_process_work_binding_reader_*` in `engram_host_adapter.rs`) or the
//! stale-fence heal tests that predate the refresh (`a0_*`, same file). New
//! module beside the adapter tests, created instead of growing them.

use super::*;

/// A root session behind a scripted control plane that answers `responses`
/// and hands out `work_bindings` one read at a time.
struct BindingRoot {
    state: AppState,
    session: String,
    receiver: mpsc::Receiver<CodexRuntimeCommand>,
    transport: Arc<ScriptedEngramControlTransport>,
}

type ScriptedBindingRead =
    std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError>;

/// A root session with Engram enabled and no transport installed yet.
fn binding_root_state(label: &str) -> (AppState, String, mpsc::Receiver<CodexRuntimeCommand>) {
    let (state, receiver) =
        test_app_state_with_delegation_codex_runtime(&format!("engram-binding-refresh-{label}"));
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join(format!("engram-binding-refresh-{label}-project"));
    fs::create_dir_all(&root).expect("project root should exist");
    let project = create_test_project(&state, &root, "Engram binding refresh");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    enable_test_project_engram(&state, &project, &root);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session)
            .expect("root should exist");
        // These tests are about the binding, not the context reader.
        inner.sessions[index].engram.context_nudge_pending = false;
    }
    (state, session, receiver)
}

impl BindingRoot {
    fn new(
        label: &str,
        responses: impl IntoIterator<Item = ScriptedEngramControlResponse>,
        work_bindings: impl IntoIterator<Item = ScriptedBindingRead>,
    ) -> Self {
        let (state, session, receiver) = binding_root_state(label);
        let transport =
            ScriptedEngramControlTransport::new_with_work_bindings(responses, work_bindings);
        state.install_control_test_transport(transport.clone());
        Self {
            state,
            session,
            receiver,
            transport,
        }
    }

    fn dispatch(&self, queued: bool) -> TurnDispatch {
        if queued {
            queue_test_engram_prompt(
                &self.state,
                &self.session,
                "Root turn",
                QueuedPromptSource::User,
                None,
            );
            return self
                .state
                .start_next_queued_turn_off_lock(&self.session, false, false)
                .expect("queued root admission should not fail locally")
                .expect("queued root should reach admission")
                .dispatch;
        }
        match self
            .state
            .dispatch_turn(
                &self.session,
                SendMessageRequest {
                    text: "Root turn".to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .expect("root should reach admission")
        {
            DispatchTurnResult::Dispatched(dispatch)
            | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
            DispatchTurnResult::Queued => panic!("an idle root should dispatch"),
        }
    }

    /// Admits, delivers and completes one turn.
    fn complete_turn(&self, queued: bool) {
        deliver_turn_dispatch(&self.state, self.dispatch(queued))
            .expect("the begun turn should reach the runtime");
        assert!(matches!(
            self.receiver.try_recv().expect("one provider prompt"),
            CodexRuntimeCommand::Prompt { .. }
        ));
        assert!(
            self.receiver.try_recv().is_err(),
            "exactly one provider prompt"
        );
        let runtime_token = self.record(|record| {
            record
                .runtime
                .runtime_token()
                .expect("the begun turn should own the runtime")
        });
        self.state
            .finish_turn_ok_if_runtime_matches(&self.session, &runtime_token)
            .expect("the turn should complete");
    }

    fn operations(&self) -> Vec<String> {
        self.transport
            .requests()
            .iter()
            .map(|request| {
                request.request["operation"]
                    .as_str()
                    .expect("operation should be present")
                    .to_owned()
            })
            .collect()
    }

    fn record<T>(&self, read: impl FnOnce(&mut SessionRecord) -> T) -> T {
        let mut inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&self.session)
            .expect("root should exist");
        read(
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid"),
        )
    }
}

/// The replies for one turn that is granted, begun and checkpointed.
fn granted_turn(grant_id: &str) -> [ScriptedEngramControlResponse; 3] {
    [
        grant_reply(grant_id),
        begin_reply(grant_id),
        checkpoint_reply(grant_id),
    ]
}

/// The replies for a rebind the refresh arms: status, then a fresh bind.
fn rebound(token: &str) -> [ScriptedEngramControlResponse; 2] {
    [status_reply("ready"), rebind_reply(token)]
}

/// The replies of consecutive exchanges, in order.
fn script(
    groups: impl IntoIterator<Item = Vec<ScriptedEngramControlResponse>>,
) -> Vec<ScriptedEngramControlResponse> {
    groups.into_iter().flatten().collect()
}

#[test]
fn a_claim_taken_during_a_turn_is_bound_at_the_next_admission() {
    // The agent claims work in its first turn, after TermAl bound the session
    // without work. Engram counts an unbound session as always current, so
    // only the refresh can carry the claim to Engram: the next admission
    // reads it and rebinds once, and that turn is the first to report.
    for queued in [false, true] {
        let claim = test_control_work_binding("taken", 1);
        let root = BindingRoot::new(
            "taken",
            script([
                vec![bind_reply("unbound-token")],
                granted_turn("claiming-grant").to_vec(),
                rebound("claimed-token").to_vec(),
                granted_turn("claimed-grant").to_vec(),
            ]),
            [Ok(None), Ok(Some(claim.clone()))],
        );
        root.complete_turn(queued);
        root.complete_turn(queued);

        assert_eq!(
            root.operations(),
            [
                "session_bind",
                "turn_evaluate",
                "turn_begin",
                "turn_checkpoint",
                "session_status",
                "session_bind",
                "turn_evaluate",
                "turn_begin",
                "turn_checkpoint"
            ],
            "queued={queued}"
        );
        let requests = root.transport.requests();
        assert!(requests[0].request.get("work_binding").is_none());
        assert_eq!(requests[5].request["work_binding"], json!(claim));
        assert_ne!(
            requests[0].request["idempotency_key"], requests[5].request["idempotency_key"],
            "the rebind is a new bind, not a replay (queued={queued})"
        );
        assert_eq!(requests[6].request["routing_token"], "claimed-token");
        assert_eq!(
            root.transport.unread_work_bindings(),
            0,
            "the rebind binds what the refresh read, without reading again"
        );
        // The claiming turn ran unbound and reports nothing; the next one
        // runs under the claim and reports its observation.
        assert!(requests[3].request.get("observations").is_none());
        assert_eq!(
            requests[8].request["observations"].as_array().map(Vec::len),
            Some(1),
            "queued={queued}"
        );
        root.record(|record| {
            assert_eq!(record.engram.work_binding, Some(claim.clone()));
            assert_eq!(record.engram.work_binding_refresh_rebinds, 1);
        });
    }
}

#[test]
fn a_binding_that_has_not_changed_is_never_rebound() {
    let claim = test_control_work_binding("kept", 3);
    let mut script = vec![bind_reply("kept-token")];
    for turn in 1..=4 {
        script.extend(granted_turn(&format!("kept-grant-{turn}")));
    }
    let root = BindingRoot::new("kept", script, (0..4).map(|_| Ok(Some(claim.clone()))));
    for _ in 0..4 {
        root.complete_turn(false);
    }

    let operations = root.operations();
    assert_eq!(
        operations.iter().filter(|op| *op == "session_bind").count(),
        1,
        "{operations:?}"
    );
    assert!(!operations.iter().any(|op| op == "session_status"));
    assert_eq!(
        root.transport.unread_work_bindings(),
        0,
        "the bind read once and each later admission read again"
    );
    root.record(|record| {
        assert_eq!(record.engram.work_binding, Some(claim.clone()));
        assert_eq!(record.engram.work_binding_refresh_rebinds, 0);
    });
}

#[test]
fn a_released_claim_rebinds_without_work() {
    let claim = test_control_work_binding("released", 2);
    let root = BindingRoot::new(
        "released",
        script([
            vec![bind_reply("claimed-token")],
            granted_turn("releasing-grant").to_vec(),
            rebound("released-token").to_vec(),
            granted_turn("released-grant").to_vec(),
        ]),
        [Ok(Some(claim.clone())), Ok(None)],
    );
    root.complete_turn(false);
    root.complete_turn(false);

    let requests = root.transport.requests();
    assert_eq!(requests[0].request["work_binding"], json!(claim));
    assert_eq!(requests[5].request["operation"], "session_bind");
    assert!(
        requests[5].request.get("work_binding").is_none(),
        "a released claim binds the session without work"
    );
    assert!(requests[8].request.get("observations").is_none());
    root.record(|record| assert_eq!(record.engram.work_binding, None));
}

#[test]
fn a_claim_that_moved_on_rebinds_with_its_new_identity() {
    let claim = test_control_work_binding("moved", 1);
    for (case, moved) in [
        ("revision", test_control_work_binding("moved", 2)),
        (
            "fence",
            EngramControlWorkBinding {
                claim_fence: claim.claim_fence + 1,
                ..claim.clone()
            },
        ),
        ("item", test_control_work_binding("moved-elsewhere", 1)),
    ] {
        let root = BindingRoot::new(
            "moved",
            script([
                vec![bind_reply("first-token")],
                granted_turn("first-grant").to_vec(),
                rebound("moved-token").to_vec(),
                granted_turn("moved-grant").to_vec(),
            ]),
            [Ok(Some(claim.clone())), Ok(Some(moved.clone()))],
        );
        root.complete_turn(false);
        root.complete_turn(false);

        let requests = root.transport.requests();
        assert_eq!(requests[5].request["operation"], "session_bind", "{case}");
        assert_eq!(requests[5].request["work_binding"], json!(moved), "{case}");
        root.record(|record| assert_eq!(record.engram.work_binding, Some(moved.clone())));
    }
}

#[test]
fn a_failed_refresh_keeps_the_binding_and_the_next_admission_reads_again() {
    // A read that fails or times out is not "no claim": the turn goes ahead
    // on its current binding. The refusal below means that turn never
    // began, so only the failed read can have left the next admission a
    // reason to read.
    for error in [
        EngramTransportError::transport("database is locked"),
        EngramTransportError::deadline("work-binding read timed out"),
    ] {
        let claim = test_control_work_binding("failed-read", 1);
        let moved = test_control_work_binding("failed-read", 2);
        let root = BindingRoot::new(
            "failed-read",
            script([
                vec![bind_reply("first-token")],
                granted_turn("first-grant").to_vec(),
                vec![evaluation_refusal_reply("policy_denied")],
                rebound("moved-token").to_vec(),
                granted_turn("moved-grant").to_vec(),
            ]),
            [
                Ok(Some(claim.clone())),
                Err(error.clone()),
                Ok(Some(moved.clone())),
            ],
        );
        root.complete_turn(false);
        let refused = deliver_turn_dispatch(&root.state, root.dispatch(false));
        assert!(refused.is_err(), "the policy refusal withholds the turn");
        assert!(root.receiver.try_recv().is_err());
        root.record(|record| {
            assert_eq!(
                record.engram.work_binding,
                Some(claim.clone()),
                "a failed read keeps the binding ({error:?})"
            );
            assert!(record.engram.turn_begun_since_binding_read);
        });
        root.complete_turn(false);

        let operations = root.operations();
        assert_eq!(
            operations,
            [
                "session_bind",
                "turn_evaluate",
                "turn_begin",
                "turn_checkpoint",
                "turn_evaluate",
                "session_status",
                "session_bind",
                "turn_evaluate",
                "turn_begin",
                "turn_checkpoint"
            ],
            "{error:?}"
        );
        let requests = root.transport.requests();
        assert_eq!(requests[4].request["routing_token"], "first-token");
        assert_eq!(requests[6].request["work_binding"], json!(moved));
    }
}

#[test]
fn a_failed_refresh_rebind_withholds_the_turn_until_its_exact_replay_binds() {
    // The refresh read a claim that moved on, and the rebind it armed
    // failed: the turn is withheld with its prompt retained, not evaluated
    // under the routing token of the binding the read found outdated, and the
    // bound binding stays. Resuming it replays that rebind exactly, without
    // reading again, and only then evaluates, under the new binding.
    let claim = test_control_work_binding("failed-rebind", 1);
    let moved = test_control_work_binding("failed-rebind", 2);
    let root = BindingRoot::new(
        "failed-rebind",
        script([
            vec![bind_reply("first-token")],
            granted_turn("first-grant").to_vec(),
            vec![
                status_reply("ready"),
                ScriptedEngramControlResponse::Reply(Err(EngramTransportError::transport(
                    "database is locked",
                ))),
            ],
            // The replay resends the retained bind alone: the status that
            // preceded it was already asked.
            vec![rebind_reply("moved-token")],
            granted_turn("moved-grant").to_vec(),
        ]),
        [
            Ok(Some(claim.clone())),
            Ok(Some(moved.clone())),
            Ok(Some(test_control_work_binding("failed-rebind-unread", 1))),
        ],
    );
    root.complete_turn(false);
    let withheld = deliver_turn_dispatch(&root.state, root.dispatch(false));
    assert!(
        matches!(withheld, TurnDispatchDeliveryOutcome::Held { error: None }),
        "the failed rebind holds the turn: {withheld:?}"
    );
    assert!(root.receiver.try_recv().is_err());
    assert_eq!(
        root.operations()[4..],
        ["session_status", "session_bind"],
        "nothing is evaluated after the failed rebind"
    );
    root.record(|record| {
        let card = record
            .session
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::EngramControl { card, .. }
                    if card.stage == EngramControlStage::Dispatch =>
                {
                    Some(card)
                }
                _ => None,
            })
            .expect("the withheld turn should leave a dispatch card");
        assert_eq!(card.dispatch, EngramControlCardDispatch::Withheld);
        assert_eq!(record.queued_prompts.len(), 1, "the prompt is retained");
        assert_eq!(record.engram.work_binding, Some(claim.clone()));
        assert!(record.engram.turn_begun_since_binding_read);
        // The backoff the failure armed elapses, as time would.
        record.engram.next_bind_retry_at = None;
    });

    root.state
        .resume_session_queue(&root.session)
        .expect("the retained rebind should replay");
    assert!(matches!(
        root.receiver.try_recv().expect("one provider prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let runtime_token = root.record(|record| {
        record
            .runtime
            .runtime_token()
            .expect("the begun turn should own the runtime")
    });
    root.state
        .finish_turn_ok_if_runtime_matches(&root.session, &runtime_token)
        .expect("the turn should complete");

    assert_eq!(
        root.operations(),
        [
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "turn_checkpoint",
            "session_status",
            "session_bind",
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "turn_checkpoint"
        ]
    );
    let requests = root.transport.requests();
    assert_eq!(requests[5].request["work_binding"], json!(moved));
    assert_eq!(requests[5].request, requests[6].request, "exact replay");
    assert_eq!(
        root.transport.unread_work_bindings(),
        1,
        "the replay reads no binding"
    );
    assert_eq!(requests[7].request["routing_token"], "moved-token");
    root.record(|record| assert_eq!(record.engram.work_binding, Some(moved.clone())));
}

#[test]
fn an_admission_after_a_turn_that_never_began_does_not_read_again() {
    // Only the agent takes its session's claim, and only during a turn: a
    // turn Engram refused never ran, so the binding read at the bind stands.
    let claim = test_control_work_binding("not-begun", 1);
    let root = BindingRoot::new(
        "not-begun",
        script([
            vec![bind_reply("first-token")],
            vec![evaluation_refusal_reply("policy_denied")],
            granted_turn("second-grant").to_vec(),
        ]),
        [
            Ok(Some(claim.clone())),
            Ok(Some(test_control_work_binding("not-begun-unread", 1))),
        ],
    );
    assert!(deliver_turn_dispatch(&root.state, root.dispatch(false)).is_err());
    root.complete_turn(false);

    assert_eq!(
        root.operations(),
        [
            "session_bind",
            "turn_evaluate",
            "turn_evaluate",
            "turn_begin",
            "turn_checkpoint"
        ]
    );
    assert_eq!(root.transport.unread_work_bindings(), 1);
}

#[test]
fn a_retained_evaluation_is_replayed_without_reading_the_binding() {
    // The retained evaluate was issued under the binding the session had; its
    // exact replay must not be reattributed to whatever the claim is now.
    let claim = test_control_work_binding("retained", 1);
    let root = BindingRoot::new(
        "retained",
        script([
            vec![bind_reply("first-token")],
            granted_turn("first-grant").to_vec(),
            vec![ScriptedEngramControlResponse::Reply(Err(
                EngramTransportError::deadline("reply lost after evaluate"),
            ))],
            vec![grant_reply("replayed-grant"), begin_reply("replayed-grant")],
        ]),
        [
            Ok(Some(claim.clone())),
            // The second admission's refresh fails, so a turn still counts
            // as begun since the last read when the replay is admitted.
            Err(EngramTransportError::transport("database is locked")),
            Ok(Some(test_control_work_binding("retained-moved", 1))),
        ],
    );
    root.complete_turn(false);
    deliver_turn_dispatch(&root.state, root.dispatch(false))
        .expect("an unknown evaluation leaves the prompt waiting");
    assert!(root.receiver.try_recv().is_err());
    root.state
        .resume_session_queue(&root.session)
        .expect("the exact replay should succeed");
    assert!(matches!(
        root.receiver.try_recv().expect("one provider prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));

    assert_eq!(
        root.operations(),
        [
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "turn_checkpoint",
            "turn_evaluate",
            "turn_evaluate",
            "turn_begin"
        ]
    );
    let requests = root.transport.requests();
    assert_eq!(requests[4].request, requests[5].request, "exact replay");
    assert_eq!(
        root.transport.unread_work_bindings(),
        1,
        "the replay reads no binding"
    );
    root.record(|record| assert_eq!(record.engram.work_binding, Some(claim.clone())));
}

/// Forwards to a scripted transport and runs a hook right after the
/// `hook_on_read`-th work-binding read, as another actor would act while the
/// read was in flight.
struct ReadHookTransport {
    scripted: Arc<ScriptedEngramControlTransport>,
    reads: std::sync::atomic::AtomicUsize,
    hook_on_read: usize,
    hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl EngramControlTransport for ReadHookTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        self.scripted.request(connection, request, timeout)
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        let result = self
            .scripted
            .read_work_binding(connection, preference, timeout);
        let read = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if read == self.hook_on_read
            && let Some(hook) = self.hook.lock().expect("hook mutex poisoned").take()
        {
            hook();
        }
        result
    }

    fn shutdown_session(&self, session_id: &str) {
        self.scripted.shutdown_session(session_id);
    }
}

#[test]
fn an_admission_superseded_during_the_refresh_neither_rebinds_nor_evaluates() {
    let claim = test_control_work_binding("superseded", 1);
    let (state, session, receiver) = binding_root_state("superseded");
    let scripted = ScriptedEngramControlTransport::new_with_work_bindings(
        script([
            vec![bind_reply("first-token")],
            granted_turn("first-grant").to_vec(),
        ]),
        [
            Ok(Some(claim.clone())),
            Ok(Some(test_control_work_binding("superseded-moved", 1))),
        ],
    );
    let shared = Arc::downgrade(&state.inner);
    let superseded_session = session.clone();
    state.install_control_test_transport(Arc::new(ReadHookTransport {
        scripted: scripted.clone(),
        reads: std::sync::atomic::AtomicUsize::new(0),
        hook_on_read: 2,
        hook: Mutex::new(Some(Box::new(move || {
            // A Stop or cancel moves the dispatch generation on, so the
            // admission that started the read no longer owns the prompt.
            let shared = shared.upgrade().expect("state should be alive");
            let mut inner = shared.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&superseded_session)
                .expect("root should exist");
            inner.sessions[index].engram.dispatch_generation += 1;
        }))),
    }));
    let root = BindingRoot {
        state,
        session,
        receiver,
        transport: scripted,
    };
    root.complete_turn(false);
    let second = root
        .state
        .dispatch_turn(
            &root.session,
            SendMessageRequest {
                text: "Root turn".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("the second turn should reach admission");
    assert!(
        matches!(second, DispatchTurnResult::Queued),
        "the superseded admission leaves the prompt queued for its new owner"
    );

    assert!(
        root.receiver.try_recv().is_err(),
        "nothing reaches the runtime"
    );
    assert_eq!(
        root.operations(),
        [
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "turn_checkpoint"
        ],
        "the changed claim read by a superseded admission is not acted on"
    );
    root.record(|record| assert_eq!(record.engram.work_binding, Some(claim.clone())));
}

#[test]
fn a_binding_engram_refused_as_stale_is_not_sent_again_while_it_reads_unchanged() {
    // Engram validates a binding against more than the read checks, so the
    // read can offer a binding every evaluate refuses as stale. The heal
    // binds without work instead of resending it, the turn goes ahead, and
    // later admissions leave the session unbound while the read is the same.
    let claim = test_control_work_binding("refused", 1);
    let root = BindingRoot::new(
        "refused",
        script([
            vec![bind_reply("claimed-token")],
            granted_turn("first-grant").to_vec(),
            vec![evaluation_refusal_reply("stale_fence")],
            rebound("unbound-token").to_vec(),
            granted_turn("unbound-grant").to_vec(),
            granted_turn("still-unbound-grant").to_vec(),
        ]),
        (0..4).map(|_| Ok(Some(claim.clone()))),
    );
    root.complete_turn(false);
    root.complete_turn(false);
    root.complete_turn(false);

    assert_eq!(
        root.operations(),
        [
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "turn_checkpoint",
            "turn_evaluate",
            "session_status",
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "turn_checkpoint",
            "turn_evaluate",
            "turn_begin",
            "turn_checkpoint"
        ]
    );
    let requests = root.transport.requests();
    assert!(
        requests[6].request.get("work_binding").is_none(),
        "the heal does not resend the refused binding"
    );
    assert_eq!(requests[10].request["routing_token"], "unbound-token");
    assert_eq!(root.transport.unread_work_bindings(), 0);
    assert_eq!(
        root.transport.refused_at_reads(),
        [
            Vec::new(),
            Vec::new(),
            vec![claim.clone()],
            vec![claim.clone()]
        ],
        "every read after the refusal is told to leave the refused binding out, so it can \
         fall back to another claim the session holds"
    );
    root.record(|record| {
        assert_eq!(record.engram.work_binding, None);
        assert_eq!(
            record
                .engram
                .refused_work_bindings
                .iter()
                .map(|(binding, _)| binding)
                .collect::<Vec<_>>(),
            [&claim]
        );
    });
}

#[test]
fn a_bind_refused_as_stale_retries_without_work_when_the_read_is_unchanged() {
    let claim = test_control_work_binding("bind-refused", 1);
    let root = BindingRoot::new(
        "bind-refused",
        script([
            vec![
                remote_error_reply("stale_fence"),
                bind_reply("unbound-token"),
            ],
            granted_turn("unbound-grant").to_vec(),
        ]),
        [Ok(Some(claim.clone())), Ok(Some(claim.clone()))],
    );
    root.complete_turn(false);

    let requests = root.transport.requests();
    assert_eq!(requests[0].request["work_binding"], json!(claim));
    assert_eq!(requests[1].request["operation"], "session_bind");
    assert!(requests[1].request.get("work_binding").is_none());
    assert_eq!(requests[2].request["routing_token"], "unbound-token");
}

#[test]
fn a_begin_refused_as_stale_blames_the_binding_only_when_the_grant_was_issued_under_it() {
    // Engram's begin refuses a grant issued under a binding the session no
    // longer holds as `stale_fence`. That is the grant racing a rebind: the
    // breaker must not withhold the binding the session holds now.
    for raced in [false, true] {
        let issued = test_control_work_binding("begin-race", 1);
        let current = if raced {
            test_control_work_binding("begin-race", 2)
        } else {
            issued.clone()
        };
        let root = BindingRoot::new(
            "begin-race",
            script([
                vec![
                    bind_reply("issued-token"),
                    grant_reply("raced-grant"),
                    checkpoint_refusal_reply("stale_fence"),
                ],
                rebound("fresh-token").to_vec(),
                vec![grant_reply("fresh-grant"), begin_reply("fresh-grant")],
            ]),
            [Ok(Some(issued.clone())), Ok(Some(current.clone()))],
        );
        let dispatch = root.dispatch(false);
        // The session is rebound after the grant was issued, before its begin.
        root.record(|record| record.engram.work_binding = Some(current.clone()));
        deliver_turn_dispatch(&root.state, dispatch)
            .expect("the re-evaluated turn should reach the runtime");

        let refused = root.record(|record| {
            record
                .engram
                .refused_work_bindings
                .iter()
                .map(|(binding, _)| binding.clone())
                .collect::<Vec<_>>()
        });
        if raced {
            assert_eq!(
                refused,
                Vec::new(),
                "a grant that raced a rebind blames no binding"
            );
        } else {
            assert_eq!(refused, [issued], "the binding the grant was issued under");
        }
        let begins = root
            .transport
            .requests()
            .into_iter()
            .filter(|request| request.request["operation"] == "turn_begin")
            .map(|request| request.request["grant_id"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            begins,
            [json!("raced-grant"), json!("fresh-grant")],
            "raced={raced}"
        );
    }
}

#[test]
fn stale_refusals_during_begin_recovery_blame_the_replacement_binding() {
    // Begin refuses the grant as stale; recovery rebinds to another claim and
    // evaluates again, and Engram refuses that binding as stale too, at the
    // re-evaluation or at the second begin. Both stay out of the reads for
    // their windows, so the next admission sends neither.
    for at_second_begin in [false, true] {
        let first = test_control_work_binding("recovery-first", 1);
        let second = test_control_work_binding("recovery-second", 1);
        let recovery = if at_second_begin {
            vec![
                grant_reply("second-grant"),
                checkpoint_refusal_reply("stale_fence"),
            ]
        } else {
            vec![evaluation_refusal_reply("stale_fence")]
        };
        let root = BindingRoot::new(
            "recovery-refusals",
            script([
                vec![
                    bind_reply("first-token"),
                    grant_reply("first-grant"),
                    checkpoint_refusal_reply("stale_fence"),
                ],
                rebound("second-token").to_vec(),
                recovery,
            ]),
            [Ok(Some(first.clone())), Ok(Some(second.clone()))],
        );
        let _ = deliver_turn_dispatch(&root.state, root.dispatch(false));
        assert!(
            root.receiver.try_recv().is_err(),
            "nothing reaches the runtime (at_second_begin={at_second_begin})"
        );
        assert_eq!(root.transport.unread_work_bindings(), 0);
        root.record(|record| {
            assert_eq!(record.engram.work_binding, Some(second.clone()));
            assert_eq!(
                record
                    .engram
                    .refused_work_bindings
                    .iter()
                    .map(|(binding, _)| binding.clone())
                    .collect::<Vec<_>>(),
                [first.clone(), second.clone()],
                "at_second_begin={at_second_begin}: {:?}",
                root.operations()
            );
        });
    }
}

/// Serves requests from a scripted control plane and runs a hook right before
/// the first request of `operation` is answered, as another actor would act
/// while it was in flight.
struct RequestHookTransport {
    scripted: Arc<ScriptedEngramControlTransport>,
    operation: &'static str,
    hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl EngramControlTransport for RequestHookTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        let operation =
            serde_json::to_value(request).expect("request should serialize")["operation"]
                .as_str()
                .map(str::to_owned);
        if operation.as_deref() == Some(self.operation)
            && let Some(hook) = self.hook.lock().expect("hook mutex poisoned").take()
        {
            hook();
        }
        self.scripted.request(connection, request, timeout)
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.scripted
            .read_work_binding(connection, preference, timeout)
    }

    fn shutdown_session(&self, session_id: &str) {
        self.scripted.shutdown_session(session_id);
    }
}

#[test]
fn an_evaluate_refused_as_stale_blames_the_binding_it_was_sent_under() {
    // The session is rebound while its evaluate is in flight, and Engram
    // refuses the evaluate as stale: the refusal concerns the binding the
    // evaluate went out under, which the session no longer holds, so neither
    // binding is withheld. Blaming the new binding would unbind a valid claim
    // for five minutes.
    let sent_under = test_control_work_binding("evaluate-race", 1);
    let rebound_to = test_control_work_binding("evaluate-race", 2);
    let (state, session, receiver) = binding_root_state("evaluate-race");
    let scripted = ScriptedEngramControlTransport::new_with_work_bindings(
        script([
            vec![
                bind_reply("first-token"),
                evaluation_refusal_reply("stale_fence"),
            ],
            rebound("fresh-token").to_vec(),
            vec![grant_reply("fresh-grant"), begin_reply("fresh-grant")],
        ]),
        [Ok(Some(sent_under.clone())), Ok(Some(rebound_to.clone()))],
    );
    let shared = Arc::downgrade(&state.inner);
    let raced_session = session.clone();
    let raced_binding = rebound_to.clone();
    state.install_control_test_transport(Arc::new(RequestHookTransport {
        scripted: scripted.clone(),
        operation: "turn_evaluate",
        hook: Mutex::new(Some(Box::new(move || {
            let shared = shared.upgrade().expect("state should be alive");
            let mut inner = shared.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&raced_session)
                .expect("root should exist");
            inner.sessions[index].engram.work_binding = Some(raced_binding);
        }))),
    }));
    let root = BindingRoot {
        state,
        session,
        receiver,
        transport: scripted,
    };
    deliver_turn_dispatch(&root.state, root.dispatch(false))
        .expect("the healed turn should reach the runtime");

    root.record(|record| {
        assert!(
            record.engram.refused_work_bindings.is_empty(),
            "a refusal that raced a rebind blames no binding: {:?}",
            record.engram.refused_work_bindings
        );
    });
}

#[test]
fn the_refused_binding_breaker_withholds_each_refused_binding_within_its_own_window() {
    let refused = test_control_work_binding("breaker", 1);
    let also_refused = test_control_work_binding("breaker-also", 1);
    let other = test_control_work_binding("breaker", 2);
    let now = std::time::Instant::now();
    let refused_at =
        |binding: &EngramControlWorkBinding, age: Duration| (binding.clone(), now - age);

    let mut state = vec![
        refused_at(&refused, Duration::from_secs(1)),
        refused_at(&also_refused, Duration::from_secs(2)),
    ];
    for binding in [&refused, &also_refused] {
        assert_eq!(
            engram_binding_after_refusal(Some(binding.clone()), &mut state, now),
            (None, true),
            "each refused binding is withheld"
        );
    }
    assert_eq!(state.len(), 2, "and both refusals stand");
    assert_eq!(
        engram_refused_work_bindings_in_window(&state, now),
        [refused.clone(), also_refused.clone()],
        "reads leave both out"
    );

    // Each window ends on its own: the older refusal lapses first.
    let mut state = vec![
        refused_at(&refused, ENGRAM_REFUSED_WORK_BINDING_RETRY_AFTER),
        refused_at(&also_refused, Duration::from_secs(1)),
    ];
    assert_eq!(
        engram_binding_after_refusal(Some(refused.clone()), &mut state, now),
        (Some(refused.clone()), false),
        "after its window a binding is tried again"
    );
    assert_eq!(
        state
            .iter()
            .map(|(binding, _)| binding.clone())
            .collect::<Vec<_>>(),
        [also_refused.clone()],
        "while the other refusal stands"
    );

    for read in [Some(other.clone()), None] {
        let mut state = vec![refused_at(&refused, Duration::from_secs(1))];
        assert_eq!(
            engram_binding_after_refusal(read.clone(), &mut state, now),
            (read, false),
            "any other read passes, a new revision of a refused claim included"
        );
        assert_eq!(
            state.len(),
            1,
            "and leaves the refusal standing: the read differs because it left the refused \
             binding out"
        );
    }

    let mut state = Vec::new();
    assert_eq!(
        engram_binding_after_refusal(Some(other.clone()), &mut state, now),
        (Some(other), false)
    );
}

/// Serves requests from a scripted control plane, and reads the work
/// binding the way the CLI reader does: by selection over a fixed set of held
/// claims (`select_engram_held_binding`), with the preference the caller
/// passes.
struct HeldClaimsTransport {
    scripted: Arc<ScriptedEngramControlTransport>,
    /// Each held claim's binding and whether it is focused, newest first.
    held: Vec<(EngramControlWorkBinding, bool)>,
}

impl EngramControlTransport for HeldClaimsTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        self.scripted.request(connection, request, timeout)
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        // Recorded like any scripted read, then selected like the CLI's.
        let _ = self
            .scripted
            .read_work_binding(connection, preference, timeout);
        let held = self
            .held
            .iter()
            .map(|(binding, focused)| EngramHeldClaim {
                work_id: binding.work_id.clone(),
                focused: *focused,
                control_binding: Some(binding.clone()),
            })
            .collect();
        Ok(select_engram_held_binding(held, 0, preference))
    }

    fn shutdown_session(&self, session_id: &str) {
        self.scripted.shutdown_session(session_id);
    }
}

#[test]
fn two_held_claims_engram_refuses_do_not_take_turns_withholding_the_session() {
    // The session holds two claims Engram comes to refuse as stale (their
    // common ancestor, say): the first refusal heals onto the second, which
    // is refused too, and the turn is refused. The next admission evaluates
    // under the second, which Engram refuses again; its heal must leave both
    // out and bind without work, not go back to the first and be refused
    // again, admission after admission.
    let focused = test_control_work_binding("refused-both-focused", 1);
    let other = test_control_work_binding("refused-both-other", 1);
    let (state, session, receiver) = binding_root_state("refused-both");
    let scripted = ScriptedEngramControlTransport::new(script([
        vec![bind_reply("focused-token")],
        granted_turn("focused-grant").to_vec(),
        vec![evaluation_refusal_reply("stale_fence")],
        rebound("other-token").to_vec(),
        vec![evaluation_refusal_reply("stale_fence")],
        // The next admission: the second claim is refused again, and the
        // heal binds without work.
        vec![evaluation_refusal_reply("stale_fence")],
        rebound("unbound-token").to_vec(),
        granted_turn("unbound-grant").to_vec(),
    ]));
    state.install_control_test_transport(Arc::new(HeldClaimsTransport {
        scripted: scripted.clone(),
        held: vec![(focused.clone(), true), (other.clone(), false)],
    }));
    let root = BindingRoot {
        state,
        session,
        receiver,
        transport: scripted,
    };

    root.complete_turn(false);
    let refused = deliver_turn_dispatch(&root.state, root.dispatch(false));
    assert!(
        refused.is_err(),
        "both refused within one admission refuses the turn"
    );
    assert!(root.receiver.try_recv().is_err());
    root.complete_turn(false);

    let requests = root.transport.requests();
    let binds = requests
        .iter()
        .filter(|request| request.request["operation"] == "session_bind")
        .map(|request| request.request.get("work_binding").cloned())
        .collect::<Vec<_>>();
    assert_eq!(
        binds,
        [Some(json!(focused)), Some(json!(other)), None],
        "the focused claim, then the other, then no work: {:?}",
        root.operations()
    );
    assert_eq!(
        root.transport.refused_at_reads().last(),
        Some(&vec![focused.clone(), other.clone()]),
        "the last read left both refused claims out"
    );
    root.record(|record| {
        assert_eq!(record.engram.work_binding, None);
        assert_eq!(record.engram.refused_work_bindings.len(), 2);
    });
}
