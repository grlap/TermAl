use super::*;

fn ordered_publication_state() -> (AppState, String) {
    let state = test_app_state();
    let session = test_session_id(&state, Agent::Codex);
    state
        .push_message(
            &session,
            Message::Text {
                id: "streamed-body".to_owned(),
                author: Author::Assistant,
                text: "first".to_owned(),
                timestamp: stamp_now(),
                attachments: Vec::new(),
                expanded_text: None,
                source: None,
            },
        )
        .unwrap();
    (state, session)
}

#[test]
fn ordered_publication_text_commit_cannot_be_overtaken_by_a_concurrent_snapshot() {
    let (initial_revision, received) = concurrent_text_publication(false);
    assert_eq!(
        received
            .iter()
            .map(|event| event["revision"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![initial_revision + 1, initial_revision + 2],
        "the real broadcaster must deliver the text commit before the concurrent newer snapshot"
    );
    assert_eq!(received[0]["delta"], " and complete");
}

#[test]
fn ordered_publication_after_unlock_witness_reverses_commit_order() {
    let (initial_revision, received) = concurrent_text_publication(true);
    assert_eq!(
        received
            .iter()
            .map(|event| event["revision"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![initial_revision + 2, initial_revision + 1],
        "publishing after unlock must violate the production regression's ordering assertion"
    );
    assert_eq!(received[1]["delta"], " and complete");
}

fn concurrent_text_publication(before_fix: bool) -> (u64, Vec<Value>) {
    let mut state = test_app_state();
    let session = test_session_id(&state, Agent::Codex);
    state
        .push_message(
            &session,
            Message::Text {
                id: "streamed-body".to_owned(),
                author: Author::Assistant,
                text: "first".to_owned(),
                timestamp: stamp_now(),
                attachments: Vec::new(),
                expanded_text: None,
                source: None,
            },
        )
        .unwrap();
    let mailbox = Arc::new(StateBroadcastMailbox::default());
    state.state_broadcast_mailbox = Some(mailbox.clone());
    let mut receiver = state.subscribe_stream_events();
    let initial_revision = state.inner.lock().unwrap().revision;
    let (at_boundary, boundary) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let producer = state.clone();
    let producer_session = session.clone();
    let first = std::thread::spawn(move || {
        TEST_AFTER_TEXT_COMMIT.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                at_boundary.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(10)).unwrap();
            }))
        });
        if before_fix {
            producer
                .append_text_delta_before_enqueue_fix(
                    &producer_session,
                    "streamed-body",
                    " and complete",
                )
                .unwrap();
        } else {
            producer
                .append_text_delta(&producer_session, "streamed-body", " and complete")
                .unwrap();
        }
    });
    boundary.recv_timeout(Duration::from_secs(10)).unwrap();
    let second_state = state.clone();
    let second = std::thread::spawn(move || {
        let mut inner = second_state.inner.lock().unwrap();
        second_state.commit_locked(&mut inner).unwrap();
    });
    second.join().unwrap();
    release.send(()).unwrap();
    first.join().unwrap();
    let senders = state.state_broadcast_senders.clone();
    let broadcaster = std::thread::spawn(move || {
        for _ in 0..2 {
            forward_state_broadcast_work(mailbox.recv_next(), &senders);
        }
    });
    broadcaster.join().unwrap();
    let received: Vec<Value> = (0..2)
        .map(|_| match receiver.try_recv().unwrap() {
            SseStreamEvent::State(body) | SseStreamEvent::Delta(body) => {
                serde_json::from_str(&body).unwrap()
            }
        })
        .collect();
    (initial_revision, received)
}

