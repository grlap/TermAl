// Owns renewed-view naming, clear and exact lost-reply recovery controls.
// Does not change the producer's retained event or canonical run association.
// New module using the fixtures from engram_named_root_naming_supersede.rs.
// Also covers successor naming through an original reporter whose live
// association has changed, without rewriting the retained event or reader.

fn substituted_reporter_naming(clear: bool) {
    for reporter_unbound in [false, true] {
        for superseded in [false, true] {
            let label = format!("reporter-{clear}-{reporter_unbound}-{superseded}");
            let (mut claimed, tree) = root_fixture(&label);
            if !clear {
                claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
                    .lose_next_reply = true;
                request_fixture_name(&claimed, Some(&tree)).unwrap_err();
            } else {
                request_fixture_name(&claimed, Some(&tree)).unwrap();
            }
            let reporter = claimed.session_id.clone();
            let original = claimed.record(|record| record.engram.work_binding.clone().unwrap());
            let project = claimed.record(|record| record.session.project_id.clone().unwrap());
            let store = naming_store(&claimed);
            let reporter_binding = if reporter_unbound {
                None
            } else {
                let mut other = original.clone();
                other.work_id.push_str("-elsewhere");
                other.claim_id.push_str("-elsewhere");
                other.run_id.push_str("-elsewhere");
                other.root_execution_id.push_str("-elsewhere");
                Some(other)
            };
            claimed.record_mut(|record| record.engram.work_binding = reporter_binding.clone());
            claimed.session_id =
                create_test_project_session(&claimed.state, Agent::Codex, &project, &claimed.root);
            claimed.record_mut(|record| record.engram.work_binding = Some(original.clone()));
            claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
                .work_session_read_reply = Some(Ok(json!({
                    "project_id": "github.com/example/source-root",
                    "session_id": claimed.session_id,
                    "work_session": true, "capabilities": ["host_work_v1"],
                })));
            let witnessed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            if superseded {
                supersede_after_renewed_send(
                    claimed.state.clone(), claimed.transport.clone(), store.clone(),
                    original.clone(), witnessed.clone(),
                );
            }
            let result = request_fixture_name(&claimed, if clear { None } else { Some(&tree) });
            TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| hook.borrow_mut().take());
            let response = result.unwrap_or_else(|error| panic!("{label}: {error:?}"));
            assert_eq!(witnessed.load(std::sync::atomic::Ordering::SeqCst), superseded);
            let requests = claimed.transport.requests();
            let sends = requests.iter()
                .filter(|request| request.request["operation"] == "named_root_bind")
                .collect::<Vec<_>>();
            assert_eq!(sends.len(), 2, "owner recovery does not send another event");
            assert!(sends.iter().all(|request| request.connection.session_id == reporter));
            if !clear {
                assert_eq!(sends[0].request, sends[1].request, "immutable intent replay");
            } else {
                assert_eq!(sends[1].request["kind"], "ended");
            }
            let inner = claimed.state.inner.lock().unwrap();
            let reporter_record = &inner.sessions[inner.find_session_index(&reporter).unwrap()];
            assert_eq!(reporter_record.engram.work_binding, reporter_binding);
            let caller = &inner.sessions[inner.find_session_index(&claimed.session_id).unwrap()];
            assert_eq!(caller.engram.work_binding.as_ref(), Some(&original));
            assert!(inner.engram_named_root_journal.iter().all(|journal| journal.pending.is_none()));
            let selected = engram_work_source_root_for_work(
                &inner.engram_work_source_roots, &store, &original.work_id,
            );
            if clear {
                assert!(selected.is_none());
                assert_eq!(response.generation, 0);
            } else {
                assert_eq!(selected.unwrap().generation, response.generation);
                let journal = engram_root_journal(
                    &inner.engram_named_root_journal, &store, &original.claim_id,
                ).unwrap();
                assert_eq!(journal.read_binding.as_ref(), Some(&original));
            }
        }
    }
}

#[test]
fn a_successor_replays_the_original_reporters_intent_after_the_reporter_moves() {
    substituted_reporter_naming(false);
}

#[test]
fn a_successor_clears_the_original_reporters_name_after_the_reporter_moves() {
    substituted_reporter_naming(true);
}

fn supersede_after_renewed_send(
    state: AppState,
    transport: Arc<ScriptedEngramControlTransport>,
    store: EngramAuthorityStoreKey,
    binding: EngramControlWorkBinding,
    witnessed: Arc<std::sync::atomic::AtomicBool>,
) {
    TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let sent = transport
                .requests()
                .iter()
                .filter(|request| request.request["operation"] == "named_root_bind")
                .count();
            // A lost reply leaves an intent before preparatory reads. Wait for
            // the second actual send so this races learning its canonical reply.
            if sent < 2 {
                supersede_after_renewed_send(state, transport, store, binding, witnessed);
                return;
            }
            let before = {
                let inner = state.inner.lock().unwrap();
                assert!(
                    engram_root_journal(
                        &inner.engram_named_root_journal,
                        &store,
                        &binding.claim_id
                    )
                    .unwrap()
                    .pending
                    .is_some()
                );
                inner
                    .engram_work_naming_history
                    .iter()
                    .find(|history| history.store == store && history.work_id == binding.work_id)
                    .unwrap()
                    .transition
                    .clone()
                    .unwrap()
            };
            let after = state
                .prepare_engram_authority(&store, &binding, DEADLOCK_GUARD)
                .unwrap();
            assert_eq!(before.binding, after.binding);
            assert_ne!(before.id, after.id);
            witnessed.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
    });
}

