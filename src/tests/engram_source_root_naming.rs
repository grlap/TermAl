// Owns the tests of the naming request behind `termal_name_source_root`
// (`AppState::name_engram_source_root`) and of the host line it and a bind
// leave for the agent's next prompt: the refusals, the generation a name gets,
// the seal a clear leaves on the running turn, authoritative reconciliation
// within its budget, and when the line counts as delivered. Does not own the
// validation, the end rules or the basis taken on a named root
// (src/tests/engram_source_roots.rs), or turns measured in a named root
// (src/tests/engram_turn_observations.rs, whose `ClaimedRoot` fixture this
// child module uses). New module.
use super::*;

mod authority {
    include!("engram_root_authority.rs");
}

#[path = "engram_legacy_root_recovery.rs"]
mod legacy_recovery;

#[path = "engram_source_root_confirmation.rs"]
mod confirmation;

impl ClaimedRoot {
    /// These reconciliation fixtures script BOTH producer operations. A
    /// status tuple alone cannot invent the canonical run/event carrier.
    fn reconcile_root_readback(
        &self,
        session: &str,
        token: &str,
        binding: Option<&EngramControlWorkBinding>,
        state: Option<EngramNamedRootState>,
        generation: u64,
    ) -> Result<(), EngramTransportError> {
        if let Some(binding) = binding
            && let Some(state) = state
                .as_ref()
                .filter(|state| **state != EngramNamedRootState::Unknown)
        {
            let store = claimed_root_store(self);
            let local = engram_work_source_root_for_claim(
                &self.state.inner.lock().unwrap().engram_work_source_roots,
                &store,
                &binding.work_id,
                &binding.claim_id,
            )
            .cloned();
            let mut roots = self.transport.named_roots.lock().unwrap();
            let roots = roots.get_or_insert_with(TestEngramNamedRoots::default);
            roots.register(binding);
            roots.project_id = Some(store.project_id.clone());
            let mut event = roots.root_reads.get(&binding.claim_id).and_then(|read| read.get("latest_event"))
                .filter(|event| !event.is_null()).cloned()
                .or_else(|| roots.latest.get(&binding.claim_id).cloned())
                .or_else(|| local.as_ref().map(|root| json!({"event":"external-prior-binding",
                    "position":{"feed":{"kind":"run_execution","id":binding.run_id},"position":1},
                    "kind":"bound","generation":root.generation,"workspace_id":root.root,"named_at":root.named_at})));
            let prior_position = event
                .as_ref()
                .and_then(|event| event["position"]["position"].as_i64())
                .unwrap_or(0);
            let prior_generation = event
                .as_ref()
                .and_then(|event| event["generation"].as_i64())
                .unwrap_or(0);
            let mut projected = serde_json::to_value(state).unwrap();
            match state {
                EngramNamedRootState::Bound {
                    workspace_id,
                    generation,
                    named_at,
                } => {
                    if *generation > prior_generation {
                        event = Some(
                            json!({"event":format!("external-bound-{}-{generation}", binding.claim_id),
                            "position":{"feed":{"kind":"run_execution","id":binding.run_id},"position":prior_position+1},
                            "kind":"bound","generation":generation,"workspace_id":workspace_id,"named_at":named_at}),
                        );
                    } else if event.as_ref().is_some_and(|event| event["kind"] == "ended") {
                        projected = json!({"state":"none"}); // A stale tuple cannot revive a ended event.
                    }
                }
                EngramNamedRootState::None => {
                    if let Some(event) = &mut event
                        && event["kind"] != "ended"
                    {
                        event["kind"] = json!("ended");
                        event["event"] = json!(format!(
                            "external-ended-{}-{}",
                            binding.claim_id,
                            prior_position + 1
                        ));
                        event["position"]["position"] = json!(prior_position + 1);
                    }
                }
                EngramNamedRootState::UnboundByRelease {
                    last_generation, ..
                } => {
                    if *last_generation > prior_generation {
                        event = Some(
                            json!({"event":format!("external-bound-{}-{last_generation}",binding.claim_id),
                            "position":{"feed":{"kind":"run_execution","id":binding.run_id},"position":prior_position+1},
                            "kind":"bound","generation":last_generation,
                            "workspace_id":local.as_ref().map(|root| root.root.as_str()).unwrap_or("external-worktree"),
                            "named_at":local.as_ref().map(|root| root.named_at.as_str()).unwrap_or("2026-01-01T00:00:00Z")}),
                        );
                    }
                }
                EngramNamedRootState::Unknown => unreachable!(),
            }
            let event_position = event
                .as_ref()
                .and_then(|event| event["position"]["position"].as_i64())
                .unwrap_or(0);
            let cut = projected["released_at_position"]
                .as_i64()
                .unwrap_or(event_position)
                .max(event_position)
                .max(
                    roots
                        .root_reads
                        .get(&binding.claim_id)
                        .and_then(|read| read["read_cut"]["position"].as_i64())
                        .unwrap_or(0),
                );
            let proof = json!({"project_id":store.project_id,"work_id":binding.work_id,"run_id":binding.run_id,
                "claim_id":binding.claim_id,"root_execution_id":binding.root_execution_id,
                "run":{"state":if projected["state"] == "unbound_by_release" { "open" } else { "claimed" },
                    "generation":roots.run_generations[&binding.run_id]},
                "named_root":projected,"latest_event":event,
                "read_cut":{"feed":{"kind":"run_execution","id":binding.run_id},"position":cut}});
            roots.root_reads.insert(binding.claim_id.clone(), proof);
        }
        self.state
            .reconcile_engram_named_root(session, token, binding, state, generation)
    }
}

#[test]
fn source_root_review_round_six_receipt_uses_the_run_feed_wire_shape() {
    // Engram's NamedRootBindingReceipt carries FeedPosition, including the
    // exact run identity; a bare integer is not the protocol.
    let value = json!({"event":"binding-event", "position":{
        "feed":{"kind":"run_execution", "id":"receipt-run"}, "position":7},
        "workspace_id":"workspace", "generation":1, "kind":"bound"});
    let receipt: EngramNamedRootReceipt = serde_json::from_value(value.clone())
        .expect("the documented run-feed receipt must deserialize");
    assert_eq!(serde_json::to_value(receipt).unwrap(), value);
    let mut integer = value;
    integer["position"] = json!(7);
    assert!(serde_json::from_value::<EngramNamedRootReceipt>(integer).is_err());
}

#[test]
fn source_root_review_round_six_partial_read_batches_do_not_starve_later_claims() {
    partial_root_reader_keeps_progress(false);
}

#[test]
fn source_root_review_round_six_partial_sweeps_ignore_filtered_reads() {
    partial_root_reader_keeps_progress(true);
}

fn partial_root_reader_keeps_progress(interleave_filtered: bool) {
    let label = "partial-lifecycle-read";
    let mut claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    replace_naming_session(&mut claimed, label);
    claimed.record(|record| record.engram.work_binding = None);
    let original = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal[0]
        .clone();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        inner.engram_named_root_journal.clear();
        for n in 0..16 {
            let mut journal = original.clone();
            journal.claim_id = format!("batch-claim-{n:02}");
            let binding = journal.read_binding.as_mut().unwrap();
            binding.claim_id = journal.claim_id.clone();
            binding.run_id = format!("batch-run-{n:02}");
            journal.confirmed.as_mut().unwrap().1.position.feed.id = binding.run_id.clone();
            journal.confirmed.as_mut().unwrap().0.root.claim_id = journal.claim_id.clone();
            let mut reply = covering_removed_root_read(&journal, true);
            reply["run"]["generation"] = json!(n + 2);
            let mut fixture = claimed.transport.named_roots.lock().unwrap();
            let roots = fixture.as_mut().unwrap();
            roots.root_reads.insert(journal.claim_id.clone(), reply);
            if n % 8 == 0 {
                roots.root_read_failures.insert(journal.claim_id.clone());
            }
            inner.engram_named_root_journal.push(journal);
        }
    }
    // The test clock exhausts the deadline after exactly one attempted read.
    // One rotation of sixteen attempts must visit all sixteen identities,
    // regardless of elapsed transport/persistence time or machine load.
    for _ in 0..16 {
        resolve_removed_roots_with_one_attempt(&claimed, None);
        if interleave_filtered {
            let cursor = claimed
                .state
                .inner
                .lock()
                .unwrap()
                .engram_root_read_cursor
                .clone();
            // A requested-claim read must not reset the unfiltered sweep cursor.
            resolve_removed_roots_with_one_attempt(&claimed, Some("batch-claim-00"));
            assert_eq!(
                claimed.state.inner.lock().unwrap().engram_root_read_cursor,
                cursor
            );
        }
    }
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(
        inner.engram_named_root_journal.len(),
        2,
        "only unavailable claims remain"
    );
    assert!(
        inner
            .engram_named_root_journal
            .iter()
            .all(|journal| journal.claim_id == "batch-claim-00"
                || journal.claim_id == "batch-claim-08")
    );
}

fn resolve_removed_roots_with_one_attempt(claimed: &ClaimedRoot, claim: Option<&str>) {
    struct ClearClock;
    impl Drop for ClearClock {
        fn drop(&mut self) {
            TEST_ENGRAM_REMOVED_ROOT_READ_CLOCK.with(|clock| *clock.borrow_mut() = None);
        }
    }
    let start = std::time::Instant::now();
    let mut samples = 0;
    TEST_ENGRAM_REMOVED_ROOT_READ_CLOCK.with(|clock| {
        *clock.borrow_mut() = Some(Box::new(move || {
            samples += 1;
            if samples <= 2 {
                start
            } else {
                start + Duration::from_secs(2)
            }
        }));
    });
    let _clear = ClearClock;
    claimed
        .state
        .resolve_removed_engram_roots(&claimed.session_id, claim, Duration::from_secs(2));
}

#[test]
fn source_root_review_round_six_rejects_a_receipt_for_another_run_and_retries() {
    let label = "receipt-wrong-run";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    claimed_root_store(&claimed);
    claimed
        .transport
        .replace_held_claims([Ok(claimed_root_held(label))]);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .runs
        .insert(binding.claim_id.clone(), "another-run".to_owned());
    // Call the production API directly: name_root's setup helper would reset
    // the intentionally corrupt server run identity before the request.
    let refused = claimed
        .state
        .name_engram_source_root(
            &claimed.session_id,
            EngramSourceRootRequest {
                work: format!("w-{label}"),
                path: Some(Some(worktree.to_string_lossy().into_owned())),
            },
        )
        .expect_err("a different run cannot confirm this intent");
    assert!(refused.message.contains("mismatched receipt"));
    {
        let inner = claimed.state.inner.lock().unwrap();
        assert!(inner.engram_work_source_roots.is_empty());
        assert!(inner.engram_named_root_journal[0].pending.is_some());
    }
    {
        let mut fixture = claimed.transport.named_roots.lock().unwrap();
        let roots = fixture.as_mut().unwrap();
        for (_, receipt) in roots.seen.values_mut() {
            receipt["position"]["feed"]["id"] = json!(binding.run_id);
        }
        roots.latest.get_mut(&binding.claim_id).unwrap()["position"]["feed"]["id"] =
            json!(binding.run_id);
    }
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    assert_eq!(
        claimed
            .transport
            .named_roots
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .seen
            .len(),
        1
    );
}

#[test]
fn source_root_review_round_six_release_read_persists_new_history_and_rolls_back_failure() {
    let label = "learn-released-history";
    let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
    prepare_claimed_root_naming(&claimed, label);
    let store = claimed_root_store(&claimed);
    let (token, binding) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone().unwrap(),
        )
    });
    let before = engram_work_naming_token(
        &claimed.state.inner.lock().unwrap(),
        &store,
        &binding.work_id,
    );
    let read = Some(EngramNamedRootState::UnboundByRelease {
        last_generation: 7,
        released_at_position: 100,
    });
    let durable_path = claimed.state.persistence_path.clone();
    let failing_path = claimed.root.join("release-read-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    claimed.state.persistence_path = Arc::new(failing_path);
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            Some(&binding),
            read.clone(),
            u64::MAX,
        )
        .unwrap_err();
    assert_eq!(
        engram_work_naming_token(
            &claimed.state.inner.lock().unwrap(),
            &store,
            &binding.work_id
        ),
        before
    );
    claimed.state.persistence_path = durable_path;
    for _ in 0..2 {
        claimed
            .reconcile_root_readback(
                &claimed.session_id,
                &token,
                Some(&binding),
                read.clone(),
                u64::MAX,
            )
            .unwrap();
    }
    let inner = claimed.state.inner.lock().unwrap();
    let learned = engram_work_naming_token(&inner, &store, &binding.work_id);
    assert_eq!(
        learned.revision, 1,
        "repeated released readback is idempotent"
    );
    assert!(!engram_work_naming_is_current(&inner, &store, &before));
    let restored = load_state(claimed.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        restored.engram_work_naming_history,
        inner.engram_work_naming_history
    );
}

fn replace_naming_session(claimed: &mut ClaimedRoot, label: &str) {
    replace_or_unbind_naming_session(claimed, label, true);
}

fn covering_removed_root_read(journal: &EngramNamedRootJournal, released: bool) -> Value {
    let binding = journal.read_binding.as_ref().unwrap();
    let (event, receipt) = journal.confirmed.as_ref().unwrap();
    let position = json!({"feed": {"kind": "run_execution", "id": binding.run_id},
        "position": receipt.position.position});
    json!({
        "project_id": journal.store.project_id, "work_id": binding.work_id,
        "root_execution_id": binding.root_execution_id, "run_id": binding.run_id,
        "claim_id": binding.claim_id, "run": {"state": if released {"open"} else {"completed"}, "generation":1},
        "named_root": if released {
            json!({"state": "unbound_by_release", "last_generation": receipt.generation,
                "released_at_position": receipt.position.position + 1})
        } else { json!({"state": "none"}) },
        "latest_event": {"event": receipt.event, "position": position,
            "generation": receipt.generation, "kind": receipt.kind,
            "workspace_id": receipt.workspace_id, "named_at": event.root.named_at},
        "read_cut": {"feed": {"kind": "run_execution", "id": binding.run_id},
            "position": receipt.position.position + 1}
    })
}

#[test]
fn source_root_review_naming_history_tracks_publication_and_keeps_exact_replay_idempotent() {
    let label = "naming-history-publication";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let named = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let (store, work_id) = {
        let inner = claimed.state.inner.lock().unwrap();
        (
            inner.engram_work_source_roots[0].store.clone(),
            inner.engram_work_source_roots[0].work_id.clone(),
        )
    };
    let token = || engram_work_naming_token(&claimed.state.inner.lock().unwrap(), &store, &work_id);
    assert_eq!(token().revision, 1);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    assert_eq!(
        token().revision,
        1,
        "an unchanged name does not publish another revision"
    );
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    name_root(
        &claimed,
        label,
        Some(&claimed.root),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    assert_eq!(
        token().revision,
        1,
        "allocation and lost remote reply are not local publication"
    );
    let pending_token = token();
    assert!(
        !engram_work_naming_is_current(
            &claimed.state.inner.lock().unwrap(),
            &store,
            &pending_token
        ),
        "an unfinished publication cannot admit even a request for another claim"
    );
    let pending_generation = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_source_root_generation;
    assert!(pending_generation > named.generation);
    name_root(
        &claimed,
        label,
        Some(&claimed.root),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    assert_eq!(
        token().revision,
        2,
        "confirmed publication after allocation advances history"
    );
    name_root(
        &claimed,
        label,
        Some(&claimed.root),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    assert_eq!(token().revision, 2, "exact replay is idempotent");
    name_root(&claimed, label, None, vec![claimed_root_held(label)]).unwrap();
    assert_eq!(token().revision, 2, "clear does not reset naming history");
}

#[test]
fn source_root_review_removed_root_reclamation_requires_covering_lifecycle_proof() {
    for case in [
        "completed",
        "released",
        "bound",
        "unknown",
        "unsupported",
        "stale",
        "wrong-run",
        "wrong-kind",
        "orphan",
        "reconciliation",
        "local-transition",
        "persist-failure",
    ] {
        let label = "removed-root-read";
        let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
        let worktree = add_claimed_root_worktree(&claimed.root);
        name_root(
            &claimed,
            label,
            Some(&worktree),
            vec![claimed_root_held(label)],
        )
        .unwrap();
        replace_naming_session(&mut claimed, label);
        // The live same-store reader does not focus or bind the historical claim.
        claimed.record(|record| record.engram.work_binding = None);
        let before = claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_named_root_journal[0]
            .clone();
        let mut reply = covering_removed_root_read(&before, case == "released");
        match case {
            "bound" => {
                reply["named_root"] = json!({"state":"bound", "workspace_id":before.confirmed.as_ref().unwrap().0.root.root,
                "generation":before.confirmed.as_ref().unwrap().0.root.generation,
                "named_at":before.confirmed.as_ref().unwrap().0.root.named_at})
            }
            "unknown" => reply["named_root"] = json!({"state":"unknown"}),
            "stale" => reply["read_cut"]["position"] = json!(0),
            "wrong-run" => reply["run_id"] = json!("another-run"),
            "wrong-kind" => reply["latest_event"]["kind"] = json!("ended"),
            "orphan" => {
                let mut inner = claimed.state.inner.lock().unwrap();
                inner.engram_named_root_journal[0].pending =
                    before.confirmed.as_ref().map(|(event, _)| event.clone());
            }
            "reconciliation" => {
                claimed
                    .state
                    .inner
                    .lock()
                    .unwrap()
                    .engram_named_root_journal[0]
                    .reconciliation = Some(EngramRootReconciliation {
                    intent: before.confirmed.as_ref().unwrap().0.clone(),
                    state: EngramNamedRootState::None,
                    observed_by: claimed.session_id.clone(),
                    observed_at: "2026-10-01T00:00:00Z".to_owned(),
                    settled: false,
                });
            }
            "local-transition" => {
                let state = claimed.state.clone();
                let event = before.confirmed.as_ref().unwrap().0.clone();
                TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        state.inner.lock().unwrap().engram_named_root_journal[0].pending =
                            Some(event);
                    }))
                });
            }
            "persist-failure" => {
                let failing_path = claimed.root.join("lifecycle-read-is-directory");
                fs::create_dir_all(&failing_path).unwrap();
                claimed.state.persistence_path = Arc::new(failing_path);
            }
            _ => {}
        }
        if case != "unsupported" {
            claimed
                .transport
                .named_roots
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .root_reads
                .insert(before.claim_id.clone(), reply);
        }
        claimed.state.resolve_removed_engram_roots(
            &claimed.session_id,
            None,
            Duration::from_secs(2),
        );
        let inner = claimed.state.inner.lock().unwrap();
        if matches!(case, "completed" | "released" | "orphan") {
            assert!(
                inner.engram_named_root_journal.is_empty(),
                "{case}: an acknowledged covering terminal proof reclaims the closed association"
            );
            assert!(
                inner.engram_work_naming_history[0]
                    .proofs
                    .iter()
                    .any(
                        |proof| proof.binding == *before.read_binding.as_ref().unwrap()
                            && matches!(proof.read.run.state.as_str(), "completed" | "cancelled")
                            || proof.binding == *before.read_binding.as_ref().unwrap()
                                && matches!(
                                    proof.read.named_root,
                                    EngramNamedRootState::UnboundByRelease { .. }
                                )
                    ),
                "{case}: no original receipt or intent is forgotten without its exact terminal association"
            );
            assert_eq!(
                engram_work_naming_token(
                    &inner,
                    &before.store,
                    &before.confirmed.as_ref().unwrap().0.root.work_id
                )
                .revision,
                1
            );
        } else {
            assert_eq!(inner.engram_named_root_journal.len(), 1, "{case}");
            assert!(
                inner.engram_named_root_journal[0].requires_reconciliation(),
                "{case}"
            );
            assert!(
                acceptance_evaluation_claim_unconfirmed_locked(
                    &inner,
                    &claimed.session_id,
                    &before.store,
                    Some(&AcceptanceEvaluationSourceClaim {
                        work_id: before.confirmed.as_ref().unwrap().0.root.work_id.clone(),
                        claim_id: before.claim_id.clone(),
                        named_generation_at_request: None,
                    })
                ),
                "{case}: an unfocused held claim cannot fall back to cwd"
            );
        }
    }
}

