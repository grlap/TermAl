// Owns the stall shape of a project settings change on a gated project: while
// the project's reset fence is held, an idle, unpaused session with a queued
// mailbox wake says why it is waiting, and the reason goes when the fence is
// released. A paused or busy session keeps its own preview. Does not own the
// reset transaction itself (src/tests/engram_host_adapter.rs), the order the
// release drains sessions in, or the teardown of control processes. New
// module, a child of src/tests/engram_root_dispatch.rs, whose root fixture it
// uses.

use super::*;

const PROJECT_RESET_WAIT_PREFIX: &str = "Waiting: Engram project settings are changing";

/// Whether `preview` names the fence's claim time as `since HH:MM:SSZ`.
fn names_since_time(preview: &str) -> bool {
    preview.split("since ").nth(1).is_some_and(|rest| {
        let time = rest.as_bytes();
        time.len() >= 9
            && [0, 1, 3, 4, 6, 7]
                .iter()
                .all(|&at| time[at].is_ascii_digit())
            && time[2] == b':'
            && time[5] == b':'
            && time[8] == b'Z'
    })
}

#[test]
fn a_held_project_settings_change_tells_an_idle_session_with_a_queued_wake_why_it_waits() {
    let (mut state, session, receiver, _transport) = root_fixture([
        bind_reply("reset-bind"),
        grant_reply("released-grant"),
        begin_reply("released-grant"),
    ]);
    let (project, mut settings, root) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let project_record = inner.projects.iter_mut().find(|p| p.id == project).unwrap();
        let root = PathBuf::from(&project_record.root_path);
        fs::write(root.join(".engram-project"), "fixture-ok\n").unwrap();
        let settings = project_record.engram.as_mut().unwrap();
        settings.binary_path = Some(
            real_engram_control_fixture_path()
                .to_string_lossy()
                .into_owned(),
        );
        (project, settings.clone(), root)
    };
    state.mailbox_store = Arc::new(
        MailboxStore::open(&resolve_coordination_persistence_path(
            state.persistence_path.as_ref(),
        ))
        .unwrap(),
    );
    let sender = test_session_id(&state, Agent::Claude);
    // A new home is a reset-required change: the settings transaction holds
    // the project's reset fence while it runs.
    let new_home = root.join("reset-wait-home");
    fs::create_dir_all(&new_home).unwrap();
    settings.home = Some(new_home.to_string_lossy().into_owned());
    let reset_gate = gate_next_engram_project_reset_fence(&project);
    std::thread::scope(|scope| {
        let reset = scope.spawn(|| {
            let result = state.update_project_engram_settings(&project, settings);
            abort_engram_project_reset_fence_gate(&project, &fence_gate_detail(&result));
            result
        });
        let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reset_gate.wait_until_entered();
            // A mailbox wake arrives while the settings change holds the fence.
            state
                .append_mailbox_message_and_notify(
                    &sender,
                    SendMailboxMessageRequest {
                        target_session_id: session.clone(),
                        message: "a wake during the settings change".to_owned(),
                        idempotency_key: "reset-wait-wake".to_owned(),
                        topic: Some("reset wait".to_owned()),
                        state_stamp: None,
                        class: Some("routine".to_owned()),
                    },
                )
                .unwrap();
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            (
                record.session.status,
                record.orchestrator_auto_dispatch_blocked,
                record
                    .queued_prompts
                    .iter()
                    .map(|queued| queued.source)
                    .collect::<Vec<_>>(),
                record.session.preview.clone(),
            )
        }));
        let public_preview = state
            .summary_snapshot()
            .sessions
            .into_iter()
            .find(|record| record.id == session)
            .map(|record| record.preview);
        let no_provider_while_fenced = receiver.try_recv().is_err();
        reset_gate.release();
        let reset_result = reset.join();
        let (status, paused, queued, preview) =
            observed.unwrap_or_else(|error| std::panic::resume_unwind(error));
        reset_result
            .expect("reset worker should settle")
            .expect("the settings change should finish");
        // The stall shape: idle, unpaused, one queued mailbox wake, nothing
        // sent to the provider.
        assert_eq!(status, SessionStatus::Idle);
        assert!(!paused, "the session is not paused");
        assert_eq!(queued, vec![QueuedPromptSource::Mailbox]);
        assert!(no_provider_while_fenced);
        assert!(
            preview.starts_with(PROJECT_RESET_WAIT_PREFIX),
            "an idle, unpaused session held by a project settings change must say why it waits: {preview:?}"
        );
        assert!(
            names_since_time(&preview),
            "the reason names when the settings change began: {preview:?}"
        );
        assert_eq!(
            public_preview.as_deref(),
            Some(preview.as_str()),
            "the public session shows the same reason"
        );
    });
    // Released: the reason is gone.
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(!record.engram.project_reset_in_progress);
        assert!(
            !record
                .session
                .preview
                .starts_with(PROJECT_RESET_WAIT_PREFIX),
            "the release replaces the waiting reason: {:?}",
            record.session.preview
        );
    }
    // Every step the fence was held for was written with its duration, so a
    // reset that keeps its fence names the step it is in.
    let lines = engram_project_reset_phase_lines(&project);
    let prefix = format!("engram> project={project} reset generation ");
    assert!(
        lines.iter().all(|line| line.starts_with(&prefix)),
        "{lines:#?}"
    );
    let written = |event: &str| lines.iter().any(|line| line.contains(event));
    assert!(written(" fence claimed"), "{lines:#?}");
    assert!(
        written(" fence released and queues drained (fence held at most "),
        "{lines:#?}"
    );
    for phase in [
        "quiescence".to_owned(),
        "checkpoint claims".to_owned(),
        "timed-out checkpoints".to_owned(),
        "commit".to_owned(),
        "authority revocation".to_owned(),
        format!("control shutdown session={session}"),
        "MCP runtime shutdown".to_owned(),
        format!("fresh bind session={session}"),
        "MCP runtime finalize".to_owned(),
        "release and drain".to_owned(),
    ] {
        assert!(
            written(&format!(" phase {phase} started (fence held ")),
            "{phase} start: {lines:#?}"
        );
        assert!(
            written(&format!(" phase {phase} took ")),
            "{phase} end: {lines:#?}"
        );
    }
    assert!(
        lines
            .last()
            .is_some_and(|line| line.contains(" finished (fence held ")),
        "{lines:#?}"
    );
}

