//! Post-commit trim ownership and unrelated router progress between trim units.
//! Uses the real mailbox store and existing StateInner/trim owners; no provider.
use super::*;

struct TrimBoundary;

impl Drop for TrimBoundary {
    fn drop(&mut self) {
        PERSIST_TAIL_TRIM_BOUNDARY.with(|hook| hook.borrow_mut().take());
    }
}

fn install_boundary(hook: impl FnMut(&str, &'static str) + 'static) -> TrimBoundary {
    PERSIST_TAIL_TRIM_BOUNDARY.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(hook));
    });
    TrimBoundary
}

fn post(app: &Router, path: String, body: Value) -> impl Future<Output = (StatusCode, Value)> + '_ {
    request_json(
        app,
        Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postcommit_trim_releases_state_between_units_for_real_mailbox_and_resume() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (mut state, sender, target) = crate::tests::mailboxes::mailbox_test_state();
    let receipt = state
        .append_mailbox_message_and_notify(
            &sender,
            SendMailboxMessageRequest {
                target_session_id: target.clone(),
                message: "bookkeeping progress".into(),
                idempotency_key: "bookkeeping-progress".into(),
                topic: None,
                state_stamp: None,
                class: Some("routine".into()),
            },
        )
        .unwrap();
    // Keep production-shaped queued persistence/publication without a worker
    // racing this explicitly driven post-commit boundary.
    let (persist_tx, _persist_rx) = mpsc::channel();
    state.persist_tx = persist_tx;
    state.state_broadcast_mailbox = Some(Arc::new(StateBroadcastMailbox::default()));
    let (ids, empty) = {
        let mut inner = state.inner.lock().unwrap();
        let first = seed_postcommit_trim_session(&mut inner, SessionStatus::Idle, 10);
        let second = seed_postcommit_trim_session(&mut inner, SessionStatus::Idle, 10);
        let empty = inner
            .create_session(
                Agent::Codex,
                None,
                state.default_workdir.clone(),
                None,
                None,
            )
            .session
            .id;
        let index = inner.find_session_index(&empty).unwrap();
        inner.sessions[index].set_auto_dispatch_blocked(true);
        (vec![first, second], empty)
    };
    let app = app_router(state.clone());
    let (boundary_tx, boundary_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let first = ids[0].clone();
    let worker_state = state.clone();
    let worker_ids = ids.clone();
    let worker = std::thread::spawn(move || {
        let observed_state = worker_state.clone();
        let mut release_rx = Some(release_rx);
        let _boundary = install_boundary(move |id, phase| {
            if id == first && phase == "unit_complete" {
                let released = observed_state
                    .inner
                    .is_not_held_by_current_thread_for_test();
                boundary_tx.send(released).unwrap();
                if released {
                    let _ = release_rx
                        .take()
                        .unwrap()
                        .recv_timeout(phase_sync::DEADLOCK_GUARD);
                }
            }
        });
        trim_persisted_session_tails_from_shared_state(&worker_state.inner, 10, &worker_ids);
    });
    let released = phase_sync::receive(&boundary_rx, "first trim unit");
    if released {
        let (status, page) = post(
            &app,
            format!(
                "/api/sessions/{target}/mailboxes/{}/read",
                receipt.mailbox_id
            ),
            json!({"issueReceipt":true}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let issued = page["receipt"]
            .as_str()
            .expect("real read issues a receipt");
        let ack_body = json!({"receipt":issued});
        for _ in 0..2 {
            let (status, ack) = post(
                &app,
                format!(
                    "/api/sessions/{target}/mailboxes/{}/acknowledge",
                    receipt.mailbox_id
                ),
                ack_body.clone(),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "unchanged receipt replay: {ack}");
        }
        let (status, response) = post(
            &app,
            format!("/api/sessions/{empty}/queue/resume"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        let mut inner = state.inner.lock().unwrap();
        let first = &inner.sessions[inner.find_session_index(&ids[0]).unwrap()];
        assert_eq!(first.message_start_index, 2);
        let next = inner.find_session_index(&ids[1]).unwrap();
        assert_eq!(
            inner.sessions[next].message_start_index, 0,
            "second unit has not run"
        );
        inner.sessions[next].mutation_stamp = 11;
        inner.sessions[next].session.status = SessionStatus::Stopping;
        inner.sessions[next].active_turn_generation += 1;
    }
    drop(release_tx);
    worker.join().unwrap();
    assert!(
        released,
        "unrelated APIs must progress before the trim batch ends"
    );
    let inner = state.inner.lock().unwrap();
    let next = &inner.sessions[inner.find_session_index(&ids[1]).unwrap()];
    assert_eq!(
        next.message_start_index, 0,
        "newer/Stopping successor is rechecked"
    );
    assert_eq!(
        next.session.messages.len(),
        SESSION_IN_MEMORY_MESSAGE_LIMIT + 2
    );
    assert_eq!(
        next.message_positions,
        build_message_positions(&next.session.messages)
    );
}

#[test]
fn postcommit_trim_releases_retired_storage_after_unlock_without_cloning_bodies() {
    let mut inner = StateInner::new();
    let id = seed_postcommit_trim_session(&mut inner, SessionStatus::Idle, 10);
    let weak = inner.sessions[inner.find_session_index(&id).unwrap()]
        .session
        .messages
        .identities();
    let shared = Arc::new(StateMutex::new(inner));
    let observed = shared.clone();
    let observation = Arc::new(Mutex::new(Vec::new()));
    let observations = observation.clone();
    let first = weak[0].clone();
    let _boundary = install_boundary(move |_, phase| {
        observations.lock().unwrap().push((
            phase,
            observed.is_not_held_by_current_thread_for_test(),
            first.upgrade().is_some(),
        ));
    });
    let (_, clones) = MessageSequence::observe_shared_body_clones(|| {
        trim_persisted_session_tails_from_shared_state(&shared, 10, &[id.clone()]);
    });
    assert_eq!(
        clones, 0,
        "retirement moves shared identities, never copies Message bodies"
    );
    assert_eq!(
        *observation.lock().unwrap(),
        vec![
            ("before_disposal", true, true),
            ("unit_complete", true, false),
        ]
    );
    let inner = shared.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
    assert!(record.session.messages.matches_identity(0, &weak[2]));
    assert_eq!(record.message_start_index, 2);
    assert_eq!(
        record.session.message_count as usize,
        SESSION_IN_MEMORY_MESSAGE_LIMIT + 2
    );
}

#[test]
fn postcommit_trim_rechecks_removal_hide_and_pause_on_each_unit() {
    let mut inner = StateInner::new();
    let first = seed_postcommit_trim_session(&mut inner, SessionStatus::Idle, 10);
    let removed = seed_postcommit_trim_session(&mut inner, SessionStatus::Idle, 10);
    let hidden = seed_postcommit_trim_session(&mut inner, SessionStatus::Idle, 10);
    let paused = seed_postcommit_trim_session(&mut inner, SessionStatus::Idle, 10);
    let ids = vec![
        first.clone(),
        removed.clone(),
        hidden.clone(),
        paused.clone(),
    ];
    let shared = Arc::new(StateMutex::new(inner));
    let changed = shared.clone();
    let removed_at_boundary = removed.clone();
    let hidden_at_boundary = hidden.clone();
    let paused_at_boundary = paused.clone();
    let _boundary = install_boundary(move |id, phase| {
        if id == first && phase == "unit_complete" {
            let mut inner = changed.lock().unwrap();
            let index = inner.find_session_index(&removed_at_boundary).unwrap();
            inner.sessions.remove(index);
            let index = inner.find_session_index(&hidden_at_boundary).unwrap();
            inner.sessions[index].hidden = true;
            inner.sessions[index].mutation_stamp = 11;
            let index = inner.find_session_index(&paused_at_boundary).unwrap();
            inner.sessions[index].set_auto_dispatch_blocked(true);
            inner.sessions[index].active_turn_generation += 1;
            inner.sessions[index].mutation_stamp = 11;
        }
    });
    trim_persisted_session_tails_from_shared_state(&shared, 10, &ids);
    let inner = shared.lock().unwrap();
    assert!(inner.find_session_index(&removed).is_none());
    for id in [hidden, paused] {
        let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
        assert_eq!(record.message_start_index, 0);
        assert_eq!(
            record.session.messages.len(),
            SESSION_IN_MEMORY_MESSAGE_LIMIT + 2
        );
        assert_eq!(
            record.message_positions,
            build_message_positions(&record.session.messages)
        );
    }
}