#[test]
fn source_root_review_released_removed_sessions_make_progress_past_journal_capacity() {
    let label = "removed-root-capacity";
    let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
    prepare_claimed_root_naming(&claimed, label);
    claimed_root_store(&claimed);
    let worktree = add_claimed_root_worktree(&claimed.root);
    let mut store_work = None;
    for sequence in 0..=ENGRAM_NAMED_ROOT_JOURNAL_LIMIT {
        let mut held = claimed_root_held(label);
        let binding = held.items[0].control_binding.as_mut().unwrap();
        binding.claim_id = format!("removed-claim-{sequence}");
        binding.run_id = format!("removed-run-{sequence}");
        binding.root_execution_id = format!("removed-execution-{sequence}");
        let binding = binding.clone();
        held.items[0].claim_id = binding.claim_id.clone();
        claimed.record(|record| {
            record.engram.work_binding = Some(binding.clone());
            record.engram.named_root = None;
        });
        claimed
            .transport
            .enable_named_roots(&claimed.session_id, &binding);
        claimed.transport.replace_held_claims([Ok(held)]);
        claimed
            .state
            .name_engram_source_root(
                &claimed.session_id,
                EngramSourceRootRequest {
                    work: format!("w-{label}"),
                    path: Some(Some(worktree.to_string_lossy().into_owned())),
                },
            )
            .expect("released removed roots are reclaimed by authoritative reads");
        let journal = claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_named_root_journal
            .iter()
            .find(|journal| journal.claim_id == binding.claim_id)
            .unwrap()
            .clone();
        store_work = Some((journal.store.clone(), binding.work_id));
        let mut released_read = covering_removed_root_read(&journal, true);
        released_read["run"]["generation"] = json!(
            claimed
                .transport
                .named_roots
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .run_generations[&journal.read_binding.as_ref().unwrap().run_id]
        );
        claimed
            .transport
            .named_roots
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .root_reads
            .insert(journal.claim_id.clone(), released_read);
        replace_naming_session(&mut claimed, label);
    }
    claimed.record(|record| record.engram.work_binding = None);
    claimed
        .state
        .resolve_removed_engram_roots(&claimed.session_id, None, Duration::from_secs(2));
    let inner = claimed.state.inner.lock().unwrap();
    assert!(inner.engram_named_root_journal.is_empty());
    let (store, work) = store_work.unwrap();
    assert_eq!(
        engram_work_naming_token(&inner, &store, &work).revision,
        (ENGRAM_NAMED_ROOT_JOURNAL_LIMIT + 1) as u64
    );
    assert!(
        claimed
            .transport
            .requests()
            .iter()
            .all(|request| request.request["operation"] != "named_root_bind"
                || request.request["kind"] != "ended"),
        "live removal and read reclamation manufacture no remote end"
    );
}

fn replace_or_unbind_naming_session(claimed: &mut ClaimedRoot, label: &str, remove: bool) {
    let project = claimed.record(|record| record.session.project_id.clone().unwrap());
    let successor =
        create_test_project_session(&claimed.state, Agent::Codex, &project, &claimed.root);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let old = inner.find_session_index(&claimed.session_id).unwrap();
        if remove {
            inner.remove_session_at(old);
        } else {
            inner.sessions[old].engram.routing_token = None;
            inner.sessions[old].engram.work_binding = None;
        }
    }
    claimed.session_id = successor;
    prepare_claimed_root_naming(claimed, label);
}

#[test]
fn source_root_orphaned_bound_reply_recovers_without_rewriting_the_intent() {
    source_root_orphaned_intent(false, true, false);
}

#[test]
fn source_root_orphaned_cleanup_reply_recovers_after_its_reporter_is_removed() {
    source_root_orphaned_intent(true, true, false);
}

#[test]
fn source_root_orphaned_intent_recovers_when_its_reporter_is_live_but_unbound() {
    for cleanup in [false, true] {
        source_root_orphaned_intent(cleanup, false, false);
    }
}

#[test]
fn source_root_orphaned_explicit_clear_recovers_without_reassigning_its_reporter() {
    source_root_orphaned_intent(true, true, true);
}

fn source_root_orphaned_intent(cleanup: bool, remove: bool, explicit_clear: bool) {
    let label = "orphaned-intent-recovery";
    let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    if cleanup {
        name_root(
            &claimed,
            label,
            Some(&worktree),
            vec![claimed_root_held(label)],
        )
        .unwrap();
        if explicit_clear {
            claimed
                .transport
                .named_roots
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .lose_next_reply = true;
            name_root(&claimed, label, None, vec![claimed_root_held(label)]).unwrap_err();
        } else {
            // Only a real restore with the naming session absent queues this reason.
            let encoded = {
                let mut inner = claimed.state.inner.lock().unwrap();
                inner.engram_work_source_roots[0].named_by_session = "absent-at-restore".to_owned();
                serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap()
            };
            let restored = serde_json::from_slice::<PersistedState>(&encoded)
                .unwrap()
                .into_inner()
                .unwrap();
            {
                let mut inner = claimed.state.inner.lock().unwrap();
                inner.engram_work_source_roots = restored.engram_work_source_roots;
                inner.engram_named_root_journal = restored.engram_named_root_journal;
            }
            replace_naming_session(&mut claimed, label);
            claimed
                .transport
                .named_roots
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .lose_next_reply = true;
            claimed
                .state
                .flush_engram_root_cleanup(&claimed.session_id, Duration::from_secs(2));
        }
    } else {
        claimed
            .transport
            .named_roots
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .lose_next_reply = true;
        name_root(
            &claimed,
            label,
            Some(&worktree),
            vec![claimed_root_held(label)],
        )
        .unwrap_err();
    }
    let original = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal[0]
        .pending
        .clone()
        .unwrap();
    assert_eq!(
        original.kind,
        if cleanup {
            EngramNamedRootKind::Ended
        } else {
            EngramNamedRootKind::Bound
        }
    );
    replace_or_unbind_naming_session(&mut claimed, label, remove);
    let (token, binding) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone(),
        )
    });
    // Omission can recover canonically, but a failed reader and an explicit
    // Unknown must never abandon a potentially applied event.
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_read_failures
        .insert(binding.as_ref().unwrap().claim_id.clone());
    for unknown in [None, Some(EngramNamedRootState::Unknown)] {
        let omitted = unknown.is_none();
        let result = claimed.reconcile_root_readback(
            &claimed.session_id,
            &token,
            binding.as_ref(),
            unknown,
            u64::MAX,
        );
        if omitted {
            result.expect_err("an omitted projection needs a successful canonical read");
        } else {
            result.unwrap();
        }
        assert_eq!(
            claimed
                .state
                .inner
                .lock()
                .unwrap()
                .engram_named_root_journal[0]
                .pending
                .as_ref(),
            Some(&original)
        );
    }
    assert_eq!(
        claimed.state.engram_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_read_failures
        .remove(&binding.as_ref().unwrap().claim_id);
    let before = claimed
        .transport
        .requests()
        .iter()
        .filter(|r| r.request["operation"] == "named_root_bind")
        .count();
    let read_started = chrono::Utc::now();
    let recovery = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    let read_finished = chrono::Utc::now();
    assert!(
        recovery.message.contains("authoritative recovery"),
        "{}",
        recovery.message
    );
    {
        let inner = claimed.state.inner.lock().unwrap();
        let journal = &inner.engram_named_root_journal[0];
        assert!(journal.pending.is_none());
        assert_eq!(journal.reconciliation.as_ref().unwrap().intent, original);
        let read = journal.reconciliation.as_ref().unwrap();
        assert_eq!(read.observed_by, claimed.session_id);
        let observed = chrono::DateTime::parse_from_rfc3339(&read.observed_at).unwrap();
        // The wire's event time is unchanged; this is the host's read receipt time.
        assert!(observed.timestamp_millis() >= read_started.timestamp_millis());
        assert!(observed.timestamp_millis() <= read_finished.timestamp_millis());
        if !cleanup {
            assert!(
                journal.confirmed.is_none(),
                "readback must not invent a receipt"
            );
        }
    }
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|r| r.request["operation"] == "named_root_bind")
            .count(),
        before
    );
    assert_eq!(
        claimed.state.engram_root_capture(&claimed.session_id),
        if cleanup {
            EngramRootCapture::Unnamed
        } else {
            EngramRootCapture::Unconfirmed
        },
        "a covering Ended proof establishes absence; a Bound root without a local selection remains withheld"
    );
    let recovered = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    assert!(recovered.generation > original.root.generation);
    assert!(matches!(
        claimed.state.engram_root_capture(&claimed.session_id),
        EngramRootCapture::Recorded { .. }
    ));
    assert!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_named_root_journal[0]
            .reconciliation
            .is_none()
    );
}

#[test]
fn source_root_stale_read_cannot_retire_an_orphan_or_replace_a_newer_selection() {
    let label = "orphan-stale-read";
    let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    let original = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal[0]
        .pending
        .clone()
        .unwrap();
    replace_naming_session(&mut claimed, label);
    let (token, binding) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone(),
        )
    });
    let current = EngramNamedRootState::Bound {
        workspace_id: original.root.root.clone(),
        generation: original.root.generation as i64 + 1,
        named_at: original.root.named_at.clone(),
    };
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let mut newer = original.root.clone();
        newer.generation += 1;
        inner.engram_source_root_generation = newer.generation;
        inner.engram_work_source_roots.push(newer);
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        inner.sessions[index].engram.named_root = Some(current.clone());
    }
    let snapshot = {
        let inner = claimed.state.inner.lock().unwrap();
        (
            inner.engram_work_source_roots.clone(),
            inner.engram_named_root_journal.clone(),
            inner.sessions[inner.find_session_index(&claimed.session_id).unwrap()]
                .engram
                .pending_source_root_line
                .clone(),
        )
    };
    for state in [
        None,
        Some(EngramNamedRootState::Unknown),
        Some(EngramNamedRootState::None),
        Some(EngramNamedRootState::UnboundByRelease {
            last_generation: original.root.generation as i64,
            released_at_position: 100,
        }),
    ] {
        claimed
            .reconcile_root_readback(
                &claimed.session_id,
                &token,
                binding.as_ref(),
                state,
                original.root.generation,
            )
            .unwrap();
        let inner = claimed.state.inner.lock().unwrap();
        assert_eq!(inner.engram_work_source_roots, snapshot.0);
        assert_eq!(inner.engram_named_root_journal, snapshot.1);
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        assert_eq!(
            inner.sessions[index].engram.named_root,
            Some(current.clone())
        );
        assert_eq!(
            inner.sessions[index].engram.pending_source_root_line,
            snapshot.2
        );
    }
}

#[test]
fn source_root_orphan_read_persistence_failure_retains_the_exact_retry_intent() {
    let label = "orphan-persist-failure";
    let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    let original = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal[0]
        .clone();
    replace_naming_session(&mut claimed, label);
    let (token, binding) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone(),
        )
    });
    let durable_path = claimed.state.persistence_path.clone();
    let failing_path = claimed.root.join("orphan-read-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    claimed.state.persistence_path = Arc::new(failing_path);
    assert!(
        claimed
            .reconcile_root_readback(
                &claimed.session_id,
                &token,
                binding.as_ref(),
                Some(EngramNamedRootState::None),
                u64::MAX
            )
            .is_err()
    );
    assert_eq!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_named_root_journal[0],
        original
    );
    assert_eq!(
        claimed.state.engram_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    claimed.state.persistence_path = durable_path;
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            binding.as_ref(),
            Some(EngramNamedRootState::None),
            u64::MAX,
        )
        .unwrap();
    let inner = claimed.state.inner.lock().unwrap();
    let read = inner.engram_named_root_journal[0]
        .reconciliation
        .as_ref()
        .unwrap();
    assert_eq!(read.intent, original.pending.unwrap());
    assert!(chrono::DateTime::parse_from_rfc3339(&read.observed_at).is_ok());
    assert!(inner.engram_named_root_journal[0].pending.is_none());
    assert!(inner.engram_named_root_journal[0].confirmed.is_none());
}

#[test]
fn source_root_unknown_at_checkpoint_withholds_previously_finished_check_evidence() {
    let label = "unknown-at-checkpoint";
    let (claimed, worktree, token) = named_root_turn(label, true);
    claimed.state.note_engram_command_started(
        &claimed.session_id,
        &EngramObservationProvenance::Ambient,
        "checked-before-unknown",
        Some("cargo test"),
        Some(worktree.to_str().unwrap()),
    );
    claimed.state.note_engram_command_finished(
        &claimed.session_id,
        &EngramObservationProvenance::Ambient,
        "checked-before-unknown",
        "cargo test",
        "test result: ok. 1 passed",
        Some(EngramCommandExit::Code(0)),
    );
    let captures = claimed.record(|record| {
        record
            .engram
            .active_turn_checks
            .iter()
            .flat_map(|check| {
                std::iter::once(check.start_basis.clone())
                    .chain(check.end.as_ref().map(|end| end.end_basis.clone()))
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(
        captures.len(),
        2,
        "the successful check was captured before authority changed"
    );
    for capture in captures {
        assert!(
            capture
                .wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
                .flatten()
                .is_some()
        );
    }
    claimed.record(|record| record.engram.named_root = Some(EngramNamedRootState::Unknown));
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &token)
        .unwrap();
    let checkpoint = claimed
        .transport
        .requests()
        .into_iter()
        .find(|request| request.request["operation"] == "turn_checkpoint")
        .unwrap()
        .request;
    assert!(
        checkpoint
            .get("verification_evidence")
            .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty)),
        "{checkpoint}"
    );
    assert!(
        checkpoint["observations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|observation| observation.get("source_basis").is_none_or(Value::is_null)),
        "{checkpoint}"
    );
}

#[test]
fn source_root_review_normalized_wire_timestamp_preserves_the_binding_and_intent() {
    let label = "wire-time-normalization";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let named = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let millis = "2026-01-01T00:00:00.000Z";
    let (token, binding, event) = {
        let mut inner = claimed.state.inner.lock().unwrap();
        inner.engram_work_source_roots[0].named_at = millis.to_owned();
        let event = &mut inner.engram_named_root_journal[0]
            .confirmed
            .as_mut()
            .unwrap()
            .0;
        event.root.named_at = millis.to_owned();
        let event = event.clone();
        inner.engram_work_naming_history[0]
            .frontier
            .as_mut()
            .unwrap()
            .named_at = millis.to_owned();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        let record = &mut inner.sessions[index];
        record.engram.named_root = Some(EngramNamedRootState::Bound {
            workspace_id: event.root.root.clone(),
            generation: event.root.generation as i64,
            named_at: millis.to_owned(),
        });
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone(),
            event,
        )
    };
    let wire = {
        let mut roots = claimed.transport.named_roots.lock().unwrap();
        let roots = roots.as_mut().unwrap();
        roots.latest.get_mut(&event.root.claim_id).unwrap()["named_at"] = json!(millis);
        roots.state(&claimed.session_id)
    };
    assert_eq!(wire["named_at"], "2026-01-01T00:00:00Z");
    let remote: EngramNamedRootState = serde_json::from_value(wire).unwrap();
    assert!(engram_root_event_matches_state(&event, Some(&remote)));
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            binding.as_ref(),
            Some(remote),
            u64::MAX,
        )
        .unwrap();
    assert!(matches!(
        claimed.state.engram_root_capture(&claimed.session_id),
        EngramRootCapture::Recorded {
            state: EngramSourceRootState::Named,
            ..
        }
    ));
    let repeated = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    assert_eq!(
        repeated.generation, named.generation,
        "same instant must keep the name"
    );
    name_root(&claimed, label, None, vec![claimed_root_held(label)]).unwrap();
    let events = claimed.transport.requests();
    let clear = events
        .iter()
        .rev()
        .find(|request| request.request["operation"] == "named_root_bind")
        .unwrap();
    assert_eq!(
        clear.request["named_at"], millis,
        "intent bytes stay unchanged"
    );
}

#[test]
fn source_root_review_missing_retry_normalizes_dot_steps() {
    let label = "missing-root-dot-retry";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    fs::rename(worktree.join(".git"), worktree.join("saved-git-entry")).unwrap();
    let error = name_root(
        &claimed,
        label,
        Some(&worktree.join("missing/../.")),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    assert!(
        error.message.contains("root_invalid end is queued"),
        "{}",
        error.message
    );
    let requests = claimed.transport.requests();
    let events = requests
        .iter()
        .filter(|request| request.request["operation"] == "named_root_bind")
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].request, events[1].request,
        "retry replays the original intent"
    );
}

#[test]
fn source_root_review_begin_without_readback_budget_retains_grant_without_evidence() {
    source_root_review_begin_readback_unknown(false);
}

#[test]
fn source_root_review_begin_failed_readback_retains_grant_without_evidence() {
    source_root_review_begin_readback_unknown(true);
}