#[test]
fn a_paused_or_busy_session_keeps_its_own_preview_while_the_fence_is_held() {
    for case in ["paused", "busy"] {
        let (state, session, _receiver, transport) = root_fixture([]);
        let (project, generation, original) = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let project = inner.sessions[index].session.project_id.clone().unwrap();
            let generation = inner.engram_project_resets.claim(&project).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.engram.project_reset_in_progress = true;
            let original = if case == "paused" {
                record.set_auto_dispatch_blocked(true);
                "Paused by the test.".to_owned()
            } else {
                record.session.status = SessionStatus::Active;
                "Working on the test turn.".to_owned()
            };
            record.session.preview = original.clone();
            (project, generation, original)
        };
        queue_test_engram_prompt(
            &state,
            &session,
            "a wake during the settings change",
            QueuedPromptSource::Mailbox,
            None,
        );
        assert!(
            state
                .dispatch_next_queued_turn(&session, false)
                .unwrap()
                .is_none(),
            "{case}: the fence holds the queue"
        );
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(
                record.session.preview, original,
                "{case}: its own preview stays"
            );
        }
        assert!(
            transport.requests().is_empty(),
            "{case}: nothing reached Engram"
        );
        let mut inner = state.inner.lock().unwrap();
        assert!(inner.engram_project_resets.release(&project, generation));
    }
}

/// An idle, unpaused session behind a held fence, with the given preview
/// before it, a queued mailbox wake, and the waiting reason the drain showed.
/// Returns the project, the fence generation, and the revision the reason
/// was committed at.
fn fenced_session_showing_the_reason(
    state: &AppState,
    session: &str,
    original: &str,
) -> (String, u64, u64) {
    let (project, generation) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(session).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let generation = inner.engram_project_resets.claim(&project).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram.project_reset_in_progress = true;
        record.session.preview = original.to_owned();
        (project, generation)
    };
    queue_test_engram_prompt(
        state,
        session,
        "a wake during the settings change",
        QueuedPromptSource::Mailbox,
        None,
    );
    let queued_revision = state.inner.lock().unwrap().revision;
    assert!(
        state
            .dispatch_next_queued_turn(session, false)
            .unwrap()
            .is_none()
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    assert!(
        record
            .session
            .preview
            .starts_with(PROJECT_RESET_WAIT_PREFIX),
        "{:?}",
        record.session.preview
    );
    // A client that adopted the queued wake's revision adopts only a newer
    // one: the reason is committed at a revision past it.
    assert!(
        inner.revision > queued_revision,
        "the waiting reason must be committed at a newer revision than {queued_revision}, got {}",
        inner.revision
    );
    (project, generation, inner.revision)
}