#[test]
fn ordered_publication_serialization_does_not_hold_the_commit_mutex() {
    let chunk = "\"\\\n".repeat(32_768);
    let (mut before, before_session) = ordered_publication_state();
    let before_holds = Arc::new(Mutex::new(Vec::new()));
    let observed_before_holds = before_holds.clone();
    let before_mutex = Arc::get_mut(&mut before.inner).expect("fixture owns its state mutex");
    before_mutex.warn_after = Duration::ZERO;
    before_mutex.diagnostic_reporter = Arc::new(move |diagnostic| {
        if let StateMutexDiagnostic::Held { held, .. } = diagnostic {
            observed_before_holds.lock().unwrap().push(held);
        }
    });
    before
        .append_text_delta_before_enqueue_fix(&before_session, "streamed-body", &chunk)
        .unwrap();
    let (mut state, session) = ordered_publication_state();
    let holds = Arc::new(Mutex::new(Vec::new()));
    let observed_holds = holds.clone();
    let mutex = Arc::get_mut(&mut state.inner).expect("fixture owns its state mutex");
    mutex.warn_after = Duration::ZERO;
    mutex.diagnostic_reporter = Arc::new(move |diagnostic| {
        if let StateMutexDiagnostic::Held { held, .. } = diagnostic {
            observed_holds.lock().unwrap().push(held);
        }
    });
    let mailbox = Arc::new(StateBroadcastMailbox::default());
    state.state_broadcast_mailbox = Some(mailbox.clone());
    state
        .append_text_delta(&session, "streamed-body", &chunk)
        .unwrap();
    let senders = state.state_broadcast_senders.clone();
    let worker_state = state.clone();
    let (at_serialization, serialization) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let serialization_started = Arc::new(Mutex::new(None));
        let probe_started = serialization_started.clone();
        TEST_BEFORE_STATE_SERIALIZE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                assert!(worker_state.inner.is_not_held_by_current_thread_for_test());
                at_serialization.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(10)).unwrap();
                *probe_started.lock().unwrap() = Some(std::time::Instant::now());
            }))
        });
        forward_state_broadcast_work(mailbox.recv_next(), &senders);
        let elapsed = serialization_started.lock().unwrap().unwrap().elapsed();
        elapsed
    });
    serialization.recv_timeout(Duration::from_secs(10)).unwrap();
    // This actual commit must finish while the serializer is suspended. A
    // duration threshold alone would not prove independence from serialization.
    let producer = state.clone();
    let (committed, commit) = mpsc::channel();
    let publisher = std::thread::spawn(move || {
        let mut inner = producer.inner.lock().unwrap();
        producer.commit_locked(&mut inner).unwrap();
        committed.send(()).unwrap();
    });
    let commit_result = commit.recv_timeout(Duration::from_secs(10));
    release.send(()).unwrap();
    let serialization = worker.join().unwrap();
    publisher.join().unwrap();
    commit_result.expect("serialization cannot block a concurrent commit");
    let holds = holds.lock().unwrap();
    assert!(holds.len() >= 2);
    eprintln!(
        "ordered publication measured lock holds (microseconds), base057c3895 append={:?}, typed enqueue append+concurrent snapshot={:?}, worker serialization={}; suspended worker did not block commit",
        before_holds
            .lock()
            .unwrap()
            .iter()
            .map(Duration::as_micros)
            .collect::<Vec<_>>(),
        holds.iter().map(Duration::as_micros).collect::<Vec<_>>(),
        serialization.as_micros()
    );
}

// Test-only model of append_text_delta from base 057c3895: mutation and
// revision allocation under the mutex, caller-thread serialization after unlock.
// The ordering witness feeds the same broadcaster with a typed event after
// unlock; the measurement's no-mailbox path retains the original serialization.
// It never participates in production publication or acceptance admission.
impl AppState {
    fn append_text_delta_before_enqueue_fix(
        &self,
        session_id: &str,
        message_id: &str,
        delta: &str,
    ) -> Result<()> {
        let (
            preview,
            revision,
            message_index,
            message_count,
            text_start_byte,
            session_mutation_stamp,
        ) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let mut preview = None;
            let (message_index, message_count, text_start_byte, session_mutation_stamp) = {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                let message_index =
                    message_index_on_record(record, message_id).ok_or_else(|| {
                        anyhow!("session `{session_id}` message `{message_id}` not found")
                    })?;
                let session = &mut record.session;

                let Some(message) = session.messages.get_mut(message_index) else {
                    return Err(anyhow!(
                        "session `{session_id}` message index `{message_index}` is out of bounds"
                    ));
                };
                let text_start_byte = match message {
                    Message::Text { id, text, .. } if id == message_id => {
                        let text_start_byte = text.len();
                        text.push_str(delta);
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            preview = Some(make_preview(trimmed));
                        }
                        text_start_byte
                    }
                    _ => {
                        return Err(anyhow!(
                            "session `{session_id}` message `{message_id}` is not a text message"
                        ));
                    }
                };

                if let Some(next_preview) = preview.as_ref() {
                    session.preview = next_preview.clone();
                }
                (
                    global_message_index(record, message_index),
                    session_message_count(record),
                    text_start_byte,
                    record.mutation_stamp,
                )
            };
            let revision = self.commit_delta_locked(&mut inner)?;
            (
                preview,
                revision,
                message_index,
                message_count,
                text_start_byte,
                session_mutation_stamp,
            )
        };

        TEST_AFTER_TEXT_COMMIT.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        let event = DeltaEvent::TextDelta {
            revision,
            session_id: session_id.to_owned(),
            message_id: message_id.to_owned(),
            message_index,
            message_count,
            text_start_byte,
            delta: delta.to_owned(),
            preview,
            session_mutation_stamp: Some(session_mutation_stamp),
        };
        if let Some(mailbox) = self.state_broadcast_mailbox.as_ref() {
            mailbox.publish_delta(event);
        } else {
            self.state_broadcast_senders
                .send_delta(serde_json::to_string(&event)?);
        }

        Ok(())
    }
}