fn source_root_review_begin_readback_unknown(fail_read: bool) {
    let label = "begin-root-readback-timeout";
    let (claimed, _, runtime_token) = named_root_turn(label, true);
    let (target, binding, token, grant) = {
        let inner = claimed.state.inner.lock().unwrap();
        let target = AppState::engram_binding_target_for_session_shape_locked(
            &inner,
            &claimed.session_id,
            true,
        )
        .unwrap()
        .unwrap();
        let record = &inner.sessions[inner.find_session_index(&claimed.session_id).unwrap()];
        (
            target.clone(),
            record.engram.work_binding.clone(),
            target.routing_token.clone().unwrap(),
            record.engram.active_grant_id.clone().unwrap(),
        )
    };
    let guard = claimed
        .state
        .guard_engram_root_read_until(
            &target,
            binding.as_ref(),
            target.budget_clock.now() + DEADLOCK_GUARD,
        )
        .unwrap();
    let before = claimed.transport.requests().len();
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .fail_next_status = fail_read;
    claimed
        .state
        .reconcile_engram_begin_root_until(
            &target,
            &token,
            binding.as_ref(),
            Some(EngramNamedRootState::None),
            target.budget_clock.now()
                + if fail_read {
                    Duration::from_secs(1)
                } else {
                    Duration::ZERO
                },
            guard,
            None,
        )
        .unwrap();
    assert_eq!(
        claimed.transport.requests().len(),
        before + usize::from(fail_read)
    );
    assert_eq!(
        claimed.record(|r| r.engram.active_grant_id.clone()),
        Some(grant.clone())
    );
    assert_eq!(
        claimed.state.engram_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    claimed
        .state
        .record_engram_turn_start_basis_off_lock(&claimed.session_id, &grant);
    assert!(claimed.record(|r| r.engram.opening_diagnostic.is_some()));
    assert_eq!(
        claimed.state.engram_turn_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    // A later readback must not make this turn's previously withheld checks creditable.
    let recovered = serde_json::from_value(
        claimed
            .transport
            .named_roots
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .state(&claimed.session_id),
    )
    .unwrap();
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            binding.as_ref(),
            Some(recovered),
            u64::MAX,
        )
        .unwrap();
    assert_eq!(
        claimed.state.engram_turn_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    let observation = finish_claimed_turn(&claimed, &runtime_token);
    assert!(observation.get("source_basis").is_none());
    let cached = claimed
        .state
        .cached_engram_turn_report(&claimed.session_id, &grant);
    assert!(cached.observations.iter().all(|o| o.source_basis.is_none()));
    assert!(cached.verification_evidence.is_empty());
    // Admission refreshes the work binding on every turn; keep the same claim.
    claimed
        .transport
        .work_bindings
        .lock()
        .unwrap()
        .push_back(Ok(binding.clone()));
    claimed.transport.responses.lock().unwrap().extend([
        grant_reply("later-confirmed-grant"),
        begin_reply("later-confirmed-grant"),
        checkpoint_reply("later-confirmed-grant"),
    ]);
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
    assert_eq!(
        claimed.record(|r| r.engram.active_grant_id.clone()),
        Some("later-confirmed-grant".to_owned()),
        "the second admission must begin"
    );
    let _ = received_prompt(&claimed);
    assert!(
        claimed.record(|r| r.engram.active_turn_start_basis.is_some()),
        "the next admitted turn refreshes authority instead of inheriting Unknown"
    );
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &runtime_token)
        .unwrap();
    let requests = claimed.transport.requests();
    let later = requests
        .iter()
        .rev()
        .find(|request| request.request["operation"] == "turn_checkpoint")
        .unwrap();
    assert_eq!(later.request["grant_id"], "later-confirmed-grant");
    assert!(
        later.request["observations"][0]
            .get("source_basis")
            .is_some()
    );
}

#[test]
fn source_root_waits_for_the_real_writer_before_transport_and_before_success() {
    let label = "binding-writer-fences";
    let mut claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    let (tx, rx) = std::sync::mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    let transport = claimed.transport.clone();
    let mut batch = PersistFenceBatch::default();
    let mut cache = SqlitePersistConnectionCache::new();
    let task = std::thread::spawn(move || {
        let result = name_root(
            &claimed,
            label,
            Some(&worktree),
            vec![claimed_root_held(label)],
        );
        (claimed, result)
    });
    receive_naming_authority_fence(&rx, &mut batch, &task);
    // The first-contact/status guard precedes staging the actual intent.
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|r| r.request["operation"] == "named_root_bind")
            .count(),
        0
    );
    let guarded = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &guarded,
        &mut batch,
    )
    .unwrap();
    receive_naming_authority_fence(&rx, &mut batch, &task);
    assert!(
        !transport
            .requests()
            .iter()
            .any(|r| r.request["operation"] == "named_root_bind"),
        "queuing the intent must not dispatch it"
    );
    assert_eq!(
        state.engram_root_capture(&session),
        EngramRootCapture::Unconfirmed
    );
    let first = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &first,
        &mut batch,
    )
    .unwrap();
    receive_naming_authority_fence(&rx, &mut batch, &task);
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|r| r.request["operation"] == "named_root_bind")
            .count(),
        1
    );
    assert!(
        !task.is_finished(),
        "a queued confirmed selection is not a successful name yet"
    );
    assert_eq!(
        state.engram_root_capture(&session),
        EngramRootCapture::Unconfirmed
    );
    let second = collect_persist_delta_from_shared_state(&state.inner, first.watermark);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &second,
        &mut batch,
    )
    .unwrap();
    let (_claimed, result) = task.join().unwrap();
    let named = result.unwrap();
    let disk = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        disk.engram_work_source_roots[0].generation,
        named.generation
    );
    assert!(disk.engram_named_root_journal[0].confirmed.is_some());
    // The final removal of pending may still be queued. Its durable copy
    // retains the confirmed receipt, and a restart can replay it exactly.
    assert!(disk.engram_named_root_journal[0].pending.is_none());
    assert_eq!(
        disk.engram_work_naming_history[0]
            .transition
            .as_ref()
            .unwrap()
            .phase,
        EngramAuthorityPhase::Candidate
    );
    assert_ne!(
        state.engram_root_capture(&session),
        EngramRootCapture::Unconfirmed
    );
}

fn receive_naming_authority_fence<T>(
    rx: &std::sync::mpsc::Receiver<PersistRequest>,
    batch: &mut PersistFenceBatch,
    task: &std::thread::JoinHandle<T>,
) {
    loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(request) => {
                let fence = matches!(request, PersistRequest::Fence(_));
                batch.accept(request);
                if fence {
                    return;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Filesystem capture runs before this fence. Its speed is not
                // the assertion: the naming operation's production budget
                // bounds it, and completion without a fence is a failure.
                assert!(
                    !task.is_finished(),
                    "naming returned without requesting its authority fence"
                );
            }
            Err(error) => panic!("writer channel ended before its authority fence: {error}"),
        }
    }
}

#[test]
fn source_root_writer_failure_keeps_the_intent_and_sends_no_binding() {
    let label = "binding-writer-failure";
    let mut claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    let (tx, rx) = std::sync::mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let transport = claimed.transport.clone();
    let task = std::thread::spawn(move || {
        let result = name_root(
            &claimed,
            label,
            Some(&worktree),
            vec![claimed_root_held(label)],
        );
        (claimed, result)
    });
    let mut batch = PersistFenceBatch::default();
    receive_naming_authority_fence(&rx, &mut batch, &task);
    // Acknowledge first-contact protection, then fail the intent's Prepared
    // image. This still proves no bind escapes failed intent durability.
    let mut cache = SqlitePersistConnectionCache::new();
    let guarded = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &guarded,
        &mut batch,
    )
    .unwrap();
    receive_naming_authority_fence(&rx, &mut batch, &task);
    batch.fail(PersistFenceError::WriteFailed("disk failure".to_owned()));
    let (claimed, result) = task.join().unwrap();
    assert!(
        result
            .unwrap_err()
            .message
            .contains("persistence is unconfirmed")
    );
    assert!(
        !transport
            .requests()
            .iter()
            .any(|r| r.request["operation"] == "named_root_bind")
    );
    let inner = claimed.state.inner.lock().unwrap();
    assert!(inner.engram_work_source_roots.is_empty());
    assert!(inner.engram_named_root_journal[0].pending.is_some());
}

#[test]
fn source_root_missing_after_a_lost_reply_replays_then_queues_the_exact_cleanup() {
    let label = "binding-lost-then-missing";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    fs::rename(worktree.join(".git"), worktree.join("saved-git-entry")).unwrap();
    let error = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    assert!(error.message.contains("root_invalid end is queued"));
    {
        let inner = claimed.state.inner.lock().unwrap();
        assert!(inner.engram_work_source_roots.is_empty());
        let journal = &inner.engram_named_root_journal[0];
        let end = journal.pending.as_ref().unwrap();
        assert_eq!(end.root, journal.confirmed.as_ref().unwrap().0.root);
        assert_eq!(end.end_reason, Some(EngramNamedRootEndReason::RootInvalid));
    }
    claimed
        .state
        .flush_engram_root_cleanup(&claimed.session_id, Duration::from_secs(1));
    let events = claimed
        .transport
        .requests()
        .into_iter()
        .filter(|request| request.request["operation"] == "named_root_bind")
        .map(|request| request.request)
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 3);
    assert_eq!(
        events[0], events[1],
        "recovery must settle the old intent exactly"
    );
    assert_eq!(events[2]["kind"], "ended");
    assert_eq!(events[2]["end_reason"], "root_invalid");
    for field in ["generation", "workspace_id", "named_at"] {
        assert_eq!(events[0][field], events[2][field]);
    }
    assert!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_named_root_journal[0]
            .pending
            .is_none()
    );
}

#[test]
fn a_source_root_clear_keeps_the_closing_turns_original_named_generation() {
    let label = "binding-clear-turn-provenance";
    let (claimed, _worktree, runtime_token) = named_root_turn(label, true);
    let generation = claimed.record(|record| {
        record
            .engram
            .active_turn_source_root
            .as_ref()
            .unwrap()
            .generation
    });
    name_root(&claimed, label, None, vec![claimed_root_held(label)]).unwrap();
    let observation = finish_claimed_turn(&claimed, &runtime_token);
    assert_eq!(
        observation["source_basis"]["source_root_generation"],
        generation
    );
    assert_eq!(
        observation["source_basis"]["source_root_state"], "named",
        "clearing must not retroactively turn the running turn's capture into an ended-workdir capture"
    );
}

#[test]
fn a_stale_source_root_begin_receipt_uses_fresh_status_before_removing_a_later_name() {
    let label = "binding-replayed-begin";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let named = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let (target, binding, token) = {
        let inner = claimed.state.inner.lock().unwrap();
        let target = AppState::engram_binding_target_for_session_shape_locked(
            &inner,
            &claimed.session_id,
            true,
        )
        .unwrap()
        .unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        (
            target.clone(),
            inner.sessions[index].engram.work_binding.clone(),
            target.routing_token.clone().unwrap(),
        )
    };
    let before = claimed.transport.requests().len();
    claimed
        .state
        .reconcile_engram_begin_root(
            &target,
            &token,
            binding.as_ref(),
            Some(EngramNamedRootState::None),
            Duration::from_secs(1),
        )
        .unwrap();
    let requests = claimed.transport.requests();
    assert_eq!(requests.len(), before + 2);
    assert_eq!(requests[before].request["operation"], "session_status");
    assert_eq!(requests[before + 1].request["operation"], "named_root_read");
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(
        inner.engram_work_source_roots[0].generation, named.generation,
        "a replayed pre-name begin receipt is not current lifecycle evidence"
    );
}

#[test]
fn source_root_reconciliation_persistence_failure_withholds_until_repaired() {
    let label = "binding-reconcile-persist";
    let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let (token, binding, roots) = {
        let inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        (
            inner.sessions[index].engram.routing_token.clone().unwrap(),
            inner.sessions[index].engram.work_binding.clone(),
            inner.engram_work_source_roots.clone(),
        )
    };
    let durable_path = claimed.state.persistence_path.clone();
    let failing_path = claimed.root.join("reconciliation-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    claimed.state.persistence_path = Arc::new(failing_path);
    assert!(
        claimed
            .reconcile_root_readback(
                &claimed.session_id,
                &token,
                binding.as_ref(),
                Some(EngramNamedRootState::None),
                u64::MAX
            )
            .is_err()
    );
    assert_eq!(
        claimed.state.engram_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    assert_eq!(
        claimed.state.inner.lock().unwrap().engram_work_source_roots,
        roots
    );
    claimed.state.persistence_path = durable_path;
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            binding.as_ref(),
            Some(EngramNamedRootState::None),
            u64::MAX,
        )
        .unwrap();
    assert!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_work_source_roots
            .is_empty()
    );
}

#[test]
fn source_root_readback_requires_durable_guards_and_keeps_known_history_idempotent() {
    let claimed = ClaimedRoot::new_scripted("binding-readback-owner", Vec::new());
    prepare_claimed_root_naming(&claimed, "binding-readback-owner");
    let store = claimed_root_store(&claimed);
    let (token, binding) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone().unwrap(),
        )
    });
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            Some(&binding),
            Some(EngramNamedRootState::Unknown),
            u64::MAX,
        )
        .unwrap();
    let initial = engram_work_naming_token(
        &claimed.state.inner.lock().unwrap(),
        &store,
        &binding.work_id,
    );
    assert!(!engram_work_naming_is_current(
        &claimed.state.inner.lock().unwrap(),
        &store,
        &initial
    ));
    let bound = Some(EngramNamedRootState::Bound {
        workspace_id: claimed.root.to_string_lossy().into_owned(),
        generation: 42,
        named_at: "2026-01-01T00:00:00Z".to_owned(),
    });
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            Some(&binding),
            bound.clone(),
            u64::MAX,
        )
        .unwrap();
    let named = engram_work_naming_token(
        &claimed.state.inner.lock().unwrap(),
        &store,
        &binding.work_id,
    );
    assert_eq!(named.revision, 1);
    claimed
        .reconcile_root_readback(&claimed.session_id, &token, Some(&binding), bound, u64::MAX)
        .unwrap();
    assert_eq!(
        engram_work_naming_token(
            &claimed.state.inner.lock().unwrap(),
            &store,
            &binding.work_id
        ),
        named
    );
    let disk = load_state(claimed.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        engram_work_naming_token(&disk, &store, &binding.work_id),
        named
    );
    assert!(engram_work_naming_is_current(&disk, &store, &named));
}

#[test]
fn source_root_control_phases_share_the_remaining_allowance_and_reject_late_replies() {
    let mut remaining = Duration::from_secs(1);
    engram_source_root_control_within(
        &EngramBudgetClock::Real,
        &mut remaining,
        Duration::from_secs(2),
        |budget| {
            assert_eq!(budget, Duration::from_secs(1));
            std::thread::sleep(Duration::from_millis(2));
            Ok(())
        },
    )
    .unwrap();
    assert!(remaining < Duration::from_secs(1));
    let after_first = remaining;
    engram_source_root_control_within(
        &EngramBudgetClock::Real,
        &mut remaining,
        Duration::from_secs(2),
        |budget| {
            assert_eq!(
                budget, after_first,
                "a second phase cannot refresh the allowance"
            );
            Ok(())
        },
    )
    .unwrap();
    let error = engram_source_root_control_within(
        &EngramBudgetClock::Real,
        &mut remaining,
        Duration::from_nanos(1),
        |_| {
            std::thread::sleep(Duration::from_millis(1));
            Ok(())
        },
    )
    .unwrap_err();
    assert!(error.message.contains("after its budget"));
    remaining = Duration::ZERO;
    assert!(
        engram_source_root_control_within::<()>(
            &EngramBudgetClock::Real,
            &mut remaining,
            Duration::from_secs(2),
            |_| { panic!("an exhausted allowance must not start another phase") }
        )
        .is_err()
    );
}

#[test]
fn newer_source_root_readback_cannot_be_stamped_with_the_old_journal_generation() {
    let label = "binding-newer-provenance";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let named = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let basis = engram_execution_source_basis(&worktree).unwrap();
    let foreign = engram_execution_source_basis(&claimed.root).unwrap();
    let before = claimed.state.engram_root_capture(&claimed.session_id);
    assert!(
        before.stamp(Some(foreign)).is_none(),
        "a confirmed root must not label another workspace"
    );
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        inner.sessions[index].engram.named_root = Some(EngramNamedRootState::Bound {
            workspace_id: basis.workspace_id.clone(),
            generation: named.generation as i64 + 1,
            named_at: "2026-09-29T18:00:00Z".to_owned(),
        });
    }
    let after = claimed.state.engram_root_capture(&claimed.session_id);
    assert_ne!(
        before, after,
        "a capture straddling generations is withheld"
    );
    assert_eq!(
        after,
        EngramRootCapture::Unconfirmed,
        "the authoritative generation has no matching local selection yet"
    );
    assert!(after.stamp(Some(basis)).is_none());
}

#[test]
fn source_root_staging_failure_sends_nothing_and_retains_the_same_retry_intent() {
    let label = "binding-staging-failure";
    let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let (binding, mut root) = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        let binding = inner.sessions[index].engram.work_binding.clone().unwrap();
        let mut root = inner.engram_work_source_roots[0].clone();
        root.generation += 1;
        root.named_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        inner.engram_source_root_generation = root.generation;
        (binding, root)
    };
    root.named_by_session = claimed.session_id.clone();
    let event = EngramNamedRootEvent {
        root,
        reporter: claimed.session_id.clone(),
        kind: EngramNamedRootKind::Bound,
        end_reason: None,
    };
    claimed
        .state
        .stage_engram_root_event_locked(
            &mut claimed.state.inner.lock().unwrap(),
            &event,
            Some(&binding),
        )
        .unwrap();
    let binds_before = claimed
        .transport
        .requests()
        .iter()
        .filter(|request| request.request["operation"] == "named_root_bind")
        .count();
    let durable_path = claimed.state.persistence_path.clone();
    let failing_path = claimed.root.join("persistence-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    claimed.state.persistence_path = Arc::new(failing_path);
    let error = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    assert!(
        error.message.contains("persistence is unconfirmed"),
        "{}",
        error.message
    );
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "named_root_bind")
            .count(),
        binds_before
    );
    let intent = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal[0]
        .pending
        .clone()
        .unwrap();
    claimed.state.persistence_path = durable_path;
    let named = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(
        inner.engram_named_root_journal[0]
            .confirmed
            .as_ref()
            .unwrap()
            .0,
        intent
    );
    assert_eq!(named.generation, intent.root.generation);
}

#[test]
fn a_lost_receipt_replayed_after_release_cannot_revive_the_old_generation() {
    let label = "binding-released-replay";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    let (claim, generation) = {
        let inner = claimed.state.inner.lock().unwrap();
        let pending = inner.engram_named_root_journal[0].pending.as_ref().unwrap();
        (pending.root.claim_id.clone(), pending.root.generation)
    };
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .released
        .insert(claim, generation as i64);
    let error = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    assert!(
        error.message.contains("no longer an active named root"),
        "{}",
        error.message
    );
    assert!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_work_source_roots
            .is_empty()
    );
    let next = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    assert!(next.generation > generation);
    let requests = claimed
        .transport
        .requests()
        .into_iter()
        .map(|r| r.request)
        .collect::<Vec<_>>();
    let replay = requests
        .iter()
        .rposition(|r| r["operation"] == "named_root_bind" && r["generation"] == generation)
        .unwrap();
    assert_eq!(
        requests[replay + 1]["operation"],
        "named_root_read",
        "a stale receipt needs covering canonical lifecycle readback"
    );
    assert_eq!(
        requests[replay + 1]["claim_id"],
        requests[replay]["claim_id"]
    );
    assert_eq!(
        requests[replay + 1]["run_id"],
        test_control_work_binding(&format!("turn-observation-{label}"), 1).run_id
    );
}

#[test]
fn source_root_review_delayed_read_cannot_overwrite_a_same_generation_end() {
    let label = "review-same-generation-read";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let (fence, connection, token, binding, earlier) = {
        let inner = claimed.state.inner.lock().unwrap();
        let target = AppState::engram_binding_target_for_session_shape_locked(
            &inner,
            &claimed.session_id,
            true,
        )
        .unwrap()
        .unwrap();
        (
            EngramRootReadFence::capture(&inner),
            target.connection,
            target.routing_token.unwrap(),
            target.work_binding,
            inner.sessions[inner.find_session_index(&claimed.session_id).unwrap()]
                .engram
                .named_root
                .clone(),
        )
    };
    name_root(&claimed, label, None, vec![claimed_root_held(label)]).unwrap();
    let settled = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal
        .clone();
    claimed
        .state
        .reconcile_engram_named_root_with_fence(
            &claimed.session_id,
            &token,
            binding.as_ref(),
            earlier,
            &fence,
            Some(&connection),
        )
        .unwrap();
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(inner.engram_named_root_journal, settled);
    assert!(inner.engram_work_source_roots.is_empty());
    assert!(matches!(
        engram_root_capture_locked(&inner, &claimed.session_id),
        EngramRootCapture::Recorded {
            state: EngramSourceRootState::Ended,
            ..
        }
    ));
}