/// Keeps the release's drain from starting a turn, so the preview it leaves
/// is the restored one.
fn hold_the_drain_with_a_pending_stop(state: &AppState, session: &str) {
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session).unwrap();
    inner
        .session_mut_by_index(index)
        .unwrap()
        .runtime_stop_in_progress = true;
}

#[test]
fn the_release_restores_the_replaced_preview_at_a_newer_revision() {
    let (state, session, _receiver, transport) = root_fixture([]);
    let original = "The preview before the settings change.";
    let (project, generation, shown_revision) =
        fenced_session_showing_the_reason(&state, &session, original);
    hold_the_drain_with_a_pending_stop(&state, &session);
    state.release_engram_project_reset_fence(&project, generation, std::slice::from_ref(&session));
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.session.preview, original);
    assert!(record.engram.project_reset_wait_preview.is_none());
    assert!(
        inner.revision > shown_revision,
        "the restored preview must be committed at a newer revision than {shown_revision}, got {}",
        inner.revision
    );
    assert_eq!(record.queued_prompts.len(), 1, "no turn started");
    assert!(transport.requests().is_empty());
}

#[test]
fn an_aborted_settings_change_drains_what_it_held_and_restores_the_preview() {
    // The aborted change's wake starts its turn, as after a release.
    let (state, session, _receiver, transport) = root_fixture([
        bind_reply("abort-bind"),
        grant_reply("abort-grant"),
        begin_reply("abort-grant"),
    ]);
    let original = "The preview before the aborted settings change.";
    let (project, generation, shown_revision) =
        fenced_session_showing_the_reason(&state, &session, original);
    // A session that joined the project while the fence was held is not in
    // the list the settings change took at its claim, yet shows the reason.
    let root = {
        let inner = state.inner.lock().unwrap();
        let project_record = inner.projects.iter().find(|p| p.id == project).unwrap();
        PathBuf::from(&project_record.root_path)
    };
    let joined = create_test_project_session(&state, Agent::Codex, &project, &root);
    let joined_original = "The joined session's own preview.";
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&joined).unwrap();
        inner.session_mut_by_index(index).unwrap().session.preview = joined_original.to_owned();
    }
    queue_test_engram_prompt(
        &state,
        &joined,
        "a wake for the joined session",
        QueuedPromptSource::Mailbox,
        None,
    );
    assert!(
        state
            .dispatch_next_queued_turn(&joined, false)
            .unwrap()
            .is_none()
    );
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&joined).unwrap()];
        assert!(
            record
                .session
                .preview
                .starts_with(PROJECT_RESET_WAIT_PREFIX),
            "{:?}",
            record.session.preview
        );
    }
    // The joined session's drain starts no turn, so its restored preview is
    // the one left.
    hold_the_drain_with_a_pending_stop(&state, &joined);
    state
        .abort_engram_project_reset(&project, generation, std::slice::from_ref(&session), &[])
        .unwrap();
    let inner = state.inner.lock().unwrap();
    assert!(!inner.engram_project_resets.contains(&project));
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(
        record.queued_prompts.is_empty(),
        "the wake the aborted change held starts its turn: {:?}",
        record.queued_prompts
    );
    assert!(
        !record
            .session
            .preview
            .starts_with(PROJECT_RESET_WAIT_PREFIX),
        "{:?}",
        record.session.preview
    );
    let joined_record = &inner.sessions[inner.find_session_index(&joined).unwrap()];
    assert_eq!(
        joined_record.session.preview, joined_original,
        "the abort clears the reason of every session the fence held"
    );
    assert!(
        inner.revision > shown_revision,
        "the restored preview must be committed at a newer revision than {shown_revision}, got {}",
        inner.revision
    );
    assert!(
        transport
            .requests()
            .iter()
            .any(|request| request.request["operation"] == "turn_begin"),
        "the drained wake was admitted"
    );
}