fn renewed_naming(operation: &str) {
    for revision_only in [false, true] {
        for superseded in [false, true] {
            let label = format!("renewed-{operation}-{revision_only}-{superseded}");
            let (claimed, first) = root_fixture(&label);
            let path = if operation == "rename" {
                let other = claimed.root.join(".worktrees").join("other");
                run_git_test_command(
                    &claimed.root,
                    &[
                        "worktree",
                        "add",
                        "--quiet",
                        "-b",
                        "other",
                        other.to_str().unwrap(),
                    ],
                );
                Some(other)
            } else if operation == "clear" {
                None
            } else {
                Some(first.clone())
            };
            if operation == "lost" {
                claimed
                    .transport
                    .named_roots
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .lose_next_reply = true;
                request_fixture_name(&claimed, Some(&first)).unwrap_err();
            } else {
                request_fixture_name(&claimed, Some(&first)).unwrap();
            }
            let store = naming_store(&claimed);
            let original = claimed.record(|record| record.engram.work_binding.clone().unwrap());
            let mut held = naming_claim(&label);
            let claim = &mut held.items[0];
            if !revision_only {
                claim.claim_fence += 1;
            }
            let live = claim.control_binding.as_mut().unwrap();
            live.claim_fence = claim.claim_fence;
            if revision_only {
                live.work_revision += 1;
            }
            let live = live.clone();
            assert!(engram_same_recovery_run(&original, &live));
            assert_ne!(original, live);
            claimed
                .transport
                .set_current_work_binding(Some(live.clone()));
            claimed
                .transport
                .named_roots
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .claim_fences
                .insert(live.claim_id.clone(), live.claim_fence);
            let witnessed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            if superseded {
                supersede_after_renewed_send(
                    claimed.state.clone(),
                    claimed.transport.clone(),
                    store.clone(),
                    original.clone(),
                    witnessed.clone(),
                );
            }
            let result = request_naming(&claimed, &label, path.as_deref(), vec![held]);
            TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| hook.borrow_mut().take());
            let response = result.unwrap_or_else(|error| panic!("{label}: {error:?}"));
            assert_eq!(
                witnessed.load(std::sync::atomic::Ordering::SeqCst),
                superseded
            );
            assert_eq!(
                claimed.record(|record| record.engram.work_binding.clone()),
                Some(live)
            );
            let sends = claimed
                .transport
                .requests()
                .into_iter()
                .filter(|request| request.request["operation"] == "named_root_bind")
                .map(|request| request.request)
                .collect::<Vec<_>>();
            assert_eq!(sends.len(), 2, "owner recovery must not resend");
            if operation == "lost" {
                assert_eq!(sends[0], sends[1], "exact original event replay");
            }
            let inner = claimed.state.inner.lock().unwrap();
            assert!(
                inner
                    .engram_named_root_journal
                    .iter()
                    .all(|journal| journal.pending.is_none())
            );
            let journal = engram_root_journal(&inner.engram_named_root_journal, &store, &original.claim_id);
            if path.is_some() {
                assert!(journal.is_some(), "a bound naming retains its original read association");
            }
            if let Some(journal) = journal {
                assert_eq!(journal.read_binding.as_ref(), Some(&original));
            }
            match &path {
                Some(path) => {
                    let selected = engram_work_source_root_for_work(
                        &inner.engram_work_source_roots,
                        &store,
                        &original.work_id,
                    )
                    .unwrap();
                    assert_eq!(selected.generation, response.generation);
                    assert_eq!(
                        engram_canonical_path_key(FsPath::new(&selected.root)),
                        engram_canonical_path_key(path)
                    );
                    assert_eq!(
                        response.generation,
                        sends[1]["generation"].as_u64().unwrap()
                    );
                }
                None => {
                    assert!(
                        engram_work_source_root_for_work(
                            &inner.engram_work_source_roots,
                            &store,
                            &original.work_id
                        )
                        .is_none()
                    );
                    assert!(response.root.is_none());
                    assert_eq!(response.generation, 0);
                }
            }
        }
    }
}

#[test]
fn renewed_view_rename_recovers_only_its_captured_live_binding() {
    renewed_naming("rename");
}

#[test]
fn renewed_view_clear_recovers_only_its_captured_live_binding() {
    renewed_naming("clear");
}

#[test]
fn renewed_view_lost_reply_replays_only_its_original_event() {
    renewed_naming("lost");
}