#[test]
fn source_root_review_known_retirement_preserves_the_admitted_snapshot() {
    let label = "review-admitted-retirement";
    let (claimed, _, _) = named_root_turn(label, true);
    let admitted = claimed.state.engram_turn_root_capture(&claimed.session_id);
    let (token, binding) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone(),
        )
    });
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            binding.as_ref(),
            Some(EngramNamedRootState::None),
            u64::MAX,
        )
        .unwrap();
    assert_eq!(
        claimed.state.engram_turn_root_capture(&claimed.session_id),
        admitted
    );
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_read_failures
        .insert(binding.as_ref().unwrap().claim_id.clone());
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            binding.as_ref(),
            None,
            u64::MAX,
        )
        .expect_err("omission cannot settle retirement when the canonical reader fails");
    assert_eq!(
        claimed.state.engram_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    assert_eq!(
        claimed.state.engram_turn_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed,
        "a failed canonical recovery cannot discharge the admitted work's guard"
    );
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            binding.as_ref(),
            Some(EngramNamedRootState::Unknown),
            u64::MAX,
        )
        .unwrap();
    assert_eq!(
        claimed.state.engram_turn_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
}

#[test]
fn source_root_review_different_active_guard_survives_focus_away() {
    let label = "review-other-active";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let (token, binding) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone(),
        )
    });
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            binding.as_ref(),
            Some(EngramNamedRootState::Bound {
                workspace_id: claimed.root.to_string_lossy().into_owned(),
                generation: 3,
                named_at: chrono::Utc::now().to_rfc3339(),
            }),
            u64::MAX,
        )
        .unwrap();
    let store = claimed_root_store(&claimed);
    let binding = binding.unwrap();
    claimed.record(|record| record.engram.work_binding = None);
    let mut inner = claimed.state.inner.lock().unwrap();
    engram_compact_root_journal(&mut inner);
    let claim = AcceptanceEvaluationSourceClaim {
        work_id: binding.work_id,
        claim_id: binding.claim_id,
        named_generation_at_request: None,
    };
    assert!(acceptance_evaluation_claim_unconfirmed_locked(
        &inner,
        &claimed.session_id,
        &store,
        Some(&claim)
    ));
    let mut event = inner.engram_named_root_journal[0]
        .confirmed
        .as_ref()
        .unwrap()
        .0
        .clone();
    event.root.generation = 4;
    claimed
        .state
        .stage_engram_root_event_locked(&mut inner, &event, None)
        .unwrap();
    assert!(
        inner.engram_named_root_journal[0].obsolete,
        "staging never revives a retired receipt"
    );
    assert_eq!(
        inner.engram_named_root_journal[0].retirement,
        Some(EngramRootRetirement::DifferentActive)
    );
}

#[test]
fn source_root_review_stale_replay_retires_without_a_replacement_name() {
    let label = "review-retired-replay";
    let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    let (token, binding) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone().unwrap(),
        )
    });
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            Some(&binding),
            Some(EngramNamedRootState::None),
            u64::MAX,
        )
        .unwrap();
    let store = claimed_root_store(&claimed);
    let work =
        claimed.record(|record| record.engram.work_binding.as_ref().unwrap().work_id.clone());
    let prepared = engram_work_naming_token(&claimed.state.inner.lock().unwrap(), &store, &work);
    let seed = AcceptanceEvaluationTargetSeed {
        work_ref: format!("w-{label}"),
        mode: AcceptanceEvaluationMode::IndependentSession,
        acceptance_basis: 1,
        evidence_basis: 1,
        criteria_count: 1,
        bindings: Vec::new(),
        supersedes: None,
        store: store.clone(),
        source_fingerprint: None,
        source_root: None,
        source_claim: Some(AcceptanceEvaluationSourceClaim {
            work_id: work.clone(),
            claim_id: "earlier-evaluation-claim".to_owned(),
            named_generation_at_request: None,
        }),
        work_id: Some(work.clone()),
        naming_history: Some(prepared.clone()),
    };
    acceptance_evaluation_spawn_admission_locked(
        &claimed.state.inner.lock().unwrap(),
        &claimed.session_id,
        &seed,
    )
    .expect("the unnamed work initially admits the evaluation");
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    let intent = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal[0]
        .pending
        .clone()
        .unwrap();
    let real_receipt: EngramNamedRootReceipt = serde_json::from_value(
        claimed
            .transport
            .named_roots
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .seen
            .values()
            .next()
            .unwrap()
            .1
            .clone(),
    )
    .unwrap();
    let durable_path = claimed.state.persistence_path.clone();
    let failing_path = claimed.root.join("retirement-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    claimed.state.persistence_path = Arc::new(failing_path);
    claimed
        .state
        .retire_engram_root_replay(&intent, &real_receipt, None)
        .unwrap_err();
    {
        let inner = claimed.state.inner.lock().unwrap();
        assert_eq!(
            inner.engram_named_root_journal[0].pending.as_ref(),
            Some(&intent)
        );
        assert_eq!(
            engram_work_naming_token(&inner, &store, &work),
            prepared,
            "failed retirement must not publish history or clear uncertainty"
        );
    }
    claimed.state.persistence_path = durable_path;
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .released
        .insert(intent.root.claim_id.clone(), intent.root.generation as i64);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    let receipt = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let journal = &inner.engram_named_root_journal[0];
        assert!(
            journal.obsolete,
            "a real replay receipt does not keep a definitively retired Bound active"
        );
        assert!(journal.pending.is_none());
        let receipt = journal.confirmed.as_ref().unwrap().1.clone();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        inner.sessions[index].engram.work_binding = None;
        engram_compact_root_journal(&mut inner);
        assert!(
            inner.engram_named_root_journal.is_empty(),
            "unreferenced retired history must not consume a slot"
        );
        assert_eq!(inner.engram_source_root_generation, intent.root.generation);
        assert!(
            !engram_work_naming_is_current(&inner, &store, &prepared),
            "learning the real retired receipt must invalidate the older preparation after compaction"
        );
        assert_eq!(
            engram_work_naming_token(&inner, &store, &work).revision,
            prepared.revision + 1
        );
        let refused =
            acceptance_evaluation_spawn_admission_locked(&inner, &claimed.session_id, &seed)
                .expect_err("retirement and compaction cannot admit the original evaluation");
        assert!(refused.message.contains("naming history"), "{refused:?}");
        let target = seed.clone().into_target("earlier-attempt".to_owned(), &claimed.session_id);
        assert!(
            acceptance_evaluation_root_changed_locked(&inner, &target),
            "the first-submission fence must also reject the original target"
        );
        receipt
    };
    claimed
        .state
        .retire_engram_root_replay(&intent, &receipt, None)
        .unwrap_err();
    assert!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_named_root_journal
            .is_empty(),
        "a late callback cannot recreate reclaimed history"
    );
}

#[test]
fn source_root_review_obsolete_main_checkout_cannot_stamp_an_absent_projection() {
    let label = "review-obsolete-checkout";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    add_claimed_root_worktree(&claimed.root);
    let named = name_root(
        &claimed,
        label,
        Some(&claimed.root),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let (token, binding) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone(),
        )
    });
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            binding.as_ref(),
            Some(EngramNamedRootState::UnboundByRelease {
                last_generation: named.generation as i64,
                released_at_position: 100,
            }),
            u64::MAX,
        )
        .unwrap();
    assert_eq!(
        claimed.state.engram_root_capture(&claimed.session_id),
        EngramRootCapture::Unnamed
    );
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_read_failures
        .insert(binding.as_ref().unwrap().claim_id.clone());
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            binding.as_ref(),
            None,
            u64::MAX,
        )
        .expect_err("omission needs canonical recovery even in the main checkout");
    let capture = claimed.state.engram_root_capture(&claimed.session_id);
    assert_eq!(
        capture,
        EngramRootCapture::Unconfirmed,
        "failed recovery cannot revive retired authority even in the main checkout"
    );
    assert!(
        capture
            .stamp(engram_execution_source_basis(&claimed.root))
            .is_none()
    );
}

#[test]
fn source_root_review_reconciliation_survives_focus_away_and_restore() {
    let label = "review-reconciliation-focus";
    let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    let intent = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal[0]
        .pending
        .clone()
        .unwrap();
    replace_naming_session(&mut claimed, label);
    let (token, binding) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone(),
        )
    });
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            binding.as_ref(),
            Some(EngramNamedRootState::Bound {
                workspace_id: intent.root.root.clone(),
                generation: intent.root.generation as i64,
                named_at: intent.root.named_at.clone(),
            }),
            u64::MAX,
        )
        .unwrap();
    let source_claim = AcceptanceEvaluationSourceClaim {
        work_id: intent.root.work_id.clone(),
        claim_id: intent.root.claim_id.clone(),
        named_generation_at_request: None,
    };
    let encoded = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        inner.sessions[index].engram.work_binding =
            Some(test_control_work_binding("another-focused-work", 1));
        engram_compact_root_journal(&mut inner);
        assert!(
            acceptance_evaluation_claim_unconfirmed_locked(
                &inner,
                &claimed.session_id,
                &intent.root.store,
                Some(&source_claim)
            ),
            "compaction cannot remove the requested claim's recovery refusal"
        );
        assert!(inner.engram_named_root_journal[0].reconciliation.is_some());
        serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap()
    };
    let restored = serde_json::from_slice::<PersistedState>(&encoded)
        .unwrap()
        .into_inner()
        .unwrap();
    assert!(acceptance_evaluation_claim_unconfirmed_locked(
        &restored,
        &claimed.session_id,
        &intent.root.store,
        Some(&source_claim)
    ));
}

#[test]
fn source_root_review_live_removal_never_reports_a_restore_end() {
    let label = "review-live-removal";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let mut inner = claimed.state.inner.lock().unwrap();
    let index = inner.find_session_index(&claimed.session_id).unwrap();
    inner.remove_session_at(index);
    assert!(inner.engram_work_source_roots.is_empty());
    assert!(
        inner
            .engram_named_root_journal
            .iter()
            .all(|journal| journal
                .pending
                .as_ref()
                .is_none_or(|event| event.end_reason
                    != Some(EngramNamedRootEndReason::SessionGoneAtRestore))),
        "live removal is not an authoritative restore observation"
    );
    assert!(
        !inner.engram_named_root_journal.is_empty(),
        "retain remote history without inventing an end"
    );
}

#[test]
fn source_root_review_successive_claim_displacements_do_not_fill_the_journal() {
    let label = "review-successive-claims";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    claimed_root_store(&claimed);
    let mut last_generation = 0;
    for occurrence in 0..=ENGRAM_NAMED_ROOT_JOURNAL_LIMIT {
        let mut held = claimed_root_held(label);
        let mut binding = held.items[0].control_binding.clone().unwrap();
        binding.claim_id = format!("successive-claim-{occurrence}");
        binding.run_id = format!("successive-run-{occurrence}");
        binding.root_execution_id = format!("successive-execution-{occurrence}");
        held.items[0].claim_id = binding.claim_id.clone();
        held.items[0].control_binding = Some(binding.clone());
        claimed.record(|record| {
            record.engram.work_binding = Some(binding.clone());
            record.engram.named_root = None;
        });
        claimed
            .transport
            .enable_named_roots(&claimed.session_id, &binding);
        claimed.transport.replace_held_claims([Ok(held)]);
        let named = claimed
            .state
            .name_engram_source_root(
                &claimed.session_id,
                EngramSourceRootRequest {
                    work: format!("w-{label}"),
                    path: Some(Some(worktree.to_string_lossy().into_owned())),
                },
            )
            .unwrap_or_else(|error| {
                panic!(
                    "resolved replacement {occurrence} refused: {}",
                    error.message
                )
            });
        assert!(named.generation > last_generation);
        last_generation = named.generation;
    }
    let mut inner = claimed.state.inner.lock().unwrap();
    engram_compact_root_journal(&mut inner);
    assert_eq!(inner.engram_named_root_journal.len(), 1);
    assert_eq!(inner.engram_source_root_generation, last_generation);
    assert_eq!(
        inner.engram_work_source_roots[0].generation,
        last_generation
    );
    assert!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "named_root_bind")
            .all(|request| request.request["kind"] == "bound"),
        "displacement never fabricates old-claim Ended"
    );
}

#[test]
fn source_root_review_other_claim_clear_retires_only_local_authorization() {
    let label = "review-other-claim-clear";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let mut held = claimed_root_held(label);
    let mut binding = held.items[0].control_binding.clone().unwrap();
    binding.claim_id = "new-clear-claim".to_owned();
    binding.run_id = "new-clear-run".to_owned();
    binding.root_execution_id = "new-clear-execution".to_owned();
    held.items[0].claim_id = binding.claim_id.clone();
    held.items[0].control_binding = Some(binding.clone());
    claimed.record(|record| {
        record.engram.work_binding = Some(binding.clone());
        record.engram.named_root = None;
    });
    claimed
        .transport
        .enable_named_roots(&claimed.session_id, &binding);
    claimed.transport.replace_held_claims([Ok(held)]);
    claimed
        .state
        .name_engram_source_root(
            &claimed.session_id,
            EngramSourceRootRequest {
                work: format!("w-{label}"),
                path: None,
            },
        )
        .unwrap();
    let mut inner = claimed.state.inner.lock().unwrap();
    assert!(inner.engram_work_source_roots.is_empty());
    assert!(
        inner
            .engram_named_root_journal
            .iter()
            .all(|journal| journal.obsolete)
    );
    engram_compact_root_journal(&mut inner);
    assert!(inner.engram_named_root_journal.is_empty());
    assert!(
        inner.engram_work_naming_history[0].frontier.is_some(),
        "compaction retains canonical naming history after a different claim clears local authorization"
    );
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "named_root_bind")
            .count(),
        1,
        "new claim may remove local selection but never ends the old claim remotely"
    );
}

#[test]
fn source_root_review_cleanup_assignment_persist_failure_retains_guarded_intent_without_send() {
    let label = "review-cleanup-assignment";
    let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let original = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let root = inner.engram_work_source_roots[0].clone();
        engram_queue_root_cleanup(
            &mut inner.engram_named_root_journal,
            &root,
            EngramNamedRootEndReason::RootInvalid,
        );
        inner.engram_named_root_journal[0].clone()
    };
    let requests = claimed.transport.requests().len();
    let failing_path = claimed.root.join("cleanup-assignment-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    claimed.state.persistence_path = Arc::new(failing_path);
    claimed
        .state
        .flush_engram_root_cleanup(&claimed.session_id, Duration::from_secs(2));
    let inner = claimed.state.inner.lock().unwrap();
    let pending = inner.engram_named_root_journal[0].pending.as_ref().unwrap();
    let original_intent = original.pending.as_ref().unwrap();
    assert_eq!(pending.root, original_intent.root);
    assert_eq!(pending.kind, original_intent.kind);
    assert_eq!(pending.end_reason, original_intent.end_reason);
    assert_eq!(
        serde_json::to_value(pending.request("same-token").unwrap()).unwrap(),
        serde_json::to_value(original_intent.request("same-token").unwrap()).unwrap()
    );
    assert!(engram_authority_work_unresolved(
        &inner,
        &original.store,
        &original.read_binding.as_ref().unwrap().work_id
    ));
    drop(inner);
    assert_eq!(
        claimed.transport.requests().len(),
        requests,
        "nothing sent before assignment durability"
    );
}

#[test]
fn an_orphaned_name_queues_its_exact_end_before_restore_drops_the_path() {
    let label = "binding-orphan";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let encoded = {
        let mut inner = claimed.state.inner.lock().unwrap();
        inner.engram_work_source_roots[0].named_by_session = "missing-session".to_owned();
        serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap()
    };
    let restored = serde_json::from_slice::<PersistedState>(&encoded)
        .unwrap()
        .into_inner()
        .unwrap();
    assert!(restored.engram_work_source_roots.is_empty());
    let journal = &restored.engram_named_root_journal[0];
    let pending = journal.pending.as_ref().unwrap();
    assert_eq!(pending.kind, EngramNamedRootKind::Ended);
    assert_eq!(
        pending.end_reason,
        Some(EngramNamedRootEndReason::SessionGoneAtRestore)
    );
    assert_eq!(pending.root, journal.confirmed.as_ref().unwrap().0.root);
}

#[test]
fn a_lost_named_root_reply_replays_the_persisted_intent_after_restore() {
    let label = "binding-lost-reply";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    let error = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    assert!(error.message.contains("retained"));
    assert_eq!(
        claimed.state.engram_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    let persisted = {
        let inner = claimed.state.inner.lock().unwrap();
        assert!(
            inner.engram_work_source_roots.is_empty(),
            "unknown outcome must not publish a root"
        );
        serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap()
    };
    let restored: PersistedState = serde_json::from_slice(&persisted).unwrap();
    let restored = restored.into_inner().unwrap();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        inner.engram_named_root_journal = restored.engram_named_root_journal;
        inner.engram_source_root_generation = restored.engram_source_root_generation;
    }
    let result = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let requests = claimed
        .transport
        .requests()
        .into_iter()
        .filter(|r| r.request["operation"] == "named_root_bind")
        .map(|r| r.request)
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0], requests[1],
        "retry must retain fence, time, generation and key"
    );
    let roots = claimed.transport.named_roots.lock().unwrap();
    assert_eq!(
        roots.as_ref().unwrap().seen.len(),
        1,
        "one durable remote event"
    );
    assert_eq!(
        result.generation,
        requests[0]["generation"].as_u64().unwrap()
    );
}

#[test]
fn a_named_root_refusal_publishes_no_root_and_allows_a_repaired_request() {
    let label = "binding-refused";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .refuse_next = true;
    let error = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    assert!(error.message.contains("named_root_binding_refused"));
    {
        let inner = claimed.state.inner.lock().unwrap();
        assert!(inner.engram_work_source_roots.is_empty());
        assert!(
            inner
                .engram_named_root_journal
                .iter()
                .all(|j| j.pending.is_none() && j.confirmed.is_none())
        );
    }
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
}

#[test]
fn named_root_readback_distinguishes_release_from_fence_changes() {
    let label = "binding-lifecycle";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let named = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let (mut binding, token, state) = claimed.record(|record| {
        (
            record.engram.work_binding.clone().unwrap(),
            record.engram.routing_token.clone().unwrap(),
            record.engram.named_root.clone(),
        )
    });
    binding.claim_fence += 3;
    claimed.record(|record| record.engram.work_binding = Some(binding.clone()));
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            Some(&binding),
            state.clone(),
            u64::MAX,
        )
        .unwrap();
    assert_eq!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_work_source_roots
            .len(),
        1,
        "handoff/recovery fence advance preserves a bound root"
    );
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            Some(&binding),
            Some(EngramNamedRootState::UnboundByRelease {
                last_generation: named.generation as i64,
                released_at_position: 9,
            }),
            u64::MAX,
        )
        .unwrap();
    assert!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_work_source_roots
            .is_empty()
    );
    // A stale fence is not used to reconstruct the removed local selection.
    claimed
        .reconcile_root_readback(&claimed.session_id, &token, Some(&binding), None, u64::MAX)
        .unwrap();
    assert!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_work_source_roots
            .is_empty()
    );
}