#[test]
fn a_failed_settings_commit_restores_the_replaced_preview() {
    let (mut state, session, receiver, _transport) = root_fixture([]);
    let (project, mut settings, root) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let project_record = inner.projects.iter_mut().find(|p| p.id == project).unwrap();
        let root = PathBuf::from(&project_record.root_path);
        fs::write(root.join(".engram-project"), "fixture-ok\n").unwrap();
        let settings = project_record.engram.as_mut().unwrap();
        settings.binary_path = Some(
            real_engram_control_fixture_path()
                .to_string_lossy()
                .into_owned(),
        );
        (project, settings.clone(), root)
    };
    state.mailbox_store = Arc::new(
        MailboxStore::open(&resolve_coordination_persistence_path(
            state.persistence_path.as_ref(),
        ))
        .unwrap(),
    );
    let sender = test_session_id(&state, Agent::Claude);
    let before = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .session
            .preview
            .clone()
    };
    // The release after the rollback drains the session; a pending stop keeps
    // that drain from starting a turn, so the restored preview is the one left.
    hold_the_drain_with_a_pending_stop(&state, &session);
    // The settings change commits to a store that rejects it; the wake keeps
    // the working store.
    state.shutdown_persist_blocking();
    let mut reset_state = state.clone();
    let failed_path = root.join("failed-reset-wait-store.sqlite");
    fs::create_dir_all(&failed_path).unwrap();
    reset_state.persistence_path = Arc::new(failed_path);
    let new_home = root.join("failed-reset-wait-home");
    fs::create_dir_all(&new_home).unwrap();
    settings.home = Some(new_home.to_string_lossy().into_owned());
    let reset_gate = gate_next_engram_project_reset_fence(&project);
    std::thread::scope(|scope| {
        let reset = scope.spawn(|| {
            let result = reset_state.update_project_engram_settings(&project, settings);
            abort_engram_project_reset_fence_gate(&project, &fence_gate_detail(&result));
            result
        });
        let shown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reset_gate.wait_until_entered();
            state
                .append_mailbox_message_and_notify(
                    &sender,
                    SendMailboxMessageRequest {
                        target_session_id: session.clone(),
                        message: "a wake during the failing settings change".to_owned(),
                        idempotency_key: "failed-reset-wait-wake".to_owned(),
                        topic: Some("failed reset wait".to_owned()),
                        state_stamp: None,
                        class: Some("routine".to_owned()),
                    },
                )
                .unwrap();
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .session
                .preview
                .clone()
        }));
        reset_gate.release();
        let result = reset.join();
        let shown = shown.unwrap_or_else(|error| std::panic::resume_unwind(error));
        assert!(
            shown.starts_with(PROJECT_RESET_WAIT_PREFIX),
            "the wake showed the reason while the fence was held: {shown:?}"
        );
        let error = match result.expect("reset worker should settle") {
            Err(error) => error,
            Ok(_) => panic!("the rejecting store must fail the settings commit"),
        };
        assert!(
            error
                .message
                .contains("failed to persist Engram project settings"),
            "{}",
            error.message
        );
    });
    let inner = state.inner.lock().unwrap();
    assert!(!inner.engram_project_resets.contains(&project));
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(
        record.session.preview, before,
        "the rolled-back settings change restores the replaced preview"
    );
    assert!(receiver.try_recv().is_err(), "nothing reached the provider");
}

#[test]
fn a_direct_prompt_queued_behind_the_fence_shows_the_reason() {
    let (state, session, _receiver, transport) = root_fixture([]);
    let generation = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let generation = inner.engram_project_resets.claim(&project).unwrap();
        inner
            .session_mut_by_index(index)
            .unwrap()
            .engram
            .project_reset_in_progress = true;
        (project, generation)
    };
    let result = state
        .dispatch_turn(
            &session,
            SendMessageRequest {
                text: "a direct prompt during the settings change".to_owned(),
                expanded_text: None,
                attachments: vec![],
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .unwrap();
    assert!(matches!(result, DispatchTurnResult::Queued));
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.queued_prompts.len(), 1);
        assert!(
            record
                .session
                .preview
                .starts_with(PROJECT_RESET_WAIT_PREFIX),
            "a prompt the fence queues says why it waits: {:?}",
            record.session.preview
        );
    }
    assert!(transport.requests().is_empty());
    let (project, generation) = generation;
    let mut inner = state.inner.lock().unwrap();
    assert!(inner.engram_project_resets.release(&project, generation));
}

#[test]
fn canceling_the_last_queued_prompt_behind_the_fence_restores_the_preview() {
    let (state, session, _receiver, transport) = root_fixture([]);
    let original = "The preview before the canceled wake.";
    let (project, generation, shown_revision) =
        fenced_session_showing_the_reason(&state, &session, original);
    let prompt = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts[0]
            .pending_prompt
            .id
            .clone()
    };
    state.cancel_queued_prompt(&session, &prompt).unwrap();
    let mut inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.queued_prompts.is_empty());
    assert_eq!(
        record.session.preview, original,
        "no queued prompt is left to wait for"
    );
    assert!(
        inner.revision > shown_revision,
        "the restored preview must be committed at a newer revision than {shown_revision}, got {}",
        inner.revision
    );
    assert!(transport.requests().is_empty());
    assert!(inner.engram_project_resets.release(&project, generation));
}

