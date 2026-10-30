// Owns parked-schedule transfer, stale timer and explicit Resume reservation
// regressions through containment. Does not settle an independent grant or
// exercise live authority, automatic retry policy, or retirement producers.
use super::*;

fn scheduled_retry_json(state: &AppState, session: &str) -> Value {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    json!({"bind": record.engram.bind_retry, "admission": record.engram.admission_retry})
}

fn frozen_retry_json(image: &EngramUnsettledControlImage) -> Value {
    json!({"bind": image.parked_bind_retry, "admission": image.parked_admission_retry})
}

#[test]
fn project_containment_only_scoped_request_preparation_can_cross_interruption() {
    let (state, project, session, _root, receiver, transport) =
        retained_admission(&["turn_evaluate"]);
    state
        .update_project_engram_settings(&project, settings(&state, &project, true))
        .unwrap();
    let capture_scope = state.begin_contained_admission_resume(&session).unwrap().unwrap();
    let (target, owner, intent, original) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        let head = record.queued_prompts.front().unwrap();
        assert!(
            head.engram_interrupted,
            "OFF preserves the interrupted head"
        );
        let target =
            AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
                .unwrap()
                .unwrap();
        (
            target,
            EngramQueuedAdmissionOwner::capture(record).unwrap(),
            EngramTurnIntentSnapshot {
                session_id: session.clone(),
                dispatch_generation: head.engram_evaluate.as_ref().unwrap().operation_generation.unwrap(),
                intent_fingerprint: containment_prompt_fingerprint(head),
            },
            head.engram_evaluate.as_ref().unwrap().request.clone(),
        )
    };
    drop(capture_scope);
    let sent = transport.inner.requests().len();
    assert!(
        state
            .queued_engram_evaluate_request(&target, &intent, &owner, original.clone())
            .is_err()
    );
    let scope = state
        .begin_contained_admission_resume(&session)
        .unwrap()
        .unwrap();
    let replay = state
        .queued_engram_evaluate_request(&target, &intent, &owner, original.clone())
        .unwrap();
    assert_eq!(
        serde_json::to_value(replay).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
    std::thread::scope(|threads| {
        threads
            .spawn(|| {
                assert!(
                    state
                        .queued_engram_evaluate_request(&target, &intent, &owner, original.clone())
                        .is_err()
                );
            })
            .join()
            .unwrap();
    });
    drop(scope);
    assert!(
        state
            .queued_engram_evaluate_request(&target, &intent, &owner, original)
            .is_err()
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.queued_prompts[0].engram_interrupted);
    assert!(!record.engram.recovered_admission);
    assert!(engram_session_obligation_locked(&inner, &session).is_some());
    assert_eq!(
        transport.inner.requests().len(),
        sent,
        "preparation does not send a request"
    );
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_consumes_exact_parked_retry_and_stale_due_tick_delivers_nothing() {
    for lost in ["session_bind", "turn_evaluate"] {
        let (state, project, session, _root, receiver, transport) = parked_admission(&[lost]);
        let retry = scheduled_retry_json(&state, &session);
        let (owner, kind, generation, due_at) = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            let (kind, generation, due_at) = if lost == "session_bind" {
                let retry = record
                    .engram
                    .bind_retry
                    .as_ref()
                    .expect("the actual Bind park");
                assert!(engram_bind_retry_head_matches(record, &retry.proof));
                (
                    EngramRetryKind::Bind,
                    retry.proof.dispatch_generation,
                    retry.due_at.clone(),
                )
            } else {
                let retry = record
                    .engram
                    .admission_retry
                    .as_ref()
                    .expect("the actual Evaluate park");
                assert!(engram_admission_retry_head_matches(record, retry));
                (
                    EngramRetryKind::Admission,
                    retry.generation,
                    retry.due_at.clone(),
                )
            };
            (
                EngramQueuedAdmissionOwner::capture(record).unwrap(),
                kind,
                generation,
                due_at,
            )
        };
        let sent = transport.inner.requests().len();
        state
            .update_project_engram_settings(&project, settings(&state, &project, false))
            .unwrap();
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.engram.admission_retry.is_none());
            assert!(record.engram.bind_retry.is_none());
            let image =
                containment_original_admission(&inner.engram_session_obligations.entries[0])
                    .unwrap();
            assert_eq!(frozen_retry_json(&image), retry);
            assert_eq!(image.dispatch_generation, generation);
            assert!(
                !owner.matches(record),
                "the selected timer lost its original owner"
            );
        }
        assert_eq!(
            transport.inner.requests().len(),
            sent,
            "OFF sends no admission RPC"
        );
        state
            .update_project_engram_settings(&project, settings(&state, &project, true))
            .unwrap();
        let due = chrono::Utc::now() + chrono::Duration::hours(1);
        let slot = state.engram_retry_slots.try_acquire().unwrap();
        state.run_engram_retry_attempt(
            EngramDueRetry {
                session_id: session.clone(),
                owner: owner.into(),
                kind,
                due_at,
            },
            due,
            slot,
        );
        state.engram_abort_retry_tick(due);
        assert_eq!(transport.inner.requests().len(), sent);
        assert!(receiver.try_recv().is_err());
        state.resume_session_queue(&session).unwrap();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            CodexRuntimeCommand::Prompt { .. }
        ));
        let after_resume = transport.inner.requests().len();
        state.engram_abort_retry_tick(due);
        assert_eq!(transport.inner.requests().len(), after_resume);
        assert!(
            receiver.try_recv().is_err(),
            "the later due tick cannot deliver twice"
        );
    }
}