#[test]
fn named_root_response_variants_include_forward_compatible_unknown_and_absence() {
    for state in [
        json!({"state":"none"}),
        json!({"state":"bound","workspace_id":"x","generation":3,"named_at":"time"}),
        json!({"state":"unbound_by_release","last_generation":3,"released_at_position":9}),
        json!({"state":"future"}),
    ] {
        let status: EngramSessionStatusResponse =
            serde_json::from_value(json!({"phase":"ready","named_root":state})).unwrap();
        let begin: EngramTurnBeginReceipt =
            serde_json::from_value(json!({"grant_id":"g","named_root":state})).unwrap();
        let bind: EngramSessionBindingResponse = serde_json::from_value(
            json!({"routing_token":"t","status":{"phase":"ready","named_root":state}}),
        )
        .unwrap();
        assert_eq!(status.named_root, begin.named_root);
        assert_eq!(status.named_root, bind.status.named_root);
    }
    let status: EngramSessionStatusResponse =
        serde_json::from_value(json!({"phase":"ready"})).unwrap();
    assert_eq!(status.named_root, None);
}

#[test]
fn clearing_a_root_records_its_original_identity_and_stamps_workdir_captures_as_ended() {
    let label = "binding-ended-basis";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    name_root(&claimed, label, None, vec![claimed_root_held(label)]).unwrap();
    let events = claimed
        .transport
        .requests()
        .into_iter()
        .filter(|r| r.request["operation"] == "named_root_bind")
        .map(|r| r.request)
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    for field in ["workspace_id", "generation", "named_at", "claim_id"] {
        assert_eq!(events[0][field], events[1][field]);
    }
    assert_eq!(events[1]["end_reason"], "explicit_clear");
    let captured = claimed
        .state
        .engram_root_capture(&claimed.session_id)
        .stamp(engram_execution_source_basis(&claimed.root))
        .unwrap();
    assert_eq!(
        captured.source_root_state,
        Some(EngramSourceRootState::Ended)
    );
    assert_eq!(
        captured.source_root_generation,
        events[0]["generation"].as_i64()
    );
    assert_ne!(
        captured.workspace_id,
        events[0]["workspace_id"].as_str().unwrap()
    );
}

/// The claimed root's project given the store identity a real Engram
/// reports, which names a source root in it.
fn claimed_root_store(claimed: &ClaimedRoot) -> EngramAuthorityStoreKey {
    let store = EngramAuthorityStoreKey {
        database_path: claimed.root.join("engram.db"),
        project_id: "github.com/example/source-root".to_owned(),
    };
    let mut inner = claimed.state.inner.lock().expect("state mutex poisoned");
    for project in &mut inner.projects {
        if let Some(engram) = project.engram.as_mut() {
            engram.authority_store_key = Some(store.clone());
        }
    }
    store
}

#[test]
fn source_root_compatibility_missing_projection_recovers_owner_without_an_absence_shortcut() {
    let label = "compatibility-missing-projection";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    prepare_claimed_root_naming(&claimed, label);
    let store = claimed_root_store(&claimed);
    let (token, binding) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone().unwrap(),
        )
    });
    claimed
        .state
        .reconcile_engram_named_root(&claimed.session_id, &token, Some(&binding), None, u64::MAX)
        .unwrap();
    assert_eq!(
        claimed.state.engram_root_capture(&claimed.session_id),
        EngramRootCapture::Unnamed
    );
    assert!(
        claimed
            .transport
            .requests()
            .iter()
            .any(|r| r.request["operation"] == "named_root_read")
    );
    let inner = claimed.state.inner.lock().unwrap();
    assert!(!engram_authority_work_unresolved(
        &inner,
        &store,
        &binding.work_id
    ));
}

#[test]
fn source_root_compatibility_omission_reads_bound_and_released_history_without_resending() {
    for released in [false, true] {
        let label = if released {
            "compatibility-released"
        } else {
            "compatibility-bound"
        };
        let claimed = ClaimedRoot::new_scripted(label, Vec::new());
        let worktree = add_claimed_root_worktree(&claimed.root);
        let named = name_root(
            &claimed,
            label,
            Some(&worktree),
            vec![claimed_root_held(label)],
        )
        .unwrap();
        let store = claimed_root_store(&claimed);
        let (token, binding) = claimed.record(|r| {
            (
                r.engram.routing_token.clone().unwrap(),
                r.engram.work_binding.clone().unwrap(),
            )
        });
        if released {
            claimed
                .reconcile_root_readback(
                    &claimed.session_id,
                    &token,
                    Some(&binding),
                    Some(EngramNamedRootState::UnboundByRelease {
                        last_generation: named.generation as i64,
                        released_at_position: 100,
                    }),
                    u64::MAX,
                )
                .unwrap();
        }
        let expected = claimed.state.engram_root_capture(&claimed.session_id);
        assert!(if released {
            expected == EngramRootCapture::Unnamed
        } else {
            matches!(
                expected,
                EngramRootCapture::Recorded {
                    state: EngramSourceRootState::Named,
                    ..
                }
            )
        });
        let count = |operation: &str| {
            claimed
                .transport
                .requests()
                .iter()
                .filter(|r| r.request["operation"] == operation)
                .count()
        };
        let reads = count("named_root_read");
        let writes = count("named_root_bind");
        claimed
            .state
            .reconcile_engram_named_root(
                &claimed.session_id,
                &token,
                Some(&binding),
                None,
                u64::MAX,
            )
            .unwrap();
        assert_eq!(
            count("named_root_read"),
            reads + 1,
            "omission requires a new canonical proof"
        );
        assert_eq!(
            count("named_root_bind"),
            writes,
            "recovery must not resend naming"
        );
        assert_eq!(
            claimed.state.engram_root_capture(&claimed.session_id),
            expected
        );
        assert!(!engram_authority_work_unresolved(
            &claimed.state.inner.lock().unwrap(),
            &store,
            &binding.work_id
        ));
    }
}

#[test]
fn source_root_compatibility_claimed_without_store_is_uncertain_from_opening() {
    let label = "compatibility-no-store";
    let grant = "compatibility-no-store-grant";
    let fresh_grant = "compatibility-no-store-fresh-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("compatibility-no-store-token"),
            grant_reply(grant),
            begin_reply(grant),
            checkpoint_reply(grant),
            grant_reply(fresh_grant),
            begin_reply(fresh_grant),
            checkpoint_reply(fresh_grant),
        ],
    );
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
    let prompt = received_prompt(&claimed);
    assert!(
        prompt.contains("Engram store identity is missing"),
        "{prompt}"
    );
    claimed.record(|record| {
        assert_eq!(
            record.engram.active_turn_root_capture,
            Some(EngramRootCapture::Unconfirmed)
        );
        assert!(record.engram.active_turn_start_basis.is_none());
        assert!(
            record
                .engram
                .opening_diagnostic
                .as_ref()
                .is_some_and(|notice| {
                    notice.delivered && notice.reason == EngramOpeningRootReason::UnverifiedStore
                })
        );
        assert!(
            record
                .engram
                .pending_source_root_line
                .as_deref()
                .is_none_or(|line| !line.contains("unconfirmed"))
        );
    });
    let observation = finish_claimed_turn(&claimed, &claimed.runtime_token());
    assert!(observation.get("source_basis").is_none(), "{observation}");
    prepare_confirmed_claimed_opening(&claimed, label);
    claimed.transport.enable_named_roots(
        &claimed.session_id,
        &test_control_work_binding(&format!("turn-observation-{label}"), 1),
    );
    claimed
        .transport
        .work_bindings
        .lock()
        .unwrap()
        .push_back(Ok(Some(test_control_work_binding(
            &format!("turn-observation-{label}"),
            1,
        ))));
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
    let prompt = received_prompt(&claimed);
    assert!(!prompt.contains("unconfirmed"), "{prompt}");
    assert!(claimed.record(|r| r.engram.active_turn_start_basis.is_some()));
    let _ = finish_claimed_turn(&claimed, &claimed.runtime_token());
    let fresh = claimed
        .transport
        .requests()
        .into_iter()
        .find(|request| {
            request.request["operation"] == "turn_checkpoint"
                && request.request["grant_id"] == fresh_grant
        })
        .expect("the fresh grant closes separately")
        .request["observations"][0]
        .clone();
    assert!(fresh.get("source_basis").is_some(), "{fresh}");
}

#[test]
fn source_root_compatibility_legacy_begin_recovers_only_the_later_fresh_turn() {
    let label = "compatibility-legacy-begin";
    let old_grant = "compatibility-legacy-grant";
    let fresh_grant = "compatibility-fresh-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("compatibility-token"),
            grant_reply(old_grant),
            begin_reply(old_grant),
            checkpoint_reply(old_grant),
            grant_reply(fresh_grant),
            begin_reply(fresh_grant),
            checkpoint_reply(fresh_grant),
        ],
    );
    let store = claimed_root_store(&claimed);
    let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
    claimed
        .transport
        .enable_named_roots(&claimed.session_id, &binding);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .omitted_begin_projections
        .insert(old_grant.to_owned());
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
    let prompt = received_prompt(&claimed);
    assert!(
        prompt.contains("begin receipt lacks opening provenance"),
        "{prompt}"
    );
    assert_eq!(
        claimed.state.engram_root_capture(&claimed.session_id),
        EngramRootCapture::Unnamed,
        "the owner recovers, but this is not an opening proof for the old receipt"
    );
    claimed.record(|r| {
        assert_eq!(
            r.engram.active_turn_root_capture,
            Some(EngramRootCapture::Unconfirmed)
        );
        assert!(r.engram.active_turn_start_basis.is_none());
    });
    let first = finish_claimed_turn(&claimed, &claimed.runtime_token());
    assert!(first.get("source_basis").is_none(), "{first}");
    let inner = claimed.state.inner.lock().unwrap();
    assert!(!engram_authority_work_unresolved(
        &inner,
        &store,
        &binding.work_id
    ));
    drop(inner);
    claimed
        .transport
        .work_bindings
        .lock()
        .unwrap()
        .push_back(Ok(Some(binding.clone())));
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
    let prompt = received_prompt(&claimed);
    assert!(!prompt.contains("unconfirmed"), "{prompt}");
    claimed.record(|r| {
        assert!(
            r.engram.active_turn_start_basis.is_some(),
            "a fresh begin has its own proof"
        )
    });
    let _ = finish_claimed_turn(&claimed, &claimed.runtime_token());
    let second = claimed
        .transport
        .requests()
        .into_iter()
        .find(|r| {
            r.request["operation"] == "turn_checkpoint" && r.request["grant_id"] == fresh_grant
        })
        .expect("the fresh grant closes separately")
        .request["observations"][0]
        .clone();
    assert!(second.get("source_basis").is_some(), "{second}");
}

#[test]
fn source_root_compatibility_late_identity_does_not_upgrade_an_uncertain_turn() {
    let label = "compatibility-late-identity";
    let grant = "compatibility-late-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("compatibility-late-token"),
            grant_reply(grant),
            begin_reply(grant),
            checkpoint_reply(grant),
        ],
    );
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
    let _ = received_prompt(&claimed);
    prepare_claimed_root_naming(&claimed, label);
    claimed_root_store(&claimed);
    let (token, binding) = claimed.record(|r| {
        (
            r.engram.routing_token.clone().unwrap(),
            r.engram.work_binding.clone().unwrap(),
        )
    });
    claimed
        .state
        .reconcile_engram_named_root(&claimed.session_id, &token, Some(&binding), None, u64::MAX)
        .unwrap();
    assert_eq!(
        claimed.state.engram_root_capture(&claimed.session_id),
        EngramRootCapture::Unnamed
    );
    assert_eq!(
        claimed.state.engram_turn_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    let observation = finish_claimed_turn(&claimed, &claimed.runtime_token());
    assert!(observation.get("source_basis").is_none(), "{observation}");
}

/// The held-claims list of the claimed root's session: the claim it is bound
/// to by `ClaimedRoot::new`.
fn claimed_root_held(label: &str) -> EngramHeldClaims {
    let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
    EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: binding.work_id.clone(),
            short_ref: format!("w-{label}"),
            claim_id: binding.claim_id.clone(),
            claim_fence: binding.claim_fence,
            focused: true,
            control_binding: Some(binding),
        }],
        omitted: 0,
    }
}

/// One naming request of the claimed root's session for `w-{label}`, with
/// Engram answering its held-claims reads with `reads` only (the last
/// repeats), whatever earlier requests were answered with.
fn name_root(
    claimed: &ClaimedRoot,
    label: &str,
    path: Option<&FsPath>,
    reads: Vec<EngramHeldClaims>,
) -> std::result::Result<EngramSourceRootResponse, ApiError> {
    prepare_claimed_root_naming(claimed, label);
    claimed_root_store(claimed);
    claimed
        .transport
        .replace_held_claims(reads.into_iter().map(Ok));
    claimed.state.name_engram_source_root(
        &claimed.session_id,
        EngramSourceRootRequest {
            work: format!("w-{label}"),
            path: path.map(|path| Some(path.to_string_lossy().into_owned())),
        },
    )
}

/// A complete held-claims list with no claim on it.
fn no_held_claims() -> EngramHeldClaims {
    EngramHeldClaims {
        items: Vec::new(),
        omitted: 0,
    }
}

fn pending_line(claimed: &ClaimedRoot) -> Option<String> {
    claimed.record(|record| record.engram.pending_source_root_line.clone())
}

#[test]
fn a_name_is_refused_without_the_live_claim_to_a_delegated_session_and_on_a_share() {
    let label = "naming-refusals";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);

    let other_work = EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: "work-other".to_owned(),
            short_ref: "w-other".to_owned(),
            claim_id: "claim-other".to_owned(),
            ..Default::default()
        }],
        omitted: 0,
    };
    let error = name_root(&claimed, label, Some(&worktree), vec![other_work.clone()])
        .expect_err("no live claim on the work");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains("holds no live claim"),
        "{}",
        error.message
    );
    assert!(!error.message.contains("left out"), "{}", error.message);

    let truncated = EngramHeldClaims {
        omitted: 3,
        ..other_work
    };
    let error = name_root(&claimed, label, Some(&worktree), vec![truncated])
        .expect_err("the claim is not on the list Engram gave");
    assert!(
        error.message.contains("3 more were left out of its list"),
        "the refusal says the list was cut: {}",
        error.message
    );

    let mut no_claim_id = claimed_root_held(label);
    no_claim_id.items[0].claim_id.clear();
    let error = name_root(&claimed, label, Some(&worktree), vec![no_claim_id])
        .expect_err("an Engram without claim ids cannot scope a name");
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);

    // Named by its id, since a row without a short reference cannot be
    // found by one.
    let mut no_short_ref = claimed_root_held(label);
    no_short_ref.items[0].short_ref.clear();
    let work_id = no_short_ref.items[0].work_id.clone();
    claimed.transport.replace_held_claims([Ok(no_short_ref)]);
    let error = claimed
        .state
        .name_engram_source_root(
            &claimed.session_id,
            EngramSourceRootRequest {
                work: work_id,
                path: Some(Some(worktree.to_string_lossy().into_owned())),
            },
        )
        .expect_err("an entry without a short reference cannot match its evaluations");
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
    assert!(
        error.message.contains("short reference"),
        "{}",
        error.message
    );

    let error = name_root(
        &claimed,
        label,
        Some(FsPath::new("//server/share/wt")),
        vec![claimed_root_held(label)],
    )
    .expect_err("a share is refused");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("network share"), "{}", error.message);
    // So is a verbatim alias of the network redirector, before anything
    // resolves it.
    let error = name_root(
        &claimed,
        label,
        Some(FsPath::new(r"\\?\GLOBALROOT\Device\Mup\server\share\wt")),
        vec![claimed_root_held(label)],
    )
    .expect_err("a redirector alias is refused");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("network share"), "{}", error.message);

    let error = name_root(
        &claimed,
        label,
        Some(FsPath::new("   ")),
        vec![claimed_root_held(label)],
    )
    .expect_err("a blank path is no clear");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(
        error.message.contains("`path` is blank"),
        "{}",
        error.message
    );

    let error = name_root(
        &claimed,
        label,
        Some(FsPath::new(&"x".repeat(MAX_DELEGATION_CWD_CHARS + 1))),
        vec![claimed_root_held(label)],
    )
    .expect_err("an overlong path is refused before it is checked");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("at most"), "{}", error.message);

    // A delegated session by its own link, with no delegation row left.
    claimed.record(|record| {
        record.session.parent_delegation_id = Some("delegation-gone".to_owned());
    });
    let error = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .expect_err("a session linked to a parent names nothing");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains("only a root session"),
        "{}",
        error.message
    );
    claimed.record(|record| record.session.parent_delegation_id = None);

    claimed
        .state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .delegations
        .push(DelegationRecord {
            id: "delegation-naming".to_owned(),
            parent_session_id: "session-parent".to_owned(),
            child_session_id: claimed.session_id.clone(),
            mode: DelegationMode::Worker,
            status: DelegationStatus::Running,
            title: "Worker".to_owned(),
            prompt: "Work.".to_owned(),
            cwd: claimed.root.to_string_lossy().into_owned(),
            agent: Agent::Codex,
            model: None,
            write_policy: DelegationWritePolicy::ReadOnly,
            created_at: stamp_now(),
            started_at: None,
            completed_at: None,
            result: None,
            submitted_review_result: None,
            post_submission_transport_error: None,
            review_result_recovery_probe_attempt: None,
            review_result_recovery_error: None,
            review_result_schema_version: None,
            queued_followup_prompt_id: None,
            review_result_submission_attempt: 0,
            acceptance_evaluation: None,
            attempt: DelegationAttemptState::default(),
        });
    let error = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .expect_err("a delegated session names nothing");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains("only a root session"),
        "{}",
        error.message
    );
    assert!(
        claimed
            .state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .engram_work_source_roots
            .is_empty(),
        "no refusal left an entry"
    );
}

#[test]
fn the_same_root_named_again_keeps_its_generation_and_a_name_after_a_clear_gets_a_new_one() {
    let label = "naming-generations";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let held = || vec![claimed_root_held(label)];

    let first = name_root(&claimed, label, Some(&worktree), held()).expect("the first name");
    let store = claimed_root_store(&claimed);
    let requested_on = {
        let inner = claimed.state.inner.lock().expect("state mutex poisoned");
        AcceptanceEvaluationSourceRoot::from_entry(&inner.engram_work_source_roots[0])
    };
    let again = name_root(&claimed, label, Some(&worktree), held()).expect("the same name again");
    assert_eq!(again.generation, first.generation, "nothing changed");
    assert_eq!(again.sealed, None);
    {
        let inner = claimed.state.inner.lock().expect("state mutex poisoned");
        assert!(
            requested_on.still_named(&inner.engram_work_source_roots, &store),
            "an evaluation requested on the root is not refused for a repeated name"
        );
    }

    let cleared = name_root(&claimed, label, None, held()).expect("a clear");
    assert_eq!(cleared.generation, 0);
    assert_eq!(cleared.root, None);
    assert_eq!(
        cleared.sealed, None,
        "no turn runs in the old root, so the clear reports no seal"
    );
    let renamed = name_root(&claimed, label, Some(&worktree), held()).expect("named again");
    assert!(
        renamed.generation > first.generation,
        "a name after a clear is a new one: {} after {}",
        renamed.generation,
        first.generation
    );
    let inner = claimed.state.inner.lock().expect("state mutex poisoned");
    assert!(
        !requested_on.still_named(&inner.engram_work_source_roots, &store),
        "the evaluation requested before the clear is refused"
    );
}