#[test]
fn the_release_with_runtime_revocation_restores_the_preview_unless_it_lost_the_fence() {
    // Released by its own generation: the reason goes, at a newer revision.
    let (state, session, _receiver, transport) = root_fixture([]);
    let original = "The preview before the authority rotation.";
    let (project, generation, shown_revision) =
        fenced_session_showing_the_reason(&state, &session, original);
    hold_the_drain_with_a_pending_stop(&state, &session);
    let release = state.release_engram_project_and_runtime_revocation_fences(
        &project,
        generation,
        std::slice::from_ref(&session),
        EngramMcpRuntimeRevocationBatch::default(),
    );
    assert!(!release.project_fence_release_failed);
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.session.preview, original);
        assert!(
            inner.revision > shown_revision,
            "the restored preview must be committed at a newer revision than {shown_revision}, got {}",
            inner.revision
        );
    }
    assert!(transport.requests().is_empty());

    // Its generation was lost to a newer one: that fence still holds the
    // session, so the reason stays.
    let (state, session, _receiver, _transport) = root_fixture([]);
    let (project, generation, _) =
        fenced_session_showing_the_reason(&state, &session, "The preview before the lost fence.");
    let newer = {
        let mut inner = state.inner.lock().unwrap();
        assert!(inner.engram_project_resets.release(&project, generation));
        inner.engram_project_resets.claim(&project).unwrap()
    };
    let release = state.release_engram_project_and_runtime_revocation_fences(
        &project,
        generation,
        std::slice::from_ref(&session),
        EngramMcpRuntimeRevocationBatch::default(),
    );
    assert!(release.project_fence_release_failed);
    let mut inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(
        record
            .session
            .preview
            .starts_with(PROJECT_RESET_WAIT_PREFIX),
        "the newer fence still holds the session: {:?}",
        record.session.preview
    );
    assert!(inner.engram_project_resets.release(&project, newer));
}

#[test]
fn an_authority_only_settings_change_writes_its_phases_while_it_holds_the_fence() {
    // Rotating only the work-authority grant holds the project fence in the
    // authority branch, not the reset below it; its steps are written too.
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("reset-wait-authority-phases");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join(".engram-project"), "fixture-ready\n").unwrap();
    let project = create_test_project(&state, &root, "Reset wait authority phases");
    let session = create_test_project_session(&state, Agent::Claude, &project, &root);
    let mut enabled = real_fixture_engram_settings(&root);
    enabled.work_authority_grant = Some("grant-old".to_owned());
    state
        .update_project_engram_settings(&project, enabled.clone())
        .expect("fixture settings should enable Engram");
    attach_engram_mcp_test_runtime(&state, &session);
    // The enabling change is a reset of its own; only the rotation is read.
    engram_project_reset_phase_lines(&project);
    let mut rotated = enabled;
    rotated.work_authority_grant = Some("grant-new".to_owned());
    state
        .update_project_engram_settings(&project, rotated)
        .expect("the rotation should finish");
    let lines = engram_project_reset_phase_lines(&project);
    let written = |event: &str| lines.iter().any(|line| line.contains(event));
    assert!(written(" fence claimed"), "{lines:#?}");
    for phase in [
        "authority preflight",
        "commit",
        "authority revocation",
        "release and drain",
    ] {
        assert!(
            written(&format!(" phase {phase} started (fence held ")),
            "{phase} start: {lines:#?}"
        );
        assert!(
            written(&format!(" phase {phase} took ")),
            "{phase} end: {lines:#?}"
        );
    }
    assert!(
        written(" fence released and queues drained (fence held at most "),
        "{lines:#?}"
    );
    assert!(
        lines
            .last()
            .is_some_and(|line| line.contains(" finished (fence held ")),
        "{lines:#?}"
    );
}

#[test]
fn a_waiting_reason_saved_before_a_restart_loads_as_the_idle_preview() {
    // The fence lives in memory only, so no reason survives a restart.
    let saved = engram_project_reset_wait_preview(Some(chrono::Utc::now()));
    assert_eq!(
        engram_project_reset_wait_preview_after_restart(saved),
        "Ready for a prompt."
    );
    assert_eq!(
        engram_project_reset_wait_preview_after_restart("Last reply.".to_owned()),
        "Last reply."
    );
}