#[test]
fn project_containment_keeps_mismatched_and_in_flight_retry_excluded() {
    for lost in ["session_bind", "turn_evaluate"] {
        for changed in [
            "generation",
            "authority",
            "fingerprint",
            "in_flight",
            "reset",
            "paused",
            "interrupted",
            "runtime",
            "phase",
        ] {
            if lost != "session_bind" && matches!(changed, "runtime" | "phase") {
                continue;
            }
            let (state, project, session, _root, receiver, transport) = parked_admission(&[lost]);
            {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                let record = inner.session_mut_by_index(index).unwrap();
                match changed {
                    "generation" => {
                        if let Some(retry) = record.engram.admission_retry.as_mut() {
                            retry.generation += 1;
                        } else {
                            record
                                .engram
                                .bind_retry
                                .as_mut()
                                .unwrap()
                                .proof
                                .dispatch_generation += 1;
                        }
                    }
                    "authority" => {
                        if let Some(retry) = record.engram.admission_retry.as_mut() {
                            retry.authority.push_str("-changed");
                        } else {
                            record
                                .engram
                                .bind_retry
                                .as_mut()
                                .unwrap()
                                .proof
                                .authority
                                .push_str("-changed");
                        }
                    }
                    "fingerprint" => {
                        if let Some(retry) = record.engram.admission_retry.as_mut() {
                            retry.fingerprint.push_str("-changed");
                        } else {
                            record
                                .engram
                                .bind_retry
                                .as_mut()
                                .unwrap()
                                .proof
                                .fingerprint
                                .push_str("-changed");
                        }
                    }
                    "in_flight" => {
                        record.engram.admission_in_progress = Some(Arc::new(AtomicBool::new(false)))
                    }
                    "reset" => record.engram.project_reset_in_progress = true,
                    "paused" => record.engram.operator_paused = true,
                    "interrupted" => record.queued_prompts[0].engram_interrupted = true,
                    "runtime" => record.engram.bind_retry_runtime = None,
                    "phase" => {
                        record.engram.bind_retry.as_mut().unwrap().proof.phase =
                            EngramBindRetryPhase::BeforePreparation
                    }
                    _ => unreachable!(),
                }
                state.commit_locked(&mut inner).unwrap();
            }
            let original = scheduled_retry_json(&state, &session);
            let sent = transport.inner.requests().len();
            state
                .update_project_engram_settings(&project, settings(&state, &project, false))
                .unwrap();
            assert_eq!(
                scheduled_retry_json(&state, &session),
                original,
                "{lost}/{changed}"
            );
            state
                .update_project_engram_settings(&project, settings(&state, &project, true))
                .unwrap();
            assert!(state.resume_session_queue(&session).is_err(), "{changed}");
            assert_eq!(transport.inner.requests().len(), sent, "{changed}");
            assert!(receiver.try_recv().is_err(), "{changed}");
        }
    }
}