#[test]
fn a_clear_during_a_turn_seals_the_root_that_turn_is_measured_in() {
    let label = "naming-clear-seals";
    let (claimed, worktree, _runtime_token) = named_root_turn(label, true);
    assert!(
        claimed.record(|record| record.engram.active_turn_source_root.is_some()),
        "the turn was admitted with the named root"
    );

    let cleared =
        name_root(&claimed, label, None, vec![claimed_root_held(label)]).expect("a clear");

    let revision = content_revision_of(&worktree);
    assert_eq!(
        cleared.sealed.and_then(|sealed| sealed.source_revision),
        Some(revision.clone())
    );
    assert_eq!(
        claimed.record(|record| {
            record
                .engram
                .active_turn_source_root
                .as_ref()
                .and_then(|turn_root| turn_root.sealed_revision.clone())
        }),
        Some(revision),
        "the running turn keeps its root, sealed at the clear"
    );
    assert!(
        pending_line(&claimed).is_some_and(|line| line.contains("no worktree named")),
        "the next prompt says the workdir measures the work again"
    );
}

#[test]
fn a_clear_seals_no_turn_of_another_claim_measured_in_the_same_worktree() {
    // Two works may name one worktree. A clear of one work's name leaves a
    // turn of the other work's claim in that tree unsealed: that work still
    // names the tree, so a seal would hide its later edits there once the
    // tree is gone.
    let label = "naming-clear-other-claim";
    let (claimed, _worktree, _runtime_token) = named_root_turn(label, true);
    claimed.record(|record| {
        record
            .engram
            .active_turn_source_root
            .as_mut()
            .expect("the turn was admitted with the named root")
            .claim_id = "claim-of-another-work".to_owned();
    });

    let cleared =
        name_root(&claimed, label, None, vec![claimed_root_held(label)]).expect("a clear");

    assert_eq!(cleared.sealed, None, "no turn took a seal");
    assert_eq!(
        claimed.record(|record| {
            record
                .engram
                .active_turn_source_root
                .as_ref()
                .map(|turn_root| turn_root.sealed_revision.clone())
        }),
        Some(None),
        "the other claim's turn keeps its root, unsealed"
    );
}

/// Changes the Engram settings of the claimed root's project.
fn change_engram_settings(state: &AppState, change: impl Fn(&mut EngramProjectSettings)) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    for project in &mut inner.projects {
        if let Some(engram) = project.engram.as_mut() {
            change(engram);
        }
    }
}

#[test]
fn a_name_is_refused_when_engram_control_is_off_or_the_project_changes_meanwhile() {
    let label = "naming-project-fence";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let nothing_named = |claimed: &ClaimedRoot, case: &str| {
        assert!(
            claimed
                .state
                .inner
                .lock()
                .expect("state mutex poisoned")
                .engram_work_source_roots
                .is_empty(),
            "{case}: no entry"
        );
        assert_eq!(pending_line(claimed), None, "{case}: no host line");
    };

    // A name counts only for mediated turns: with Engram control off there
    // is nothing to name.
    change_engram_settings(&claimed.state, |engram| engram.turn_gated_control = false);
    let error = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .expect_err("control is off");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("not enabled"), "{}", error.message);
    nothing_named(&claimed, "control off");
    change_engram_settings(&claimed.state, |engram| engram.turn_gated_control = true);

    // The project changes while the name is being checked: another store,
    // or control turned off. Nothing is named for a store nobody could
    // clear it in.
    let changes: [(&str, fn(&mut EngramProjectSettings)); 2] = [
        ("another store", |engram| {
            if let Some(store) = engram.authority_store_key.as_mut() {
                store.project_id = "github.com/example/another".to_owned();
            }
        }),
        ("control turned off", |engram| {
            engram.turn_gated_control = false
        }),
    ];
    for (case, change) in changes {
        let state = claimed.state.clone();
        TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || change_engram_settings(&state, change)));
        });
        let error = name_root(
            &claimed,
            label,
            Some(&worktree),
            vec![claimed_root_held(label)],
        )
        .expect_err(case);
        assert!(
            TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| hook.borrow().is_none()),
            "{case}: the change landed inside the call"
        );
        assert_eq!(error.status, StatusCode::CONFLICT, "{case}");
        assert!(
            error
                .message
                .contains("changed while the root was being named"),
            "{case}: {}",
            error.message
        );
        nothing_named(&claimed, case);
        change_engram_settings(&claimed.state, |engram| engram.turn_gated_control = true);
    }
}

/// The claimed root's turn record: its grant, and the revision sealed on its
/// root, if any.
fn turn_seal(claimed: &ClaimedRoot) -> (Option<String>, Option<String>) {
    claimed.record(|record| {
        (
            record.engram.active_grant_id.clone(),
            record
                .engram
                .active_turn_source_root
                .as_ref()
                .and_then(|turn_root| turn_root.sealed_revision.clone()),
        )
    })
}

#[test]
fn a_clear_after_the_turn_finished_seals_nothing() {
    // A finished turn keeps its root on the record until the next grant
    // begins, but a clear after it has no running turn to seal.
    let label = "naming-clear-after-finish";
    let (claimed, _worktree, runtime_token) = named_root_turn(label, true);
    let _ = finish_claimed_turn(&claimed, &runtime_token);
    assert!(
        claimed.record(|record| record.engram.active_turn_source_root.is_some()),
        "the finished turn's root is still on the record"
    );

    let cleared =
        name_root(&claimed, label, None, vec![claimed_root_held(label)]).expect("a clear");

    assert_eq!(cleared.sealed, None, "no running turn took a seal");
    assert_eq!(
        turn_seal(&claimed),
        (None, None),
        "the finished turn stays unsealed"
    );
}

#[test]
fn a_clear_seals_no_turn_admitted_after_the_call_began() {
    // A turn admitted while the name is being given measures what it finds:
    // the seal's capture may predate that turn's own start.
    let label = "naming-clear-later-turn";
    let (claimed, _worktree, _runtime_token) = named_root_turn(label, true);
    let (state, session_id) = (claimed.state.clone(), claimed.session_id.clone());
    TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            // The running turn ends and a later one begins, with the same
            // root, after the call's first read.
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .expect("the session exists");
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid")
                .engram
                .active_grant_id = Some("grant-of-a-later-turn".to_owned());
        }));
    });

    let cleared =
        name_root(&claimed, label, None, vec![claimed_root_held(label)]).expect("a clear");

    assert!(
        TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| hook.borrow().is_none()),
        "the later turn began inside the call"
    );
    assert_eq!(cleared.sealed, None, "no turn took a seal");
    assert_eq!(
        turn_seal(&claimed),
        (Some("grant-of-a-later-turn".to_owned()), None),
        "the later turn keeps its root unsealed"
    );
}

#[cfg(windows)]
#[test]
fn a_git_bash_drive_spelling_names_the_worktree() {
    // Git Bash's `pwd` prints `/c/…` for `C:/…`; read as written it would be
    // joined to the workdir.
    let label = "naming-msys-drive";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let canonical = fs::canonicalize(&worktree).expect("the worktree canonicalizes");
    let windows = engram_source_root_display(&canonical.to_string_lossy()).replace('\\', "/");
    let (drive, rest) = windows.split_once(':').expect("a drive path");
    let msys = format!("/{}{rest}", drive.to_ascii_lowercase());

    let named = name_root(
        &claimed,
        label,
        Some(FsPath::new(&msys)),
        vec![claimed_root_held(label)],
    )
    .unwrap_or_else(|error| panic!("{msys} should name the worktree: {}", error.message));

    assert_eq!(
        named
            .root
            .as_deref()
            .map(|root| engram_exact_path_key(FsPath::new(root))),
        Some(engram_exact_path_key(&canonical)),
        "{msys} names the worktree itself"
    );
}

/// Another session of the claimed root's project.
fn other_session(claimed: &ClaimedRoot) -> String {
    let project_id = claimed.record(|record| {
        record
            .session
            .project_id
            .clone()
            .expect("the claimed root has a project")
    });
    create_test_project_session(&claimed.state, Agent::Codex, &project_id, &claimed.root)
}

/// A full list, every entry named by `named_by` for a claim that has ended.
fn fill_list(claimed: &ClaimedRoot, store: &EngramAuthorityStoreKey, named_by: &str) {
    claimed
        .state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .engram_work_source_roots = (0..ENGRAM_WORK_SOURCE_ROOT_LIMIT)
        .map(|index| EngramWorkSourceRoot {
            store: store.clone(),
            work_id: format!("work-ended-{index}"),
            short_ref: format!("w-ended-{index}"),
            claim_id: format!("claim-ended-{index}"),
            claim_fence: 1,
            root: claimed.root.to_string_lossy().into_owned(),
            common_dir_key: "unused".to_owned(),
            named_by_session: named_by.to_owned(),
            named_at: "2026-09-27T00:00:00.000Z".to_owned(),
            generation: 1,
        })
        .collect();
}

#[test]
fn a_full_list_does_not_infer_release_from_a_holders_missing_claims() {
    let label = "naming-reclaim";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let store = claimed_root_store(&claimed);
    let other = other_session(&claimed);
    fill_list(&claimed, &store, &other);
    let error = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label), no_held_claims()],
    )
    .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_work_source_roots
            .len(),
        ENGRAM_WORK_SOURCE_ROOT_LIMIT,
        "a claim may have been handed off; a holder's empty list proves no end"
    );
    assert!(
        !claimed
            .transport
            .requests()
            .iter()
            .any(|r| r.request["operation"] == "named_root_bind")
    );
}

#[test]
fn an_authoritative_end_frees_only_the_matching_claim_for_a_new_name() {
    let label = "naming-reclaim-own";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let store = claimed_root_store(&claimed);
    let other = other_session(&claimed);
    fill_list(&claimed, &store, &other);
    let mut binding = test_control_work_binding("ended", 1);
    binding.work_id = "work-ended-0".to_owned();
    binding.claim_id = "claim-ended-0".to_owned();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&other).unwrap();
        inner.sessions[index].engram.work_binding = Some(binding.clone());
        inner.sessions[index].engram.routing_token = Some("other-token".to_owned());
    }
    claimed
        .reconcile_root_readback(
            &other,
            "other-token",
            Some(&binding),
            Some(EngramNamedRootState::None),
            1,
        )
        .unwrap();
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(
        inner.engram_work_source_roots.len(),
        ENGRAM_WORK_SOURCE_ROOT_LIMIT
    );
    assert_eq!(
        inner
            .engram_work_source_roots
            .iter()
            .filter(|root| root.named_by_session == other)
            .count(),
        ENGRAM_WORK_SOURCE_ROOT_LIMIT - 1
    );
}

#[test]
fn source_root_journal_keeps_cached_evidence_until_its_last_reference_ends() {
    let label = "journal-cached-evidence";
    let (claimed, _, token) = named_root_turn(label, true);
    let observation = finish_claimed_turn(&claimed, &token);
    assert!(observation.get("source_basis").is_some());
    name_root(&claimed, label, None, vec![claimed_root_held(label)]).unwrap();
    let mut inner = claimed.state.inner.lock().unwrap();
    let index = inner.find_session_index(&claimed.session_id).unwrap();
    let runtime = &mut inner.sessions[index].engram;
    runtime.work_binding = None;
    runtime.active_turn_source_root = None;
    runtime.active_turn_start_basis = None;
    runtime.active_turn_root_capture = None;
    assert!(runtime.active_turn_report.is_some());
    engram_compact_root_journal(&mut inner);
    assert_eq!(
        inner.engram_named_root_journal.len(),
        1,
        "cached evidence protects its ended generation"
    );
    inner.sessions[index].engram.active_turn_report = None;
    inner.sessions[index].engram.active_turn_report_fallback = None;
    engram_compact_root_journal(&mut inner);
    assert!(inner.engram_named_root_journal.is_empty());
}

#[test]
fn source_root_journal_reclaims_released_history_only_after_authoritative_readback() {
    let label = "journal-released-history";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let named = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let (token, binding) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone(),
        )
    });
    // Removing a selection alone must not claim that its event ended.
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        inner.engram_work_source_roots.clear();
        engram_compact_root_journal(&mut inner);
        assert!(!inner.engram_named_root_journal[0].obsolete);
    }
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            binding.as_ref(),
            Some(EngramNamedRootState::UnboundByRelease {
                last_generation: named.generation as i64,
                released_at_position: 100,
            }),
            u64::MAX,
        )
        .unwrap();
    let mut inner = claimed.state.inner.lock().unwrap();
    assert!(inner.engram_named_root_journal[0].obsolete);
    let index = inner.find_session_index(&claimed.session_id).unwrap();
    inner.sessions[index].engram.work_binding = None;
    engram_compact_root_journal(&mut inner);
    assert!(inner.engram_named_root_journal.is_empty());
    assert_eq!(inner.engram_source_root_generation, named.generation);
}

#[test]
fn source_root_journal_compacts_settled_history_without_losing_generation_on_restore() {
    let label = "journal-compaction";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let mut inner = claimed.state.inner.lock().unwrap();
    let (template, receipt) = inner.engram_named_root_journal[0]
        .confirmed
        .clone()
        .unwrap();
    for i in 0..(ENGRAM_NAMED_ROOT_JOURNAL_LIMIT + 8) {
        let mut event = template.clone();
        event.root.claim_id = format!("settled-claim-{i}");
        event.root.work_id = format!("settled-work-{i}");
        event.root.generation = i as u64 + 2;
        event.kind = EngramNamedRootKind::Ended;
        event.end_reason = Some(EngramNamedRootEndReason::ExplicitClear);
        inner.engram_source_root_generation = event.root.generation;
        let mut binding = inner.engram_named_root_journal[0]
            .read_binding
            .clone()
            .unwrap();
        binding.work_id = event.root.work_id.clone();
        binding.claim_id = event.root.claim_id.clone();
        binding.run_id = format!("settled-run-{i}");
        binding.root_execution_id = format!("settled-execution-{i}");
        claimed
            .state
            .stage_engram_root_event_locked(&mut inner, &event, Some(&binding))
            .unwrap();
        let journal = inner.engram_named_root_journal.last_mut().unwrap();
        let mut ended_receipt = receipt.clone();
        ended_receipt.generation = event.root.generation as i64;
        ended_receipt.kind = EngramNamedRootKind::Ended;
        journal.confirmed = Some((event, ended_receipt));
        journal.pending = None;
    }
    engram_compact_root_journal(&mut inner);
    assert_eq!(
        inner.engram_named_root_journal.len(),
        1,
        "active name survives unlimited settled history"
    );
    assert_eq!(
        inner.engram_named_root_journal[0]
            .confirmed
            .as_ref()
            .unwrap()
            .0,
        template
    );
    let high_water = inner.engram_source_root_generation;
    claimed.state.persist_internal_locked(&inner).unwrap();
    drop(inner);
    // The naming path fences the same metadata snapshot before returning.
    name_root(&claimed, label, None, vec![claimed_root_held(label)]).unwrap();
    let restored = load_state(claimed.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(restored.engram_source_root_generation, high_water);
    assert_eq!(restored.engram_named_root_journal.len(), 1);
    let renamed = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    assert!(
        renamed.generation > high_water,
        "compaction cannot reuse a retired generation"
    );
}

#[test]
fn source_root_journal_full_of_pending_intents_refuses_without_discarding_them() {
    let label = "naming-journal-full";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let template = inner.engram_named_root_journal[0]
            .confirmed
            .as_ref()
            .unwrap()
            .0
            .clone();
        inner.engram_work_source_roots.clear();
        inner.engram_named_root_journal = (0..ENGRAM_NAMED_ROOT_JOURNAL_LIMIT)
            .map(|i| {
                let mut event = template.clone();
                event.root.claim_id = format!("claim-{i}");
                EngramNamedRootJournal {
                    store: event.root.store.clone(),
                    claim_id: event.root.claim_id.clone(),
                    read_binding: None,
                    confirmed: None,
                    pending: Some(event),
                    obsolete: false,
                    retirement: None,
                    reconciliation: None,
                }
            })
            .collect();
    }
    let before = claimed
        .transport
        .requests()
        .iter()
        .filter(|r| r.request["operation"] == "named_root_bind")
        .count();
    let error = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    assert!(error.message.contains("journal is full"));
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(
        inner.engram_named_root_journal.len(),
        ENGRAM_NAMED_ROOT_JOURNAL_LIMIT
    );
    assert!(
        inner
            .engram_named_root_journal
            .iter()
            .all(|journal| journal.pending.is_some())
    );
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|r| r.request["operation"] == "named_root_bind")
            .count(),
        before
    );
}

#[test]
fn a_bound_claim_tells_the_agent_where_its_turns_are_measured_in_its_first_prompt() {
    let label = "naming-bind-line";
    let grant_id = "turn-observation-naming-bind-line-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("turn-observation-naming-bind-line-token"),
            grant_reply(grant_id),
            begin_reply(grant_id),
            checkpoint_reply(grant_id),
        ],
    );

    prepare_confirmed_claimed_opening(&claimed, label);
    deliver_turn_dispatch(&claimed.state, claimed.dispatch())
        .expect("the begun root turn should reach the runtime");

    let prompt = received_prompt(&claimed);
    let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
    assert!(
        prompt.contains(&format!(
            "Engram source basis for work {}:",
            binding.work_id
        )) && prompt.contains("no worktree named"),
        "the bind's line, naming the work by its id with no name given: {prompt}"
    );
    assert_eq!(
        pending_line(&claimed),
        None,
        "a delivered line is not repeated"
    );
}

#[test]
fn a_line_set_after_the_prompt_was_built_waits_for_the_next_prompt() {
    let label = "naming-late-line";
    let grant_id = "turn-observation-naming-late-line-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("turn-observation-naming-late-line-token"),
            grant_reply(grant_id),
            begin_reply(grant_id),
            checkpoint_reply(grant_id),
        ],
    );
    prepare_confirmed_claimed_opening(&claimed, label);
    let dispatch = claimed.dispatch();
    // Set between the prompt's build and its delivery, as a rebind during
    // the begin's recovery sets one.
    claimed.record(|record| {
        record
            .engram
            .set_pending_source_root_line("[TermAl] a later line".to_owned());
    });

    deliver_turn_dispatch(&claimed.state, dispatch)
        .expect("the begun root turn should reach the runtime");

    let prompt = received_prompt(&claimed);
    assert!(!prompt.contains("a later line"), "{prompt}");
    assert_eq!(
        pending_line(&claimed).as_deref(),
        Some("[TermAl] a later line"),
        "the later line is kept for the next prompt, not lost"
    );
}

#[test]
fn a_line_the_prompt_carried_given_again_after_its_build_stays_the_last_line() {
    // The prompt carries a name; before the runtime accepts it the name is
    // cleared and given again. The acceptance must not take the name given
    // again, or the clear would end the next prompt's lines.
    let label = "naming-line-again";
    let grant_id = "turn-observation-naming-line-again-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("turn-observation-naming-line-again-token"),
            grant_reply(grant_id),
            begin_reply(grant_id),
            checkpoint_reply(grant_id),
        ],
    );
    prepare_confirmed_claimed_opening(&claimed, label);
    claimed.record(|record| {
        record
            .engram
            .set_pending_source_root_line("[TermAl] named".to_owned());
    });
    let dispatch = claimed.dispatch();
    claimed.record(|record| {
        record
            .engram
            .set_pending_source_root_line("[TermAl] cleared".to_owned());
        record
            .engram
            .set_pending_source_root_line("[TermAl] named".to_owned());
    });

    deliver_turn_dispatch(&claimed.state, dispatch)
        .expect("the begun root turn should reach the runtime");

    let prompt = received_prompt(&claimed);
    assert!(prompt.contains("[TermAl] named"), "{prompt}");
    assert!(!prompt.contains("[TermAl] cleared"), "{prompt}");
    assert_eq!(
        pending_line(&claimed).as_deref(),
        Some("[TermAl] cleared\n[TermAl] named"),
        "the next prompt tells the clear and ends on the name given again"
    );
}

#[test]
fn a_session_that_engram_disables_drops_its_waiting_lines() {
    // Once Engram disables the session, its turns are measured nowhere: a
    // line waiting for the next prompt would tell of measurements that no
    // longer happen. A failure that only delays the session keeps it.
    let label = "naming-disabled-drops-lines";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    claimed.record(|record| {
        record
            .engram
            .set_pending_source_root_line("[TermAl] a waiting line".to_owned());
        record.engram.source_root_line_delivery = Some(("[TermAl] a waiting line".to_owned(), 7));
    });

    claimed.state.record_engram_transport_failure(
        &claimed.session_id,
        &EngramTransportError::deadline("control deadline"),
    );
    assert_eq!(
        pending_line(&claimed).as_deref(),
        Some("[TermAl] a waiting line"),
        "a failure that does not disable the session keeps the line"
    );

    claimed.state.record_engram_transport_failure(
        &claimed.session_id,
        &EngramTransportError::protocol("unparsable control response"),
    );
    assert!(
        claimed.record(|record| record.engram.disabled_reason.is_some()),
        "the protocol failure disabled the session"
    );
    assert_eq!(pending_line(&claimed), None, "the waiting line is dropped");
    assert_eq!(
        claimed.record(|record| record.engram.source_root_line_delivery.clone()),
        None,
        "and so is its delivery in flight"
    );
}

#[test]
fn a_delivered_line_is_cleared_on_a_turn_that_begins_no_grant() {
    let (state, runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-source-root-line-no-grant");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-source-root-line-no-grant-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "No Engram");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let set_line = |line: &str| {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).expect("session");
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram
            .set_pending_source_root_line(line.to_owned());
    };
    set_line("[TermAl] a line for a turn without a grant");

    let dispatch = match state
        .dispatch_turn(
            &session_id,
            SendMessageRequest {
                text: "Hello.".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("the session should dispatch")
    {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("an idle session should dispatch"),
    };
    deliver_turn_dispatch(&state, dispatch).expect("the prompt should reach the runtime");

    match receive(&runtime_rx, "runtime should receive the prompt") {
        CodexRuntimeCommand::Prompt { command, .. } => {
            assert!(command.prompt.contains("a line for a turn without a grant"))
        }
        _ => panic!("expected a prompt"),
    }
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(&session_id).expect("session");
    assert_eq!(
        inner.sessions[index].engram.pending_source_root_line, None,
        "delivered once, not repeated on every prompt"
    );
}

#[test]
fn a_clear_during_the_opening_capture_seals_the_root_that_turn_began_with() {
    opening_capture_keeps_admitted_basis("clear");
}

#[test]
fn a_rename_during_the_opening_capture_keeps_the_root_that_turn_began_with() {
    opening_capture_keeps_admitted_basis("rename");
}

#[test]
fn a_pending_rename_during_the_opening_capture_withholds_without_a_synthetic_change() {
    opening_capture_keeps_admitted_basis("pending");
}

#[test]
fn an_unknown_read_during_the_opening_capture_withholds_without_a_synthetic_change() {
    opening_capture_keeps_admitted_basis("unknown");
}

#[test]
fn source_root_stale_opening_capture_cannot_change_a_delivered_successor() {
    let label = "stale-opening-successor";
    let old_grant = format!("turn-observation-{label}-grant");
    let fresh_grant = format!("turn-observation-{label}-fresh-grant");
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply(&format!("turn-observation-{label}-token")),
            grant_reply(&old_grant),
            begin_reply(&old_grant),
            checkpoint_reply(&old_grant),
            grant_reply(&fresh_grant),
            begin_reply(&fresh_grant),
            checkpoint_reply(&fresh_grant),
        ],
    );
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let saved = Arc::new(Mutex::new(None));
    let (state, session_id, transport, successor) = (
        claimed.state.clone(),
        claimed.session_id.clone(),
        claimed.transport.clone(),
        saved.clone(),
    );
    TEST_ENGRAM_DURING_TURN_START_CAPTURE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let (token, binding, runtime) = {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
                (
                    record.engram.routing_token.clone().unwrap(),
                    record.engram.work_binding.clone().unwrap(),
                    record.runtime.runtime_token().unwrap(),
                )
            };
            state
                .reconcile_engram_named_root(
                    &session_id,
                    &token,
                    Some(&binding),
                    Some(EngramNamedRootState::Unknown),
                    u64::MAX,
                )
                .unwrap();
            transport
                .work_bindings
                .lock()
                .unwrap()
                .push_back(Ok(Some(binding)));
            // The original prompt has not reached the provider. Completion
            // drains that queued prompt through a fresh grant and real handoff
            // while the superseded opening's capture is still suspended.
            state
                .finish_turn_ok_if_runtime_matches(&session_id, &runtime)
                .unwrap();
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
            let root = record.engram.active_turn_source_root.as_ref().unwrap();
            assert!(record.engram.opening_diagnostic.is_none());
            *successor.lock().unwrap() = Some((
                record.active_turn_generation,
                record.engram.active_grant_id.clone(),
                record.engram.active_turn_root_capture.clone(),
                record
                    .engram
                    .active_turn_start_basis
                    .as_ref()
                    .unwrap()
                    .source_revision
                    .clone(),
                root.root.clone(),
                root.generation,
                record.engram.active_turn_naming_identity.clone(),
            ));
        }));
    });
    assert!(matches!(
        deliver_turn_dispatch_now(&claimed.state, claimed.dispatch()),
        TurnDispatchDeliveryOutcome::Superseded
    ));
    assert!(TEST_ENGRAM_DURING_TURN_START_CAPTURE.with(|hook| hook.borrow().is_none()));
    let prompt = received_prompt(&claimed);
    assert!(prompt.contains("Change the root workspace."), "{prompt}");
    assert!(!prompt.contains("unconfirmed"), "{prompt}");
    assert!(
        matches!(
            claimed.runtime_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ),
        "the superseded opening must not reach the provider"
    );
    let actual = claimed.record(|record| {
        let root = record.engram.active_turn_source_root.as_ref().unwrap();
        assert!(record.engram.opening_diagnostic.is_none());
        (
            record.active_turn_generation,
            record.engram.active_grant_id.clone(),
            record.engram.active_turn_root_capture.clone(),
            record
                .engram
                .active_turn_start_basis
                .as_ref()
                .unwrap()
                .source_revision
                .clone(),
            root.root.clone(),
            root.generation,
            record.engram.active_turn_naming_identity.clone(),
        )
    });
    assert_eq!(
        Some(actual),
        *saved.lock().unwrap(),
        "stale finalization cannot alter the delivered successor's opening"
    );
    assert_eq!(
        claimed.record(|record| record.engram.active_grant_id.clone()),
        Some(fresh_grant)
    );
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
        .unwrap();
}

fn opening_capture_keeps_admitted_basis(transition: &str) {
    let label = format!("naming-{transition}-during-capture");
    let label = label.as_str();
    let uncertain = matches!(transition, "pending" | "unknown");
    let grant_id = format!("turn-observation-{label}-grant");
    let fresh_grant = format!("turn-observation-{label}-fresh-grant");
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply(&format!("turn-observation-{label}-token")),
            grant_reply(&grant_id),
            begin_reply(&grant_id),
            checkpoint_reply(&grant_id),
            grant_reply(&fresh_grant),
            begin_reply(&fresh_grant),
            checkpoint_reply(&fresh_grant),
        ],
    );
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .expect("the root is named before the turn");
    let (state, session_id, work) = (
        claimed.state.clone(),
        claimed.session_id.clone(),
        format!("w-{label}"),
    );
    let new_path = matches!(transition, "rename" | "pending")
        .then(|| Some(claimed.root.to_string_lossy().into_owned()));
    let transport = claimed.transport.clone();
    let capture_transition = transition.to_owned();
    // The transition lands after admission and before the opening capture,
    // without changing the admitted workspace's contents.
    TEST_ENGRAM_DURING_TURN_START_CAPTURE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            if capture_transition == "unknown" {
                let (token, binding) = {
                    let inner = state.inner.lock().unwrap();
                    let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
                    (
                        record.engram.routing_token.clone().unwrap(),
                        record.engram.work_binding.clone().unwrap(),
                    )
                };
                state
                    .reconcile_engram_named_root(
                        &session_id,
                        &token,
                        Some(&binding),
                        Some(EngramNamedRootState::Unknown),
                        u64::MAX,
                    )
                    .unwrap();
                return;
            }
            if capture_transition == "pending" {
                transport
                    .named_roots
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .lose_next_reply = true;
            }
            let result = state.name_engram_source_root(
                &session_id,
                EngramSourceRootRequest {
                    work,
                    path: new_path,
                },
            );
            if capture_transition == "pending" {
                result.expect_err("the lost reply keeps the rename pending");
            } else {
                result.expect("a confirmed transition during the opening capture");
            }
        }));
    });

    deliver_turn_dispatch(&claimed.state, claimed.dispatch())
        .expect("the begun root turn should reach the runtime");
    let prompt = received_prompt(&claimed);
    assert_eq!(prompt.contains("unconfirmed"), uncertain, "{prompt}");

    assert!(
        TEST_ENGRAM_DURING_TURN_START_CAPTURE.with(|hook| hook.borrow().is_none()),
        "the clear ran inside the window"
    );
    if uncertain {
        assert!(
            claimed.record(|record| record.engram.active_turn_start_basis.is_none()),
            "pending or Unknown authority withholds the opening basis"
        );
        let admitted = claimed.record(|record| {
            let root = record.engram.active_turn_source_root.as_ref().unwrap();
            (root.root.clone(), root.generation, root.claim_id.clone())
        });
        if transition == "pending" {
            // Retry the exact intent whose reply was lost, through production
            // naming. Authority recovers while the original grant is active.
            name_root(
                &claimed,
                label,
                Some(&claimed.root),
                vec![claimed_root_held(label)],
            )
            .expect("the immutable pending rename recovers");
        } else {
            let (token, binding) = claimed.record(|record| {
                (
                    record.engram.routing_token.clone().unwrap(),
                    record.engram.work_binding.clone().unwrap(),
                )
            });
            claimed
                .state
                .reconcile_engram_named_root(
                    &claimed.session_id,
                    &token,
                    Some(&binding),
                    None,
                    u64::MAX,
                )
                .expect("the covering canonical read recovers Unknown authority");
        }
        assert!(matches!(
            claimed.state.engram_root_capture(&claimed.session_id),
            EngramRootCapture::Recorded { .. }
        ));
        claimed.record(|record| {
            assert_eq!(
                record.engram.active_grant_id.as_deref(),
                Some(grant_id.as_str())
            );
            let root = record.engram.active_turn_source_root.as_ref().unwrap();
            assert_eq!(
                (root.root.clone(), root.generation, root.claim_id.clone()),
                admitted
            );
        });
        let run_check = |key: &str, root: &FsPath| {
            claimed.state.note_engram_command_started(
                &claimed.session_id,
                &EngramObservationProvenance::Ambient,
                key,
                Some("cargo test"),
                Some(root.to_str().unwrap()),
            );
            claimed.state.note_engram_command_finished(
                &claimed.session_id,
                &EngramObservationProvenance::Ambient,
                key,
                "cargo test",
                "test result: ok. 1 passed",
                Some(EngramCommandExit::Code(0)),
            );
            let captures = claimed.record(|record| {
                record
                    .engram
                    .active_turn_checks
                    .iter()
                    .flat_map(|check| {
                        std::iter::once(check.start_basis.clone())
                            .chain(check.end.as_ref().map(|end| end.end_basis.clone()))
                    })
                    .collect::<Vec<_>>()
            });
            for capture in captures {
                capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD);
            }
        };
        run_check("recovered-original-opening", &worktree);
        // Repeating this grant's capture cannot reset either its opening
        // eligibility or its admitted workspace to the recovered selection.
        claimed
            .state
            .record_engram_turn_start_basis_off_lock(&claimed.session_id, &grant_id);
        claimed.record(|record| {
            assert_eq!(
                record.engram.active_turn_root_capture,
                Some(EngramRootCapture::Unconfirmed)
            );
            assert!(record.engram.active_turn_start_basis.is_none());
            let root = record.engram.active_turn_source_root.as_ref().unwrap();
            assert_eq!(
                (root.root.clone(), root.generation, root.claim_id.clone()),
                admitted
            );
        });
        claimed
            .state
            .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
            .unwrap();
        let first = claimed
            .transport
            .requests()
            .into_iter()
            .find(|request| {
                request.request["operation"] == "turn_checkpoint"
                    && request.request["grant_id"] == grant_id
            })
            .unwrap()
            .request;
        assert!(
            first
                .get("verification_evidence")
                .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty)),
            "recovery cannot credit a check of the same uncertain opening: {first}"
        );
        assert!(
            first["observations"]
                .as_array()
                .unwrap()
                .iter()
                .all(|observation| observation.get("source_basis").is_none_or(Value::is_null)),
            "{first}"
        );

        // A new begin takes its own confirmed opening, rather than upgrading
        // the old one. Both its actual prompt and recognised check agree.
        let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
        claimed
            .transport
            .work_bindings
            .lock()
            .unwrap()
            .push_back(Ok(Some(binding)));
        deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
        let prompt = received_prompt(&claimed);
        assert!(!prompt.contains("unconfirmed"), "{prompt}");
        assert!(claimed.record(|record| record.engram.active_turn_start_basis.is_some()));
        let fresh_root = if transition == "pending" {
            &claimed.root
        } else {
            &worktree
        };
        run_check("confirmed-fresh-opening", fresh_root);
        claimed
            .state
            .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
            .unwrap();
        let fresh = claimed
            .transport
            .requests()
            .into_iter()
            .find(|request| {
                request.request["operation"] == "turn_checkpoint"
                    && request.request["grant_id"] == fresh_grant
            })
            .unwrap()
            .request;
        assert_eq!(
            fresh["verification_evidence"].as_array().map(Vec::len),
            Some(1),
            "{fresh}"
        );
        assert!(
            fresh["observations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|observation| observation
                    .get("source_basis")
                    .is_some_and(|basis| !basis.is_null())),
            "{fresh}"
        );
        return;
    }
    assert_eq!(
        claimed.record(|record| {
            record
                .engram
                .active_turn_source_root
                .as_ref()
                .and_then(|turn_root| turn_root.sealed_revision.clone())
        }),
        Some(content_revision_of(&worktree)),
        "the turn keeps the root it began with, sealed at the clear"
    );
    let start = claimed
        .record(|record| record.engram.active_turn_start_basis.clone())
        .expect("a confirmed clear preserves the admitted opening basis");
    assert_eq!(start.source_revision, content_revision_of(&worktree));
    assert_eq!(start.source_root_state, Some(EngramSourceRootState::Named));
    let observation = finish_claimed_turn(&claimed, &claimed.runtime_token());
    assert_eq!(observation["source_changed"], false, "{observation}");
    assert_eq!(observation["effect"], "observe", "{observation}");
    assert_eq!(
        observation["source_basis"]["source_root_generation"],
        json!(start.source_root_generation),
        "closing evidence keeps the opening generation"
    );
}

#[test]
fn a_watcher_event_in_a_named_root_beside_the_workdir_counts_while_the_grant_is_held() {
    let label = "naming-watcher-sibling";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let canonical = fs::canonicalize(&worktree).expect("the worktree canonicalizes");
    // The session works in a directory beside the named root, not above it.
    let workdir = claimed.root.join("beside");
    fs::create_dir_all(&workdir).expect("the workdir exists");
    claimed.record(|record| {
        record.session.workdir = engram_source_root_display(
            &fs::canonicalize(&workdir)
                .expect("the workdir canonicalizes")
                .to_string_lossy(),
        );
        record.active_turn_start_message_count = Some(record.session.messages.len());
        record.engram.active_grant_id = Some("grant-watched".to_owned());
        record.engram.active_turn_source_root = Some(EngramTurnSourceRoot {
            root: canonical.to_string_lossy().into_owned(),
            common_dir_key: "unused".to_owned(),
            short_ref: format!("w-{label}"),
            claim_id: "claim-watched".to_owned(),
            generation: 0,
            sealed_revision: None,
        });
    });
    let in_root = engram_source_root_display(&canonical.join("README.md").to_string_lossy());
    let elsewhere = engram_source_root_display(
        &fs::canonicalize(&claimed.root)
            .expect("the root canonicalizes")
            .join("elsewhere.txt")
            .to_string_lossy(),
    );
    let events = |paths: &[&str]| {
        paths
            .iter()
            .map(|path| WorkspaceFileChangeEvent {
                path: (*path).to_owned(),
                kind: WorkspaceFileChangeKind::Modified,
                root_path: None,
                session_id: None,
                mtime_ms: None,
                size_bytes: None,
            })
            .collect::<Vec<_>>()
    };

    claimed
        .state
        .record_active_turn_file_changes(&events(&[&in_root, &elsewhere]));
    assert_eq!(
        claimed.record(|record| {
            record
                .active_turn_file_changes
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        }),
        [in_root.clone()],
        "the turn's root counts; a path outside it and the workdir does not"
    );

    claimed.record(|record| {
        record.active_turn_file_changes.clear();
        record.engram.active_grant_id = None;
    });
    claimed
        .state
        .record_active_turn_file_changes(&events(&[&in_root]));
    assert!(
        claimed.record(|record| record.active_turn_file_changes.is_empty()),
        "a root kept from an earlier grant counts for nothing"
    );
}