#[test]
fn project_containment_resume_reservation_excludes_duplicates_until_writer_reply() {
    for acknowledged in [false, true] {
        let (mut state, project, session, _root, receiver, transport) =
            retained_admission(&["turn_evaluate"]);
        state
            .update_project_engram_settings(&project, settings(&state, &project, true))
            .unwrap();
        state.shutdown_persist_blocking();
        let original_tx = state.persist_tx.clone();
        let (tx, rx) = mpsc::channel();
        state.persist_tx = tx;
        let sent = transport.inner.requests().len();
        std::thread::scope(|threads| {
            let first = threads.spawn(|| {
                state
                    .begin_contained_admission_resume(&session)
                    .map(|scope| scope.is_some())
            });
            let fence = loop {
                if let PersistRequest::Fence(fence) =
                    phase_sync::receive(&rx, "Resume's writer fence")
                {
                    assert!(matches!(
                        &fence.target,
                        PersistFenceTarget::EngramProjectContainment(_)
                    ));
                    break fence;
                }
            };
            assert!(
                state.begin_contained_admission_resume(&session).is_err(),
                "a second caller cannot share the reservation"
            );
            state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::hours(1));
            assert_eq!(transport.inner.requests().len(), sent);
            assert!(receiver.try_recv().is_err());
            fence.finish(if acknowledged {
                Ok(())
            } else {
                Err(PersistFenceError::Deadline)
            });
            assert_eq!(first.join().unwrap().is_ok(), acknowledged);
        });
        state.persist_tx = original_tx;
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.engram.containment_resume_owner.is_none());
            assert!(record.engram.admission_retry.is_none());
            assert!(engram_session_obligation_locked(&inner, &session).is_some());
        }
        let scope = state
            .begin_contained_admission_resume(&session)
            .unwrap()
            .unwrap();
        drop(scope);
        assert_eq!(
            transport.inner.requests().len(),
            sent,
            "acknowledgement alone sends nothing"
        );
    }
}

#[test]
fn project_containment_failed_retry_transfer_never_revives_timer_and_reload_keeps_exclusion() {
    for lost in ["session_bind", "turn_evaluate"] {
        for failure in ["write", "ack"] {
            let (state, project, session, root, receiver, transport) = parked_admission(&[lost]);
            let retry = scheduled_retry_json(&state, &session);
            let sent = transport.inner.requests().len();
            let failing = AppState {
                persistence_path: if failure == "write" {
                    Arc::new(root)
                } else {
                    state.persistence_path.clone()
                },
                ..state.clone()
            };
            TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(failure != "ack"));
            let result =
                failing.update_project_engram_settings(&project, settings(&state, &project, false));
            TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
            assert!(result.is_err(), "{failure}");
            {
                let mut inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert!(record.engram.admission_retry.is_none());
                assert!(record.engram.bind_retry.is_none());
                assert!(record.engram.containment_resume_owner.is_none());
                assert!(engram_session_obligation_locked(&inner, &session).is_some());
                assert!(!inner.engram_project_resets.contains(&project));
                let image =
                    containment_original_admission(&inner.engram_session_obligations.entries[0])
                        .unwrap();
                assert_eq!(frozen_retry_json(&image), retry);
                // A subsequent successful persistence saves the conservative image,
                // rather than rolling the consumed automatic owner back into it.
                state.commit_locked(&mut inner).unwrap();
            }
            state.shutdown_persist_blocking();
            let loaded = load_state(state.persistence_path.as_path())
                .unwrap()
                .unwrap();
            let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
            assert!(record.engram.admission_retry.is_none());
            assert!(record.engram.bind_retry.is_none());
            assert!(record.engram.containment_resume_owner.is_none());
            assert!(engram_session_obligation_locked(&loaded, &session).is_some());
            assert!(loaded.engram_confirmed_containment_ids.is_empty());
            assert_eq!(transport.inner.requests().len(), sent);
            assert!(receiver.try_recv().is_err());
        }
    }
}