#[test]
fn the_naming_route_answers_with_the_handlers_statuses() {
    let label = "naming-route";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    prepare_claimed_root_naming(&claimed, label);
    let worktree = add_claimed_root_worktree(&claimed.root);
    claimed_root_store(&claimed);
    claimed
        .transport
        .replace_held_claims([Ok(claimed_root_held(label))]);
    let app = app_router(claimed.state.clone());
    let post = |session: &str, body: Value| {
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session}/engram-source-root"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&body).expect("the body serializes"),
            ))
            .expect("the request builds")
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a test runtime");

    let (status, named): (StatusCode, Value) = runtime.block_on(request_json(
        &app,
        post(
            &claimed.session_id,
            json!({ "work": format!("w-{label}"), "path": worktree.to_string_lossy() }),
        ),
    ));
    assert_eq!(status, StatusCode::OK, "{named:#}");
    assert_eq!(named["workRef"], format!("w-{label}"));
    assert!(named["root"].is_string(), "{named:#}");

    let unknown = runtime.block_on(request_response(
        &app,
        post(&claimed.session_id, json!({ "work": "w-x", "bogus": 1 })),
    ));
    assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let missing = runtime.block_on(request_response(
        &app,
        post("session-missing", json!({ "work": "w-x" })),
    ));
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[test]
fn a_watcher_event_in_the_named_root_counts_when_another_sessions_scope_carries_it() {
    // The named root lies under another session's workdir, so the watcher
    // routes its events to that session's scope and marks the unscoped copy
    // as a duplicate; the turn measured in the root still counts them.
    let label = "naming-watcher-scoped";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let canonical = fs::canonicalize(&worktree).expect("the worktree canonicalizes");
    let project_root = fs::canonicalize(&claimed.root).expect("the root canonicalizes");
    let other = other_session(&claimed);
    let workdir = claimed.root.join("beside");
    fs::create_dir_all(&workdir).expect("the workdir exists");
    claimed.record(|record| {
        record.session.workdir = engram_source_root_display(
            &fs::canonicalize(&workdir)
                .expect("the workdir canonicalizes")
                .to_string_lossy(),
        );
        record.active_turn_start_message_count = Some(record.session.messages.len());
        record.engram.active_grant_id = Some("grant-watched".to_owned());
        record.engram.active_turn_source_root = Some(EngramTurnSourceRoot {
            root: canonical.to_string_lossy().into_owned(),
            common_dir_key: "unused".to_owned(),
            short_ref: format!("w-{label}"),
            claim_id: "claim-watched".to_owned(),
            generation: 0,
            sealed_revision: None,
        });
    });
    let scope_root = PathBuf::from(engram_source_root_display(&project_root.to_string_lossy()));
    // The other session works in the same tree and has a turn running too.
    {
        let mut inner = claimed.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&other)
            .expect("the other session exists");
        let record = &mut inner.sessions[index];
        record.session.workdir = scope_root.to_string_lossy().into_owned();
        record.active_turn_start_message_count = Some(record.session.messages.len());
    }
    let scopes = [
        WorkspaceFileWatchScope {
            root_path: scope_root.clone(),
            session_id: None,
        },
        WorkspaceFileWatchScope {
            root_path: scope_root,
            session_id: Some(other.clone()),
        },
    ];
    let edited = PathBuf::from(engram_source_root_display(
        &canonical.join("README.md").to_string_lossy(),
    ));
    let events =
        workspace_file_changes_from_path(&edited, WorkspaceFileChangeKind::Modified, &scopes);
    assert!(
        events
            .iter()
            .all(|event| event.session_id.as_deref() != Some(claimed.session_id.as_str())),
        "no copy is routed to the session measured in the root: {:?}",
        events
            .iter()
            .map(|event| event.session_id.clone())
            .collect::<Vec<_>>()
    );

    claimed.state.record_active_turn_file_changes(&events);

    assert_eq!(
        claimed.record(|record| {
            record
                .active_turn_file_changes
                .iter()
                .map(|(path, kind)| (path.clone(), *kind))
                .collect::<Vec<_>>()
        }),
        [(
            edited.to_string_lossy().into_owned(),
            WorkspaceFileChangeKind::Modified
        )],
        "counted once, whichever scope carried it"
    );
    // The watcher cannot tell who made the edit, so the session whose scope
    // carried it counts it as well: the edit is attributed to both.
    let other_paths = {
        let inner = claimed.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&other)
            .expect("the other session exists");
        inner.sessions[index]
            .active_turn_file_changes
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(other_paths, [edited.to_string_lossy().into_owned()]);
    // The set is the turn's "files changed" summary, so the card lists it.
    let card = claimed.record(|record| {
        assert!(push_active_turn_file_changes_on_record(
            record,
            "message-root-card".to_owned()
        ));
        match record.session.messages.last() {
            Some(Message::FileChanges { files, .. }) => files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>(),
            _ => panic!("the turn's file-change card should be the last message"),
        }
    });
    assert_eq!(card, [edited.to_string_lossy().into_owned()]);
}

#[test]
fn a_turn_admitted_with_a_named_root_marks_a_check_open_in_that_root() {
    // The turn-start sweep runs before the root is known; the admission that
    // installs it marks another session's check still open in that root.
    let label = "naming-admission-overlap";
    let grant_id = "turn-observation-naming-admission-overlap-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("turn-observation-naming-admission-overlap-token"),
            grant_reply(grant_id),
            begin_reply(grant_id),
            checkpoint_reply(grant_id),
        ],
    );
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .expect("the root is named before the turn");
    let other = other_session(&claimed);
    let pending = Arc::new(EngramBasisCapture::default());
    let root = fs::canonicalize(&worktree).expect("the worktree canonicalizes");
    {
        let mut inner = claimed.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("the other session");
        let mut check = super::super::turn_checks::finished_check(0, pending.clone());
        check.target.root = root.clone();
        check.target.directory = root;
        inner.sessions[index].engram.active_turn_checks = vec![check];
    }
    let overlapped = || {
        let inner = claimed.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("the other session");
        inner.sessions[index].engram.active_turn_checks[0].overlapped
    };

    let dispatch = claimed.dispatch();
    assert!(
        !overlapped(),
        "the turn-start sweep sees only the workdir, a worktree of its own"
    );
    deliver_turn_dispatch(&claimed.state, dispatch)
        .expect("the begun root turn should reach the runtime");
    let _ = received_prompt(&claimed);

    assert!(
        overlapped(),
        "the admission that installs the root marks the check"
    );
    pending.finish(None);
}

#[test]
fn a_delegated_session_is_not_told_to_name_a_root_it_cannot_name() {
    let child_bound = test_control_work_binding("naming-child-line", 1);
    assert_eq!(
        engram_bind_source_root_line(true, &[], None, None, &child_bound, "C:/child", false),
        None,
        "a delegated session gets no bind line"
    );
    assert!(
        engram_bind_source_root_line(false, &[], None, None, &child_bound, "C:/root", false)
            .is_some_and(|line| line.contains("termal_name_source_root")),
        "a root session is told how to name one"
    );
    let child_line = engram_source_root_withheld_line(
        FsPath::new("C:/elsewhere"),
        "C:/child",
        false,
        true,
        None,
    );
    assert!(
        !child_line.contains("termal_name_source_root"),
        "{child_line}"
    );
    assert!(
        child_line.contains("run the tests in your workdir"),
        "{child_line}"
    );
}

#[test]
fn a_name_landing_after_the_held_read_is_not_overwritten() {
    // Another holder names the work between this call's held-claims read and
    // its commit: the call's baseline is the list before the read, so it
    // sees the change and refuses rather than overwrite the newer name.
    let label = "naming-late-other";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let store = claimed_root_store(&claimed);
    let held = claimed_root_held(label);
    let newer = EngramWorkSourceRoot {
        store,
        work_id: held.items[0].work_id.clone(),
        short_ref: format!("w-{label}"),
        claim_id: "claim-newer".to_owned(),
        claim_fence: 2,
        root: claimed.root.to_string_lossy().into_owned(),
        common_dir_key: "unused".to_owned(),
        named_by_session: "session-newer".to_owned(),
        named_at: "2026-09-27T00:00:00.000Z".to_owned(),
        generation: 7,
    };
    let (state, landed) = (claimed.state.clone(), newer.clone());
    TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            state
                .inner
                .lock()
                .expect("state mutex poisoned")
                .engram_work_source_roots
                .push(landed);
        }));
    });

    let error = name_root(&claimed, label, Some(&worktree), vec![held])
        .expect_err("the newer name is not overwritten");

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains("changed while it was being named"),
        "{}",
        error.message
    );
    assert_eq!(
        claimed
            .state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .engram_work_source_roots,
        vec![newer],
        "the newer name stands"
    );
}

#[test]
fn an_explicit_null_path_is_refused_on_the_route() {
    let label = "naming-route-null";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .expect("a root is named first");
    let app = app_router(claimed.state.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a test runtime");

    let response = runtime.block_on(request_response(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{}/engram-source-root",
                claimed.session_id
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "work": format!("w-{label}"), "path": null }))
                    .expect("the body serializes"),
            ))
            .expect("the request builds"),
    ));

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        claimed
            .state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .engram_work_source_roots
            .len(),
        1,
        "a null path clears nothing"
    );
}

#[test]
fn a_name_given_again_after_a_clear_is_the_last_line_the_agent_reads() {
    // Name, clear, name the same root again before the next prompt: the
    // pending lines end on the name, the state the turn is measured in.
    let label = "naming-name-clear-name";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .expect("named");
    name_root(&claimed, label, None, vec![claimed_root_held(label)]).expect("cleared");
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .expect("named again");

    let pending = pending_line(&claimed).expect("lines wait for the next prompt");
    let last = pending.lines().last().expect("a last line");
    assert!(last.contains("its named source root"), "{pending}");
    assert_eq!(
        pending
            .lines()
            .filter(|line| line.contains("its named source root"))
            .count(),
        1,
        "the name is not repeated: {pending}"
    );
}

#[test]
fn named_claim_fixture_refuses_an_unregistered_run_without_applying_the_name() {
    let label = "unregistered-named-claim";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    claimed_root_store(&claimed);
    let binding = claimed_root_held(label).items[0]
        .control_binding
        .clone()
        .unwrap();
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .runs
        .remove(&binding.claim_id);
    claimed
        .transport
        .replace_held_claims([Ok(claimed_root_held(label))]);
    let error = claimed
        .state
        .name_engram_source_root(
            &claimed.session_id,
            EngramSourceRootRequest {
                work: format!("w-{label}"),
                path: Some(Some(worktree.to_string_lossy().into_owned())),
            },
        )
        .expect_err("a fixture claim without a run cannot publish a receipt");
    assert!(
        error.message.contains("no registered run"),
        "{}",
        error.message
    );
    let roots = claimed.transport.named_roots.lock().unwrap();
    let roots = roots.as_ref().unwrap();
    assert!(roots.latest.is_empty());
    assert!(roots.seen.is_empty());
}

#[test]
fn recognised_tests_name_the_bound_claim_and_the_other_live_named_claim() {
    for case in ["distinct", "shared", "expired"] {
        let label = format!("two-claims-{case}");
        let claimed = ClaimedRoot::new_scripted(
            &label,
            vec![
                bind_reply("two-claims-token"),
                grant_reply("two-claims-grant"),
                begin_reply("two-claims-grant"),
                checkpoint_reply("two-claims-grant"),
            ],
        );
        let first = add_claimed_root_worktree(&claimed.root);
        let second = if case == "shared" {
            first.clone()
        } else {
            let second = claimed.root.join(".worktrees/second");
            run_git_test_command(
                &claimed.root,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    "second",
                    second.to_str().expect("UTF-8 fixture path"),
                ],
            );
            second
        };
        let mut held = claimed_root_held(&label);
        let first_ref = held.items[0].short_ref.clone();
        let second_binding = test_control_work_binding("second", 1);
        held.items.push(EngramHeldClaim {
            work_id: second_binding.work_id.clone(),
            short_ref: "w-second".to_owned(),
            claim_id: second_binding.claim_id.clone(),
            claim_fence: second_binding.claim_fence,
            focused: false,
            control_binding: Some(second_binding.clone()),
        });
        name_root(&claimed, &label, Some(&first), vec![held.clone()]).expect("first root named");
        claimed.transport.register_named_root_run(&second_binding);
        claimed
            .state
            .name_engram_source_root(
                &claimed.session_id,
                EngramSourceRootRequest {
                    work: "w-second".to_owned(),
                    path: Some(Some(second.to_string_lossy().into_owned())),
                },
            )
            .expect("second root named");
        if case == "expired" {
            held.items.pop();
            claimed.transport.replace_held_claims([Ok(held)]);
        }
        deliver_turn_dispatch(&claimed.state, claimed.dispatch()).expect("turn delivered");
        let _ = received_prompt(&claimed);
        claimed.record(|record| record.engram.pending_source_root_line = None);
        claimed.state.note_engram_command_started(
            &claimed.session_id,
            &EngramObservationProvenance::Ambient,
            "test",
            Some("cargo test"),
            Some(second.to_str().expect("UTF-8 fixture path")),
        );
        let line = pending_line(&claimed).expect("test routing is explained");
        let checks = claimed.record(|record| record.engram.active_turn_checks.len());
        if case == "expired" {
            assert!(
                !line.contains("w-second"),
                "a released claim is not presented as held: {line}"
            );
            assert!(line.contains("run the tests in that root"), "{line}");
            assert_eq!(checks, 0);
        } else {
            assert!(
                line.contains(&first_ref),
                "the line identifies the bound claim: {line}"
            );
            assert!(
                line.contains("w-second"),
                "the line identifies the other held claim: {line}"
            );
            assert!(
                line.contains("focus") && line.contains("next turn"),
                "the remedy names the next turn: {line}"
            );
            assert_eq!(
                checks,
                usize::from(case == "shared"),
                "one bound claim only"
            );
        }
        claimed
            .state
            .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
            .expect("turn closes without rerouting its grant");
    }
}

#[test]
fn claimed_root_checks_carry_the_fixture_toolchain_label_settled_at_their_start() {
    // A real toolchain capture runs rustup in the check's worktree on a
    // detached thread; one still running when the fixture drops keeps its
    // temporary root from being removed.
    let label = "fixture-toolchain";
    let (claimed, worktree, runtime_token) = named_root_turn(label, true);
    claimed.state.note_engram_command_started(
        &claimed.session_id,
        &EngramObservationProvenance::Ambient,
        "fixture-toolchain-test",
        Some("cargo test"),
        Some(worktree.to_str().expect("UTF-8 fixture path")),
    );
    let toolchain = claimed.record(|record| {
        assert_eq!(
            record.engram.active_turn_checks.len(),
            1,
            "the check in the named root is recorded"
        );
        record.engram.active_turn_checks[0].toolchain.clone()
    });
    assert!(
        toolchain.is_ready(),
        "the toolchain capture must be settled when the check starts"
    );
    assert_eq!(
        toolchain.wait_until(std::time::Instant::now()),
        Some(Some(CLAIMED_ROOT_TOOLCHAIN.to_owned()))
    );
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &runtime_token)
        .expect("the turn closes");
}

#[test]
fn claimed_root_teardown_waits_for_its_capture_workers_before_removing_its_root() {
    let claimed = ClaimedRoot::new_scripted("teardown-captures", Vec::new());
    let temp_root = claimed
        .state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .to_owned();
    let workers = claimed.record(|record| record.engram.capture_workers.clone());
    // One capture worker of the session, held until released, which reports
    // whether the fixture's temporary root still existed when it finished.
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let worker_gate = gate.clone();
    let worker_root = temp_root.clone();
    let held = EngramCapture::spawn(&workers, move || {
        let (open, opened) = &*worker_gate;
        let _ = opened
            .wait_timeout_while(
                open.lock().expect("gate mutex poisoned"),
                DEADLOCK_GUARD,
                |open| !*open,
            )
            .expect("gate mutex poisoned");
        worker_root.exists()
    });
    let (events_tx, events_rx) = std::sync::mpsc::channel();
    *claimed
        .teardown_events
        .lock()
        .expect("teardown events mutex poisoned") = Some(events_tx.clone());
    let teardown = std::thread::spawn(move || {
        drop(claimed);
        let _ = events_tx.send("dropped");
    });
    // Teardown either starts waiting for the held worker or finishes first.
    let first = events_rx
        .recv_timeout(DEADLOCK_GUARD)
        .expect("teardown reports its progress");
    let (open, opened) = &*gate;
    *open.lock().expect("gate mutex poisoned") = true;
    opened.notify_all();
    let root_outlived_worker = held
        .wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
        .expect("the released worker finishes");
    teardown.join().expect("teardown should finish");
    assert_eq!(
        first, "settling",
        "teardown must wait for the session's capture workers"
    );
    assert!(
        root_outlived_worker,
        "the temporary root must outlive the capture workers running in it"
    );
    assert!(!temp_root.exists(), "teardown still removes the root");
}

#[test]
fn focusing_another_named_claim_rebinds_its_next_turn_and_test_evidence() {
    let label = "focused-named-claim";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("first-token"),
            grant_reply("first-grant"),
            begin_reply("first-grant"),
            checkpoint_reply("first-grant"),
            // The named-root wire model answers status reads directly;
            // only grant and binding responses belong in this queue.
            rebind_reply("second-token"),
            grant_reply("second-grant"),
            begin_reply("second-grant"),
            checkpoint_reply("second-grant"),
        ],
    );
    let first = add_claimed_root_worktree(&claimed.root);
    let second = claimed.root.join(".worktrees/second");
    run_git_test_command(
        &claimed.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "second",
            second.to_str().expect("UTF-8 fixture path"),
        ],
    );
    let mut held = claimed_root_held(label);
    let first_binding = held.items[0]
        .control_binding
        .clone()
        .expect("first binding");
    let second_binding = test_control_work_binding("focused-second", 1);
    held.items.push(EngramHeldClaim {
        work_id: second_binding.work_id.clone(),
        short_ref: "w-second".to_owned(),
        claim_id: second_binding.claim_id.clone(),
        claim_fence: second_binding.claim_fence,
        focused: false,
        control_binding: Some(second_binding.clone()),
    });
    name_root(&claimed, label, Some(&first), vec![held.clone()]).expect("first root named");
    claimed.transport.register_named_root_run(&second_binding);
    claimed
        .state
        .name_engram_source_root(
            &claimed.session_id,
            EngramSourceRootRequest {
                work: "w-second".to_owned(),
                path: Some(Some(second.to_string_lossy().into_owned())),
            },
        )
        .expect("second root named");
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).expect("first turn delivered");
    let _ = received_prompt(&claimed);
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
        .expect("first turn completes");

    held.items[0].focused = false;
    held.items[1].focused = true;
    let selected = select_engram_held_binding(
        held.items.clone(),
        held.omitted,
        EngramBindingPreference {
            current: Some(&first_binding),
            refused: &[],
        },
    );
    assert_eq!(
        selected,
        Some(second_binding.clone()),
        "focus wins over current binding"
    );
    claimed
        .transport
        .work_bindings
        .lock()
        .expect("binding reads mutex poisoned")
        .push_back(Ok(selected));
    claimed.transport.replace_held_claims([Ok(held)]);
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).expect("focused turn delivered");
    let _ = received_prompt(&claimed);
    claimed.record(|record| {
        assert_eq!(record.engram.work_binding, Some(second_binding.clone()));
        assert_eq!(
            record
                .engram
                .active_turn_source_root
                .as_ref()
                .map(|root| root.short_ref.as_str()),
            Some("w-second")
        );
    });
    let wait = || {
        let captures = claimed.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .flat_map(|check| {
                    std::iter::once(check.start_basis.clone())
                        .chain(check.end.as_ref().map(|end| end.end_basis.clone()))
                })
                .collect::<Vec<_>>()
        });
        for capture in captures {
            capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD);
        }
    };
    claimed.state.note_engram_command_started(
        &claimed.session_id,
        &EngramObservationProvenance::Ambient,
        "focused-test",
        Some("cargo test"),
        Some(second.to_str().expect("UTF-8 fixture path")),
    );
    wait();
    assert_eq!(
        claimed.record(|record| record.engram.active_turn_checks.len()),
        1
    );
    claimed.state.note_engram_command_finished(
        &claimed.session_id,
        &EngramObservationProvenance::Ambient,
        "focused-test",
        "cargo test",
        "test result: ok. 1 passed",
        Some(EngramCommandExit::Code(0)),
    );
    wait();
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
        .expect("focused turn completes");
    let requests = claimed.transport.requests();
    let rebind = requests
        .iter()
        .filter(|r| r.request["operation"] == "session_bind")
        .last()
        .unwrap();
    assert_eq!(rebind.request["work_binding"], json!(second_binding));
    let checkpoint = requests
        .iter()
        .filter(|r| r.request["operation"] == "turn_checkpoint")
        .last()
        .unwrap();
    assert_eq!(
        checkpoint.request["verification_evidence"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        checkpoint.request["observations"][0]["outcome"],
        "succeeded"
    );
    assert_eq!(
        checkpoint.request["observations"][0]["source_basis"]["workspace_id"].as_str(),
        fs::canonicalize(&second).unwrap().to_str()
    );
}
