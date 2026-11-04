// Owns the tests of the naming request behind `termal_name_source_root`
// (`AppState::name_engram_source_root`) and of the host line it and a bind
// leave for the agent's next prompt: the refusals, the generation a name gets,
// the seal a clear leaves on the running turn, authoritative reconciliation
// within its budget, and when the line counts as delivered. Does not own the
// validation, the end rules or the basis taken on a named root
// (src/tests/engram_source_roots.rs), or turns measured in a named root
// (src/tests/engram_turn_observations.rs, whose `EOCheckedTurn` fixture this
// child module uses). New module.
use super::*;

mod durability_pending {
    include!("durability_pending_naming.rs");
}

mod authority {
    include!("engram_root_authority.rs");
}

#[path = "engram_authority_manual_write.rs"]
mod manual_write;

#[path = "engram_legacy_root_recovery.rs"]
mod legacy_recovery;

#[path = "engram_source_root_confirmation.rs"]
mod confirmation;

#[path = "engram_source_root_unknown_fields.rs"]
mod unknown_fields;

#[path = "engram_source_root_off_rebind.rs"]
mod off_rebind;

#[path = "engram_unconfirmed_root_reasons.rs"]
mod unconfirmed_root_reasons;

#[path = "engram_root_retained_runs.rs"]
mod retained_runs;

impl EOCheckedTurn {
    /// These reconciliation fixtures script BOTH producer operations. A
    /// status tuple alone cannot invent the canonical run/event carrier.
    fn canonical_root_readback(
        &self,
        session: &str,
        binding: Option<&EngramControlWorkBinding>,
        state: Option<EngramNamedRootState>,
        generation: u64,
    ) -> Result<(), EngramTransportError> {
        if let Some(binding) = binding
            && let Some(state) = state
                .as_ref()
                .filter(|state| **state != EngramNamedRootState::Unknown)
        {
            let store = naming_store(self);
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
                .or_else(|| local.as_ref().map(|root| json!({"event":sha256_hex(b"external-prior-binding"),
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
                            json!({"event":sha256_hex(format!("external-bound-{}-{generation}", binding.claim_id).as_bytes()),
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
                        event["event"] = json!(sha256_hex(format!(
                            "external-ended-{}-{}",
                            binding.claim_id,
                            prior_position + 1
                        ).as_bytes()));
                        event["position"]["position"] = json!(prior_position + 1);
                    }
                }
                EngramNamedRootState::UnboundByRelease {
                    last_generation, ..
                } => {
                    if *last_generation > prior_generation {
                        event = Some(
                            json!({"event":sha256_hex(format!("external-bound-{}-{last_generation}",binding.claim_id).as_bytes()),
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
        let (target, mut fence) = {
            let inner = self.state.inner.lock().unwrap();
            (AppState::engram_source_root_target_locked(&inner, session)
                .map_err(EngramTransportError::local_state)?
                .ok_or_else(|| EngramTransportError::local_state("no current ordinary root reader"))?,
                EngramRootReadFence::capture(&inner))
        };
        fence.generation = generation;
        self.state.reconcile_engram_named_root_with_route_classified_until(
            session, binding, state, &fence, Some(&target.connection),
            self.state.engram_budget_clock().now() + Duration::from_secs(2),
        ).map_err(EngramRootReconcileFailure::into_transport)
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
    // Real time on purpose: the removed-root reads below take their deadline
    // from TEST_ENGRAM_REMOVED_ROOT_READ_CLOCK, a deterministic sample
    // sequence that the reader consults only on the real clock (a scripted
    // clock would bypass it and the deadline would never pass).
    let mut claimed = EOCheckedTurn::idle_real(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .unwrap();
    replace_naming_session(&mut claimed, label);
    claimed.record_mut(|record| record.engram.work_binding = None);
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

fn resolve_removed_roots_with_one_attempt(claimed: &EOCheckedTurn, claim: Option<&str>) {
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let binding = claimed.record_mut(|record| record.engram.work_binding.clone().unwrap());
    naming_store(&claimed);
    claimed
        .transport
        .replace_held_claims([Ok(naming_claim(label))]);
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
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let mut claimed = EOCheckedTurn::idle(label, true);
    let store = naming_store(&claimed);
    let (binding,) = claimed.record_mut(|record| {
        (
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
        .canonical_root_readback(
            &claimed.session_id,
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
            .canonical_root_readback(
                &claimed.session_id,
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

fn replace_naming_session(claimed: &mut EOCheckedTurn, label: &str) {
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let named = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    request_naming(
        &claimed,
        label,
        Some(&claimed.root),
        vec![naming_claim(label)],
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
    request_naming(
        &claimed,
        label,
        Some(&claimed.root),
        vec![naming_claim(label)],
    )
    .unwrap();
    assert_eq!(
        token().revision,
        2,
        "confirmed publication after allocation advances history"
    );
    request_naming(
        &claimed,
        label,
        Some(&claimed.root),
        vec![naming_claim(label)],
    )
    .unwrap();
    assert_eq!(token().revision, 2, "exact replay is idempotent");
    request_naming(&claimed, label, None, vec![naming_claim(label)]).unwrap();
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
        let mut claimed = EOCheckedTurn::idle(label, true);
        let worktree = naming_worktree(&claimed.root);
        request_naming(
            &claimed,
            label,
            Some(&worktree),
            vec![naming_claim(label)],
        )
        .unwrap();
        replace_naming_session(&mut claimed, label);
        // The live same-store reader does not focus or bind the historical claim.
        claimed.record_mut(|record| record.engram.work_binding = None);
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
    let mut claimed = EOCheckedTurn::idle(label, true);
    naming_store(&claimed);
    let worktree = naming_worktree(&claimed.root);
    let mut store_work = None;
    for sequence in 0..=ENGRAM_NAMED_ROOT_JOURNAL_LIMIT {
        let mut held = naming_claim(label);
        let binding = held.items[0].control_binding.as_mut().unwrap();
        binding.claim_id = format!("removed-claim-{sequence}");
        binding.run_id = format!("removed-run-{sequence}");
        binding.root_execution_id = format!("removed-execution-{sequence}");
        let binding = binding.clone();
        held.items[0].claim_id = binding.claim_id.clone();
        claimed.record_mut(|record| {
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
    claimed.record_mut(|record| record.engram.work_binding = None);
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

fn replace_or_unbind_naming_session(claimed: &mut EOCheckedTurn, label: &str, remove: bool) {
    let project = claimed.record_mut(|record| record.session.project_id.clone().unwrap());
    let successor =
        create_test_project_session(&claimed.state, Agent::Codex, &project, &claimed.root);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let old = inner.find_session_index(&claimed.session_id).unwrap();
        if remove {
            inner.remove_session_at(old);
        } else {
            inner.sessions[old].session.project_id = None;
            inner.sessions[old].engram.work_binding = None;
        }
    }
    claimed.session_id = successor;
    let binding = test_control_work_binding(&format!("eo-check-{label}"), 1);
    claimed.record_mut(|record| record.engram.work_binding = Some(binding.clone()));
    claimed.transport.enable_named_roots(&claimed.session_id, &binding);
    claimed.transport.set_current_work_binding(Some(binding));
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .work_session_read_reply = Some(Ok(ordinary_naming_reply(claimed)));
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
    let mut claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    if cleanup {
        request_naming(
            &claimed,
            label,
            Some(&worktree),
            vec![naming_claim(label)],
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
            request_naming(&claimed, label, None, vec![naming_claim(label)]).unwrap_err();
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
        request_naming(
            &claimed,
            label,
            Some(&worktree),
            vec![naming_claim(label)],
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
    let (binding,) = claimed.record_mut(|record| {
        (
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
        let result = claimed.canonical_root_readback(
            &claimed.session_id,
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
    let recovery = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let recovered = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let mut claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let (binding,) = claimed.record_mut(|record| {
        (
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
            .canonical_root_readback(
                &claimed.session_id,
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
    let mut claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let (binding,) = claimed.record_mut(|record| {
        (
            record.engram.work_binding.clone(),
        )
    });
    let durable_path = claimed.state.persistence_path.clone();
    let failing_path = claimed.root.join("orphan-read-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    claimed.state.persistence_path = Arc::new(failing_path);
    assert!(
        claimed
            .canonical_root_readback(
                &claimed.session_id,
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
        .canonical_root_readback(
            &claimed.session_id,
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
fn ordinary_finished_check_facts_do_not_override_an_unknown_root() {
    let label = "ordinary-check-before-unknown";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    claimed.begin("Observe the ordinary check.");
    let mut recorder = claimed.recorder();
    let cwd = worktree.to_string_lossy();
    recorder.command_started_in("checked-before-unknown", "cargo test",
        Some("cargo test"), Some(&cwd)).unwrap();
    recorder.command_completed_with_exit("checked-before-unknown", "cargo test",
        "test result: ok. 1 passed", CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    claimed.wait_for_snapshots();
    let binding = claimed.record_mut(|record| {
        assert_eq!(record.engram.active_turn_checks.len(), 1);
        assert!(record.engram.active_turn_checks[0].end.is_some());
        record.engram.work_binding.clone().unwrap()
    });
    claimed.canonical_root_readback(&claimed.session_id, Some(&binding),
        Some(EngramNamedRootState::Unknown), u64::MAX).unwrap();
    assert_eq!(claimed.state.engram_turn_root_capture(&claimed.session_id), EngramRootCapture::Unconfirmed);
    assert_ordinary_observation_has_no_native_credit(&claimed, &claimed.finish());
}

#[test]
fn source_root_review_normalized_wire_timestamp_preserves_the_binding_and_intent() {
    let label = "wire-time-normalization";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let named = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .unwrap();
    let millis = "2026-01-01T00:00:00.000Z";
    let (binding, event) = {
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
        .canonical_root_readback(
            &claimed.session_id,
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
    let repeated = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .unwrap();
    assert_eq!(
        repeated.generation, named.generation,
        "same instant must keep the name"
    );
    request_naming(&claimed, label, None, vec![naming_claim(label)]).unwrap();
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .unwrap_err();
    fs::rename(worktree.join(".git"), worktree.join("saved-git-entry")).unwrap();
    let error = request_naming(
        &claimed,
        label,
        Some(&worktree.join("missing/../.")),
        vec![naming_claim(label)],
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
fn source_root_ordinary_reader_without_budget_preserves_uncertainty() {
    source_root_ordinary_readback_failure(false);
}

#[test]
fn source_root_ordinary_reader_failure_preserves_uncertainty() {
    source_root_ordinary_readback_failure(true);
}

fn source_root_ordinary_readback_failure(fail_read: bool) {
    let label = "ordinary-source-readback-failure";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let named = request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    let target = AppState::engram_source_root_target_locked(
        &claimed.state.inner.lock().unwrap(), &claimed.session_id).unwrap().unwrap();
    let binding = target.work_binding.as_ref().unwrap();
    let clock = claimed.state.engram_budget_clock();
    let deadline = clock.now() + Duration::from_secs(2);
    let guard = claimed.state.guard_engram_root_read_until(&target, Some(binding), deadline).unwrap().unwrap();
    assert!(guard.still_withholding(&claimed.state.inner.lock().unwrap()));
    if fail_read {
        claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
            .root_read_failures.insert(binding.claim_id.clone());
    }
    let before = claimed.transport.requests().len();
    let read = claimed.state.read_engram_authority_fact_until(&target, binding,
        if fail_read { deadline } else { clock.now() });
    assert!(read.is_err());
    if !fail_read { assert_eq!(claimed.transport.requests().len(), before, "an expired budget sends nothing"); }
    assert_eq!(claimed.state.engram_root_capture(&claimed.session_id), EngramRootCapture::Unconfirmed);
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(inner.engram_work_source_roots[0].generation, named.generation);
    assert!(engram_authority_work_unresolved(&inner, &guard.store, &binding.work_id));
    drop(inner);
    assert_ordinary_turn_has_no_native_requests(&claimed);
}

#[test]
fn source_root_waits_for_the_real_writer_before_transport_and_before_success() {
    let label = "binding-writer-fences";
    let mut claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let (tx, rx) = std::sync::mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    let transport = claimed.transport.clone();
    let mut batch = PersistFenceBatch::default();
    let mut cache = SqlitePersistConnectionCache::new();
    let task = std::thread::spawn(move || {
        let result = request_naming(
            &claimed,
            label,
            Some(&worktree),
            vec![naming_claim(label)],
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
    receive_prepared_naming_intent(&state, &rx, &mut batch, &mut cache, &task);
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

// Identity and canonical reads can each prepare an authority image. Select
// the actual staged intent, acknowledging only the preceding empty images.
fn receive_prepared_naming_intent<T>(
    state: &AppState,
    rx: &std::sync::mpsc::Receiver<PersistRequest>,
    batch: &mut PersistFenceBatch,
    cache: &mut SqlitePersistConnectionCache,
    task: &std::thread::JoinHandle<T>,
) {
    loop {
        receive_naming_authority_fence(rx, batch, task);
        let image = match &batch.pending[0].target {
            PersistFenceTarget::EngramWorkAuthority(image) => image,
            _ => panic!("naming must request an exact authority-image fence"),
        };
        if image.journals.iter().any(|journal| journal.pending.is_some()) {
            assert_eq!(image.history.transition.as_ref().unwrap().phase,
                EngramAuthorityPhase::Prepared);
            return;
        }
        assert!(image.journals.iter().all(|journal| journal.confirmed.is_none()),
            "a completed event is not a preparatory guard");
        let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
        persist_delta_with_fences(cache, state.persistence_path.as_path(), &delta, batch).unwrap();
    }
}

#[test]
fn source_root_writer_failure_keeps_the_intent_and_sends_no_binding() {
    let label = "binding-writer-failure";
    let mut claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let (tx, rx) = std::sync::mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let transport = claimed.transport.clone();
    let task = std::thread::spawn(move || {
        let result = request_naming(
            &claimed,
            label,
            Some(&worktree),
            vec![naming_claim(label)],
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
    receive_prepared_naming_intent(&state, &rx, &mut batch, &mut cache, &task);
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .unwrap_err();
    fs::rename(worktree.join(".git"), worktree.join("saved-git-entry")).unwrap();
    let error = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
fn a_source_root_clear_keeps_the_ordinary_turns_original_named_generation() {
    let label = "ordinary-clear-provenance";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let named = request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    claimed.begin("Observe the original named root.");
    let opening = claimed.record_mut(|record| record.engram.observed_opening().unwrap().clone());
    request_naming(&claimed, label, None, vec![naming_claim(label)]).unwrap();
    claimed.record_mut(|record| {
        assert_eq!(record.engram.observed_opening().unwrap().provenance, opening.provenance);
        let root = record.engram.observed_source_root().unwrap();
        assert_eq!(root.generation, named.generation);
        assert!(root.sealed_revision.is_some());
    });
    assert_ordinary_observation_has_no_native_credit(&claimed, &claimed.finish());
}

#[test]
fn a_stale_root_projection_cannot_remove_a_later_canonically_named_root() {
    let label = "ordinary-stale-projection";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let named = request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    let target = AppState::engram_source_root_target_locked(
        &claimed.state.inner.lock().unwrap(), &claimed.session_id).unwrap().unwrap();
    let fence = EngramRootReadFence::capture(&claimed.state.inner.lock().unwrap());
    let before = claimed.transport.requests().len();
    claimed.state.reconcile_engram_named_root_with_route_classified_until(
        &claimed.session_id, target.work_binding.as_ref(), Some(EngramNamedRootState::None),
        &fence, Some(&target.connection), claimed.state.engram_budget_clock().now() + Duration::from_secs(2),
    ).unwrap_or_else(|failure| panic!("canonical root reconciliation failed: {}", failure.into_transport()));
    assert_eq!(claimed.transport.requests()[before..].iter()
        .filter(|request| request.request["operation"] == "named_root_read").count(), 1);
    assert_ordinary_turn_has_no_native_requests(&claimed);
    assert_eq!(claimed.state.inner.lock().unwrap().engram_work_source_roots[0].generation, named.generation,
        "only the current canonical read can displace the root; a stale tuple cannot");
}

#[test]
fn source_root_reconciliation_persistence_failure_withholds_until_repaired() {
    let label = "binding-reconcile-persist";
    let mut claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .unwrap();
    let (binding, roots) = {
        let inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        (
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
            .canonical_root_readback(
                &claimed.session_id,
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
        .canonical_root_readback(
            &claimed.session_id,
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
    let claimed = EOCheckedTurn::idle("binding-readback-owner", true);
    let store = naming_store(&claimed);
    let (binding,) = claimed.record_mut(|record| {
        (
            record.engram.work_binding.clone().unwrap(),
        )
    });
    claimed
        .canonical_root_readback(
            &claimed.session_id,
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
        .canonical_root_readback(
            &claimed.session_id,
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
        .canonical_root_readback(&claimed.session_id, Some(&binding), bound, u64::MAX)
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let named = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let mut claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let error = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let named = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let error = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let next = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
        test_control_work_binding(&format!("eo-check-{label}"), 1).run_id
    );
}

#[test]
fn source_root_review_delayed_read_cannot_overwrite_a_same_generation_end() {
    let label = "review-same-generation-read";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .unwrap();
    let (fence, connection, binding, earlier) = {
        let inner = claimed.state.inner.lock().unwrap();
        let target = AppState::engram_source_root_target_locked(&inner, &claimed.session_id)
        .unwrap()
        .unwrap();
        (
            EngramRootReadFence::capture(&inner),
            target.connection,
            target.work_binding,
            inner.sessions[inner.find_session_index(&claimed.session_id).unwrap()]
                .engram
                .named_root
                .clone(),
        )
    };
    request_naming(&claimed, label, None, vec![naming_claim(label)]).unwrap();
    let settled = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal
        .clone();
    claimed
        .state
        .reconcile_engram_named_root_with_route_classified_until(
            &claimed.session_id, binding.as_ref(), earlier, &fence, Some(&connection),
            claimed.state.engram_budget_clock().now() + Duration::from_secs(2),
        )
        .unwrap_or_else(|failure| panic!("canonical root reconciliation failed: {}", failure.into_transport()));
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
fn source_root_review_known_retirement_preserves_the_ordinary_snapshot() {
    let label = "review-admitted-retirement";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    claimed.begin("Observe the named root.");
    let admitted = claimed.state.engram_turn_root_capture(&claimed.session_id);
    let (binding,) = claimed.record_mut(|record| {
        (
            record.engram.work_binding.clone(),
        )
    });
    claimed
        .canonical_root_readback(
            &claimed.session_id,
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
        .canonical_root_readback(
            &claimed.session_id,
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
        .canonical_root_readback(
            &claimed.session_id,
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .unwrap();
    let (binding,) = claimed.record_mut(|record| {
        (
            record.engram.work_binding.clone(),
        )
    });
    claimed
        .canonical_root_readback(
            &claimed.session_id,
            binding.as_ref(),
            Some(EngramNamedRootState::Bound {
                workspace_id: claimed.root.to_string_lossy().into_owned(),
                generation: 3,
                named_at: chrono::Utc::now().to_rfc3339(),
            }),
            u64::MAX,
        )
        .unwrap();
    let store = naming_store(&claimed);
    let binding = binding.unwrap();
    claimed.record_mut(|record| record.engram.work_binding = None);
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
    let mut claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let (binding,) = claimed.record_mut(|record| {
        (
            record.engram.work_binding.clone().unwrap(),
        )
    });
    claimed
        .canonical_root_readback(
            &claimed.session_id,
            Some(&binding),
            Some(EngramNamedRootState::None),
            u64::MAX,
        )
        .unwrap();
    let store = naming_store(&claimed);
    let work =
        claimed.record_mut(|record| record.engram.work_binding.as_ref().unwrap().work_id.clone());
    let prepared = engram_work_naming_token(&claimed.state.inner.lock().unwrap(), &store, &work);
    let seed = AcceptanceEvaluationTargetSeed {
        reuse_identity: None,
        selected_evidence: Vec::new(),
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
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let claimed = EOCheckedTurn::idle(label, true);
    naming_worktree(&claimed.root);
    let named = request_naming(
        &claimed,
        label,
        Some(&claimed.root),
        vec![naming_claim(label)],
    )
    .unwrap();
    let (binding,) = claimed.record_mut(|record| {
        (
            record.engram.work_binding.clone(),
        )
    });
    claimed
        .canonical_root_readback(
            &claimed.session_id,
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
        .canonical_root_readback(
            &claimed.session_id,
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
    let mut claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let (binding,) = claimed.record_mut(|record| {
        (
            record.engram.work_binding.clone(),
        )
    });
    claimed
        .canonical_root_readback(
            &claimed.session_id,
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    naming_store(&claimed);
    let mut last_generation = 0;
    for occurrence in 0..=ENGRAM_NAMED_ROOT_JOURNAL_LIMIT {
        let mut held = naming_claim(label);
        let mut binding = held.items[0].control_binding.clone().unwrap();
        binding.claim_id = format!("successive-claim-{occurrence}");
        binding.run_id = format!("successive-run-{occurrence}");
        binding.root_execution_id = format!("successive-execution-{occurrence}");
        held.items[0].claim_id = binding.claim_id.clone();
        held.items[0].control_binding = Some(binding.clone());
        claimed.record_mut(|record| {
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .unwrap();
    let mut held = naming_claim(label);
    let mut binding = held.items[0].control_binding.clone().unwrap();
    binding.claim_id = "new-clear-claim".to_owned();
    binding.run_id = "new-clear-run".to_owned();
    binding.root_execution_id = "new-clear-execution".to_owned();
    held.items[0].claim_id = binding.claim_id.clone();
    held.items[0].control_binding = Some(binding.clone());
    claimed.record_mut(|record| {
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
    let mut claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
        serde_json::to_value(pending.request().unwrap()).unwrap(),
        serde_json::to_value(original_intent.request().unwrap()).unwrap()
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    let error = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let result = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .refuse_next = true;
    let error = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .unwrap_err();
    assert!(error.message.contains("named_root_binding_refused"));
    let pending = {
        let inner = claimed.state.inner.lock().unwrap();
        assert!(inner.engram_work_source_roots.is_empty());
        assert!(
            inner
                .engram_named_root_journal
                .iter()
                .all(|j| j.confirmed.is_none())
        );
        inner.engram_named_root_journal.iter()
            .find_map(|journal| journal.pending.clone())
            .expect("a refusal without covering lifecycle proof retains the exact intent")
    };
    let pending_request = serde_json::to_value(pending.request().unwrap()).unwrap();
    let refused = claimed.transport.requests().into_iter()
        .find(|request| request.request["operation"] == "named_root_bind")
        .expect("the refusal belongs to the actual naming send").request;
    assert_eq!(pending_request, refused,
        "retained generation, content and idempotency key equal the refused send");
    let repaired = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .unwrap();
    let sends = claimed.transport.requests().into_iter()
        .filter(|request| request.request["operation"] == "named_root_bind")
        .map(|request| request.request).collect::<Vec<_>>();
    assert_eq!(sends, vec![pending_request.clone(), pending_request],
        "repair retries the immutable intent and key rather than minting a new event");
    assert_eq!(repaired.generation, pending.root.generation);
    let inner = claimed.state.inner.lock().unwrap();
    assert!(inner.engram_work_source_roots.contains(&pending.root),
        "the repaired acknowledgement publishes the original root");
    assert!(inner.engram_named_root_journal.iter().all(|journal| journal.pending.is_none()));
    assert!(inner.engram_named_root_journal.iter().any(|journal|
        journal.confirmed.as_ref().is_some_and(|(event, _)| event == &pending)));
}

#[test]
fn named_root_readback_distinguishes_release_from_fence_changes() {
    let label = "binding-lifecycle";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let named = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .unwrap();
    let (mut binding, state) = claimed.record_mut(|record| {
        (
            record.engram.work_binding.clone().unwrap(),
            record.engram.named_root.clone(),
        )
    });
    binding.claim_fence += 3;
    claimed.record_mut(|record| record.engram.work_binding = Some(binding.clone()));
    claimed
        .canonical_root_readback(
            &claimed.session_id,
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
        .canonical_root_readback(
            &claimed.session_id,
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
        .canonical_root_readback(&claimed.session_id, Some(&binding), None, u64::MAX)
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
    for (wire, expected) in [
        (json!({"state":"none"}), EngramNamedRootState::None),
        (json!({"state":"bound","workspace_id":"x","generation":3,"named_at":"time"}),
            EngramNamedRootState::Bound { workspace_id: "x".into(), generation: 3, named_at: "time".into() }),
        (json!({"state":"unbound_by_release","last_generation":3,"released_at_position":9}),
            EngramNamedRootState::UnboundByRelease { last_generation: 3, released_at_position: 9 }),
        (json!({"state":"future"}), EngramNamedRootState::Unknown),
    ] {
        assert_eq!(serde_json::from_value::<EngramNamedRootState>(wire).unwrap(), expected);
    }
    assert_eq!(serde_json::from_value::<Option<EngramNamedRootState>>(Value::Null).unwrap(), None);
}

#[test]
fn clearing_a_root_records_its_original_identity_and_stamps_workdir_captures_as_ended() {
    let label = "binding-ended-basis";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .unwrap();
    request_naming(&claimed, label, None, vec![naming_claim(label)]).unwrap();
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
fn naming_store(claimed: &EOCheckedTurn) -> EngramAuthorityStoreKey {
    let inner = claimed.state.inner.lock().unwrap();
    engram_project_for_session_locked(&inner, &claimed.session_id).unwrap()
        .engram.as_ref().unwrap().authority_store_key.clone().unwrap()
}

fn prepare_naming_worktree_container(root: &FsPath) {
    // Match the real Git census used by the original claimed-root fixture:
    // sibling worktrees are not untracked nested repositories in this root.
    let ignore = root.join(".gitignore");
    let mut contents = fs::read_to_string(&ignore).unwrap_or_default();
    if contents.lines().any(|line| line == ".worktrees/") { return; }
    if !contents.is_empty() && !contents.ends_with('\n') { contents.push('\n'); }
    contents.push_str(".worktrees/\n");
    fs::write(ignore, contents).unwrap();
    run_git_test_command(root, &["add", ".gitignore"]);
    run_git_test_command(root, &["commit", "--quiet", "-m", "ignore naming worktrees"]);
}

fn naming_worktree(root: &FsPath) -> PathBuf {
    prepare_naming_worktree_container(root);
    let path = root.join(".worktrees").join("naming");
    run_git_test_command(root, &[
        "worktree", "add", "--quiet", "-b", "naming", path.to_str().unwrap(),
    ]);
    path
}

#[test]
fn source_root_compatibility_missing_projection_recovers_owner_without_an_absence_shortcut() {
    let label = "compatibility-missing-projection";
    let claimed = EOCheckedTurn::idle(label, true);
    let store = naming_store(&claimed);
    let (binding,) = claimed.record_mut(|record| {
        (
            record.engram.work_binding.clone().unwrap(),
        )
    });
    claimed
        .canonical_root_readback(&claimed.session_id, Some(&binding), None, u64::MAX)
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
        let claimed = EOCheckedTurn::idle(label, true);
        let worktree = naming_worktree(&claimed.root);
        let named = request_naming(
            &claimed,
            label,
            Some(&worktree),
            vec![naming_claim(label)],
        )
        .unwrap();
        let store = naming_store(&claimed);
        let (binding,) = claimed.record_mut(|r| {
            (
                r.engram.work_binding.clone().unwrap(),
            )
        });
        if released {
            claimed
                .canonical_root_readback(
                    &claimed.session_id,
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
            .canonical_root_readback(
                &claimed.session_id,
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
fn an_ordinary_claim_without_store_identity_has_no_confirmed_opening() {
    let claimed = EOCheckedTurn::idle("ordinary-no-store", true);
    change_engram_settings(&claimed.state, |settings| settings.authority_store_key = None);
    claimed.begin("Ordinary input does not wait for store identity.");
    claimed.record_mut(|record| {
        assert!(record.engram.unadmitted_active_turn.is_some());
        assert!(record.engram.observed_opening().is_none());
    });
    assert_ordinary_turn_has_no_native_requests(&claimed);
    claimed.end();
}

#[test]
fn passive_legacy_begin_history_never_controls_an_ordinary_named_opening() {
    let label = "ordinary-legacy-history-opening";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let named = request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    claimed.record_mut(|record| {
        record.engram.historical_control.insert("engramBeginsRecorded".into(), json!(false));
        record.engram.historical_control.insert("engramBeginRecovery".into(),
            json!({"unknownOldEnvelope":{"grant_id":"historical-only", "future":[null,true]}}));
    });
    let history = claimed.record_mut(|record| record.engram.historical_control.clone());
    for _ in 0..2 {
        claimed.begin("Historical control bytes remain passive.");
        claimed.record_mut(|record| {
            assert_eq!(record.engram.observed_source_root().unwrap().generation, named.generation);
            assert!(matches!(record.engram.observed_opening().unwrap().provenance.clone(),
                EngramRootCapture::Recorded { state: EngramSourceRootState::Named, .. }));
            assert_eq!(record.engram.historical_control, history);
        });
        assert_ordinary_observation_has_no_native_credit(&claimed, &claimed.finish());
        claimed.record_mut(|record| assert_eq!(record.engram.historical_control, history));
    }
}

#[test]
fn a_late_store_identity_never_upgrades_the_current_ordinary_opening() {
    let label = "ordinary-late-store-identity";
    let claimed = EOCheckedTurn::idle(label, true);
    let store = naming_store(&claimed);
    let worktree = naming_worktree(&claimed.root);
    change_engram_settings(&claimed.state, |settings| settings.authority_store_key = None);
    claimed.begin("Start without attributed store identity.");
    let original = claimed.record_mut(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    assert!(original.opening_root.is_none());
    change_engram_settings(&claimed.state, |settings| settings.authority_store_key = Some(store.clone()));
    let named = request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    claimed.record_mut(|record| {
        let current = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert_eq!(current.host_turn_ref, original.host_turn_ref);
        assert!(current.opening_root.is_none());
    });
    claimed.end();
    claimed.begin("The next ordinary opening uses the new name.");
    claimed.record_mut(|record| assert_eq!(record.engram.observed_source_root().unwrap().generation, named.generation));
    assert_ordinary_observation_has_no_native_credit(&claimed, &claimed.finish());
}

fn naming_claim(label: &str) -> EngramHeldClaims {
    let binding = test_control_work_binding(&format!("eo-check-{label}"), 1);
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
fn request_naming(
    claimed: &EOCheckedTurn,
    label: &str,
    path: Option<&FsPath>,
    reads: Vec<EngramHeldClaims>,
) -> std::result::Result<EngramSourceRootResponse, ApiError> {
    naming_store(claimed);
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


fn delivered_naming_prompt(turn: &EOCheckedTurn) -> String {
    match receive(&turn._runtime_rx.lock().unwrap(), "ordinary naming prompt") {
        CodexRuntimeCommand::Prompt { session_id, command } => {
            assert_eq!(session_id, turn.session_id);
            command.prompt
        }
        _ => panic!("expected an actual ordinary provider prompt"),
    }
}

fn pending_line(claimed: &EOCheckedTurn) -> Option<String> {
    claimed.record_mut(|record| record.engram.pending_source_root_line.clone())
}

#[test]
fn a_name_is_refused_without_the_live_claim_to_a_delegated_session_and_on_a_share() {
    let label = "naming-refusals";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);

    let other_work = EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: "work-other".to_owned(),
            short_ref: "w-other".to_owned(),
            claim_id: "claim-other".to_owned(),
            ..Default::default()
        }],
        omitted: 0,
    };
    let error = request_naming(&claimed, label, Some(&worktree), vec![other_work.clone()])
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
    let error = request_naming(&claimed, label, Some(&worktree), vec![truncated])
        .expect_err("the claim is not on the list Engram gave");
    assert!(
        error.message.contains("3 more were left out of its list"),
        "the refusal says the list was cut: {}",
        error.message
    );

    let mut no_claim_id = naming_claim(label);
    no_claim_id.items[0].claim_id.clear();
    let error = request_naming(&claimed, label, Some(&worktree), vec![no_claim_id])
        .expect_err("an Engram without claim ids cannot scope a name");
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);

    // Named by its id, since a row without a short reference cannot be
    // found by one.
    let mut no_short_ref = naming_claim(label);
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

    let error = request_naming(
        &claimed,
        label,
        Some(FsPath::new("//server/share/wt")),
        vec![naming_claim(label)],
    )
    .expect_err("a share is refused");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("network share"), "{}", error.message);
    // So is a verbatim alias of the network redirector, before anything
    // resolves it.
    let error = request_naming(
        &claimed,
        label,
        Some(FsPath::new(r"\\?\GLOBALROOT\Device\Mup\server\share\wt")),
        vec![naming_claim(label)],
    )
    .expect_err("a redirector alias is refused");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("network share"), "{}", error.message);

    let error = request_naming(
        &claimed,
        label,
        Some(FsPath::new("   ")),
        vec![naming_claim(label)],
    )
    .expect_err("a blank path is no clear");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(
        error.message.contains("`path` is blank"),
        "{}",
        error.message
    );

    let error = request_naming(
        &claimed,
        label,
        Some(FsPath::new(&"x".repeat(MAX_DELEGATION_CWD_CHARS + 1))),
        vec![naming_claim(label)],
    )
    .expect_err("an overlong path is refused before it is checked");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("at most"), "{}", error.message);

    // A delegated session by its own link, with no delegation row left.
    claimed.record_mut(|record| {
        record.session.parent_delegation_id = Some("delegation-gone".to_owned());
    });
    let error = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .expect_err("a session linked to a parent names nothing");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains("only a root session"),
        "{}",
        error.message
    );
    claimed.record_mut(|record| record.session.parent_delegation_id = None);

    claimed
        .state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .delegations
        .push(DelegationRecord {
            original_project_id: None,
            project_basis_recorded: false,
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
            base_revision: None,
            review_result_recovery_probe_attempt: None,
            review_result_recovery_error: None,
            review_result_schema_version: None,
            queued_followup_prompt_id: None,
            review_result_submission_attempt: 0,
            acceptance_evaluation: None,
            attempt: DelegationAttemptState::default(),
        });
    let error = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let held = || vec![naming_claim(label)];

    let first = request_naming(&claimed, label, Some(&worktree), held()).expect("the first name");
    let store = naming_store(&claimed);
    let requested_on = {
        let inner = claimed.state.inner.lock().expect("state mutex poisoned");
        AcceptanceEvaluationSourceRoot::from_entry(&inner.engram_work_source_roots[0])
    };
    let again = request_naming(&claimed, label, Some(&worktree), held()).expect("the same name again");
    assert_eq!(again.generation, first.generation, "nothing changed");
    assert_eq!(again.sealed, None);
    {
        let inner = claimed.state.inner.lock().expect("state mutex poisoned");
        assert!(
            requested_on.still_named(&inner.engram_work_source_roots, &store),
            "an evaluation requested on the root is not refused for a repeated name"
        );
    }

    let cleared = request_naming(&claimed, label, None, held()).expect("a clear");
    assert_eq!(cleared.generation, 0);
    assert_eq!(cleared.root, None);
    assert_eq!(
        cleared.sealed, None,
        "no turn runs in the old root, so the clear reports no seal"
    );
    let renamed = request_naming(&claimed, label, Some(&worktree), held()).expect("named again");
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
    let label = "ordinary-clear-seals";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    claimed.begin("Observe the named root.");
    assert!(claimed.record_mut(|record| record.engram.observed_source_root().is_some()));
    let cleared = request_naming(&claimed, label, None, vec![naming_claim(label)]).unwrap();
    let revision = content_revision_of(&worktree);
    assert_eq!(cleared.sealed.and_then(|sealed| sealed.source_revision), Some(revision.clone()));
    assert_eq!(claimed.record_mut(|record| record.engram.observed_source_root()
        .and_then(|root| root.sealed_revision.clone())), Some(revision));
    assert!(pending_line(&claimed).is_some_and(|line| line.contains("no worktree named")));
    assert_ordinary_observation_has_no_native_credit(&claimed, &claimed.finish());
}

#[test]
fn a_clear_seals_no_turn_of_another_claim_measured_in_the_same_worktree() {
    let label = "ordinary-clear-other-claim";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    let original = claimed.record_mut(|record| record.engram.work_binding.clone().unwrap());
    let other = test_control_work_binding("ordinary-other-named-work", 1);
    let mut held = naming_claim(label);
    held.items.push(EngramHeldClaim {
        work_id: other.work_id.clone(), short_ref: other.work_id.clone(),
        claim_id: other.claim_id.clone(), claim_fence: other.claim_fence,
        focused: false, control_binding: Some(other.clone()),
    });
    claimed.transport.register_named_root_run(&other);
    claimed.transport.set_current_work_binding(Some(other.clone()));
    claimed.transport.replace_held_claims([Ok(held.clone())]);
    claimed.state.name_engram_source_root(&claimed.session_id, EngramSourceRootRequest {
        work: other.work_id.clone(), path: Some(Some(worktree.to_string_lossy().into_owned())),
    }).unwrap();
    claimed.record_mut(|record| record.engram.work_binding = Some(other.clone()));
    claimed.begin("The other work uses the same physical tree.");
    assert_eq!(claimed.record_mut(|record| record.engram.observed_source_root().unwrap().claim_id.clone()), other.claim_id);
    claimed.transport.set_current_work_binding(Some(original));
    let cleared = request_naming(&claimed, label, None, vec![held]).unwrap();
    assert!(cleared.sealed.is_none());
    assert_eq!(claimed.record_mut(|record| record.engram.observed_source_root()
        .map(|root| root.sealed_revision.clone())), Some(None));
    assert_ordinary_turn_has_no_native_requests(&claimed);
    claimed.end();
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
fn source_root_base_mcp_names_and_clears_without_execution_observability_or_a_grant() {
    let label = "naming-base-only";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    script_ordinary_work_session(&claimed, label);
    claimed.record_mut(|record| {
        assert!(!record.engram.historical_control.contains_key("engramRoutingToken"));
        assert!(record.engram.work_binding.is_none());
        assert!(!record.engram.historical_control.contains_key("engramActiveGrantId"));
        assert!(record.engram.observed_source_root().is_none());
    });

    let named = ordinary_naming_request(&claimed, label, Some(&worktree))
        .expect("Base MCP names a claimed root without turn admission");
    assert!(named.root.is_some());
    assert!(named.generation > 0);
    assert!(named.sealed.is_none());
    assert_eq!(named.evaluation_notice, Some(EngramSourceRootEvaluationNotice::NamedRootSightingRequired));
    assert_eq!(serde_json::to_value(&named).unwrap()["evaluationNotice"], "named_root_sighting_required");
    assert!(named.notice.contains("Naming the source root does not supply that evidence")
        && named.notice.contains("existing matching sighting")
        && named.notice.contains("eligible measured execution observation")
        && named.notice.contains("(named_root_sighting_unavailable)")
        && !named.notice.contains(ENGRAM_SOURCE_ROOT_EVALUATION_NOTICE),
        "OFF naming must not promise a sighting from another ordinary turn: {}", named.notice);
    let saved = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
    assert_eq!(saved.engram_work_source_roots.len(), 1);
    assert_eq!(saved.engram_work_source_roots[0].generation, named.generation);
    assert_eq!(Some(&saved.engram_work_source_roots[0].root), named.root.as_ref());
    {
        let inner = claimed.state.inner.lock().unwrap();
        assert!(AppState::engram_source_root_target_locked(
            &inner, &claimed.session_id).unwrap().is_some(),
            "Base work services remain independent of automatic observations");
        assert!(engram_root_reclamation_session_is_reader(
            &inner, &claimed.session_id, &saved.engram_work_source_roots[0].store));
    }

    let cleared = ordinary_naming_request(&claimed, label, None)
        .expect("the Base-only holder can clear its name");
    assert!(cleared.root.is_none());
    assert_eq!(cleared.generation, 0);
    assert!(cleared.sealed.is_none());
    assert_eq!(cleared.evaluation_notice, None);
    let saved = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(saved.engram_work_source_roots.is_empty());
    claimed.record_mut(|record| {
        assert!(!record.engram.historical_control.contains_key("engramActiveGrantId"));
        assert!(record.engram.observed_source_root().is_none());
    });
    assert!(claimed.transport.requests().iter().all(|request| !matches!(
        request.request["operation"].as_str(),
        Some("turn_evaluate" | "turn_begin" | "turn_checkpoint")
    )), "naming never manufactures a mediated turn");
    assert_eq!(claimed.transport.requests().iter().filter(|request|
        request.request["operation"] == "session_bind").count(), 0,
        "ordinary naming never creates a control binding");
    claimed.record_mut(|record| assert!(!record.engram.historical_control.contains_key("engramRoutingToken")));
}

fn ordinary_naming_reply(claimed: &EOCheckedTurn) -> Value {
    json!({"project_id": "github.com/example/source-root",
        "session_id": claimed.session_id, "work_session": true,
        "capabilities": ["host_work_v1"]})
}

// Configure explicit ordinary-child and canonical held-claim reads without
// installing any control credential or local work association.
fn script_ordinary_work_session(claimed: &EOCheckedTurn, label: &str) {
    script_work_session_reply(claimed, label, Ok(ordinary_naming_reply(claimed)));
}

fn script_work_session_reply(
    claimed: &EOCheckedTurn, label: &str, reply: Result<Value, EngramTransportError>,
) {
    change_engram_settings(&claimed.state, |engram| engram.execution_observability = false);
    claimed.record_mut(|record| record.engram.work_binding = None);
    naming_store(claimed);
    let held = naming_claim(label);
    claimed.transport.enable_named_roots(
        &claimed.session_id, held.items[0].control_binding.as_ref().unwrap());
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .work_session_read_reply = Some(reply);
    let binding = held.items[0].control_binding.as_ref().unwrap().clone();
    *claimed.transport.work_bindings.lock().unwrap() = (0..4)
        .map(|_| Ok(Some(binding.clone()))).collect();
    claimed.transport.replace_held_claims([Ok(held)]);
}

fn ordinary_naming_request(
    claimed: &EOCheckedTurn, label: &str, path: Option<&FsPath>,
) -> Result<EngramSourceRootResponse, ApiError> {
    claimed.state.name_engram_source_root(&claimed.session_id, EngramSourceRootRequest {
        work: format!("w-{label}"),
        path: path.map(|path| Some(path.to_string_lossy().into_owned())),
    })
}

#[test]
fn source_root_unavailable_work_read_retries_without_a_control_key_or_grant() {
    let label = "base-only-lost-bind";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    script_work_session_reply(&claimed, label,
        Err(EngramTransportError::transport("work read unavailable")));
    let error = ordinary_naming_request(&claimed, label, Some(&worktree)).unwrap_err();
    assert!(error.message.contains("work read unavailable"), "{error:?}");
    claimed.record_mut(|record| {
        assert!(!record.engram.historical_control.contains_key("engramRoutingToken"));
        assert!(record.engram.work_binding.is_none());
        assert!(!record.engram.historical_control.contains_key("engramBindInProgress"));
        assert!(record.engram.source_root_bind_owner.is_none());
    });
    assert!(claimed.state.inner.lock().unwrap().engram_work_source_roots.is_empty());
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .work_session_read_reply = Some(Ok(ordinary_naming_reply(&claimed)));
    assert!(ordinary_naming_request(&claimed, label, Some(&worktree)).unwrap().root.is_some());
    let requests = claimed.transport.requests();
    let reads = requests.iter().filter(|request| request.request["operation"] == "work_session_read")
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 2);
    assert_eq!(reads[0].request, reads[1].request, "same ordinary identity read");
    assert_eq!(reads[0].connection, reads[1].connection);
    assert!(reads[0].request.get("idempotency_key").is_none());
    assert!(requests.iter().all(|request| request.request["operation"] != "session_bind"));
    assert!(requests.iter().all(|request| !matches!(request.request["operation"].as_str(),
        Some("turn_evaluate" | "turn_begin" | "turn_checkpoint"))));
}

#[test]
fn source_root_refuses_legacy_or_foreign_work_read_replies_without_adopting_them() {
    for case in ["legacy", "session", "project", "work-session", "capability"] {
        let label = format!("base-only-open-{case}");
        let claimed = EOCheckedTurn::idle(&label, true);
        let mut reply = ordinary_naming_reply(&claimed);
        match case {
            "legacy" => reply = json!({"routing_token":"must-not-be-adopted", "status":{"phase":"turn_open"}}),
            "session" => reply["session_id"] = json!("another-session"),
            "project" => reply["project_id"] = json!("another-project"),
            "work-session" => reply["work_session"] = json!(false),
            "capability" => reply["capabilities"] = json!([]),
            _ => unreachable!(),
        }
        script_work_session_reply(&claimed, &label, Ok(reply));
        let error = ordinary_naming_request(&claimed, &label, Some(&claimed.root)).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_GATEWAY, "{case}: {error:?}");
        assert!(!claimed.transport.requests().iter().any(|request|
            request.request["operation"] == "named_root_bind"), "{case}: no write after failed identity proof");
        claimed.record_mut(|record| {
            assert!(!record.engram.historical_control.contains_key("engramRoutingToken"));
            assert!(record.engram.work_binding.is_none());
            assert!(!record.engram.historical_control.contains_key("engramActiveGrantId"));
            assert!(!record.engram.historical_control.contains_key("engramBindInProgress"));
        });
        assert!(claimed.state.inner.lock().unwrap().engram_work_source_roots.is_empty());
    }
}

#[test]
fn ordinary_naming_preserves_unresolved_control_history_without_rebinding_it() {
    for (case, key, value) in [
        ("active", "engramActiveGrantId", json!("keep-active")),
        ("uncertain", "engramUncertainGrantId", json!("keep-uncertain")),
        ("legacy", "engramBeginsRecorded", json!(false)),
        ("checkpoint", "engramCheckpointInProgress", json!(true)),
        ("binding", "engramBindInProgress", json!(true)),
    ] {
        let label = format!("base-only-unresolved-{case}");
        let claimed = EOCheckedTurn::idle(&label, true);
        script_ordinary_work_session(&claimed, &label);
        claimed.record_mut(|record| { record.engram.historical_control.insert(key.into(), value); });
        let before = claimed.record_mut(|record| record.engram.historical_control.clone());
        let named = ordinary_naming_request(&claimed, &label, Some(&claimed.root)).unwrap();
        assert!(named.root.is_some(), "{case}");
        assert_eq!(before, claimed.record_mut(|record| record.engram.historical_control.clone()));
        assert_ordinary_turn_has_no_native_requests(&claimed);
    }
}

#[test]
fn source_root_base_only_preserves_base_reset_and_child_shape_refusals() {
    for case in ["base-off", "reset", "child"] {
        let label = format!("base-only-guard-{case}");
        let claimed = EOCheckedTurn::idle(&label, true);
        script_ordinary_work_session(&claimed, &label);
        match case {
            "base-off" => change_engram_settings(&claimed.state, |settings| settings.enabled = false),
            "reset" => {
                let project_id = claimed.record_mut(|record| record.session.project_id.clone().unwrap());
                assert!(claimed.state.inner.lock().unwrap().engram_project_resets.claim(&project_id).is_some());
            }
            "child" => claimed.record_mut(|record| record.session.parent_delegation_id = Some("missing-child-link".to_owned())),
            _ => unreachable!(),
        }
        assert_eq!(ordinary_naming_request(&claimed, &label, Some(&claimed.root))
            .unwrap_err().status, StatusCode::CONFLICT, "{case}");
        assert!(claimed.transport.requests().is_empty(), "{case}: nothing sent to control");
        assert!(claimed.state.inner.lock().unwrap().engram_work_source_roots.is_empty());
    }
}

#[test]
fn a_name_is_refused_when_base_mcp_is_off_or_the_project_changes_meanwhile() {
    let label = "naming-project-fence";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let nothing_named = |claimed: &EOCheckedTurn, case: &str| {
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

    // Base disablement still refuses naming, independently of turn gating.
    change_engram_settings(&claimed.state, |engram| {
        engram.enabled = false;
        engram.execution_observability = false;
    });
    let error = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .expect_err("Base MCP is off");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("not enabled"), "{}", error.message);
    nothing_named(&claimed, "Base MCP off");
    change_engram_settings(&claimed.state, |engram| {
        engram.enabled = true;
        engram.execution_observability = true;
    });

    // A store or Base-service change during the read invalidates the captured
    // authority. The observation toggle is independent of this service.
    let original_store = naming_store(&claimed);
    let changes: [(&str, fn(&mut EngramProjectSettings)); 2] = [
        ("another store", |engram| {
            if let Some(store) = engram.authority_store_key.as_mut() {
                store.project_id = "github.com/example/another".to_owned();
            }
        }),
        ("Base turned off", |engram| {
            engram.enabled = false
        }),
    ];
    for (case, change) in changes {
        let state = claimed.state.clone();
        TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || change_engram_settings(&state, change)));
        });
        let error = request_naming(
            &claimed,
            label,
            Some(&worktree),
            vec![naming_claim(label)],
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
        change_engram_settings(&claimed.state, |engram| {
            engram.authority_store_key = Some(original_store.clone());
            engram.enabled = true;
            engram.execution_observability = true;
        });
    }
}

/// The claimed root's turn record: its grant, and the revision sealed on its
/// root, if any.
fn turn_seal(claimed: &EOCheckedTurn) -> (Option<String>, Option<String>) {
    claimed.record_mut(|record| (
        record.engram.observed_turn_ref().map(str::to_owned),
        record.engram.observed_source_root().and_then(|root| root.sealed_revision.clone()),
    ))
}

#[test]
fn a_clear_after_the_turn_finished_seals_nothing() {
    let label = "ordinary-clear-after-finish";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    claimed.begin("Observe before clearing.");
    assert_ordinary_observation_has_no_native_credit(&claimed, &claimed.finish());
    let cleared = request_naming(&claimed, label, None, vec![naming_claim(label)]).unwrap();
    assert!(cleared.sealed.is_none());
    assert_eq!(turn_seal(&claimed), (None, None), "no completed observation can receive a later seal");
}

#[test]
fn a_clear_seals_no_ordinary_turn_started_after_the_call_began() {
    let label = "ordinary-clear-later-turn";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    claimed.begin("The original ordinary turn.");
    let original = claimed.record_mut(|record| record.engram.observed_turn_ref().unwrap().to_owned());
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    let runtime = claimed._runtime_rx.clone();
    TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let token = {
                let inner = state.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&session).unwrap()].runtime.runtime_token().unwrap()
            };
            state.finish_turn_ok_if_runtime_matches(&session, &token).unwrap();
            let dispatch = match state.dispatch_turn(&session, SendMessageRequest {
                text: "The successor ordinary turn.".into(), expanded_text: None, attachments: Vec::new(),
                source_session_id: None, source_mailbox: None,
            }).unwrap() {
                DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
                DispatchTurnResult::Queued => panic!("the real successor must dispatch"),
            };
            deliver_turn_dispatch(&state, dispatch).unwrap();
            assert!(matches!(receive(&runtime.lock().unwrap(), "successor provider handoff"),
                CodexRuntimeCommand::Prompt { .. }));
        }));
    });
    let cleared = request_naming(&claimed, label, None, vec![naming_claim(label)]).unwrap();
    assert!(TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| hook.borrow().is_none()));
    assert!(cleared.sealed.is_none());
    let (successor, seal) = turn_seal(&claimed);
    assert!(successor.is_some());
    assert_ne!(successor.as_deref(), Some(original.as_str()));
    assert!(seal.is_none());
    assert_ordinary_turn_has_no_native_requests(&claimed);
    claimed.end();
}

#[cfg(windows)]
#[test]
fn a_git_bash_drive_spelling_names_the_worktree() {
    // Git Bash's `pwd` prints `/c/…` for `C:/…`; read as written it would be
    // joined to the workdir.
    let label = "naming-msys-drive";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let canonical = fs::canonicalize(&worktree).expect("the worktree canonicalizes");
    let windows = engram_source_root_display(&canonical.to_string_lossy()).replace('\\', "/");
    let (drive, rest) = windows.split_once(':').expect("a drive path");
    let msys = format!("/{}{rest}", drive.to_ascii_lowercase());

    let named = request_naming(
        &claimed,
        label,
        Some(FsPath::new(&msys)),
        vec![naming_claim(label)],
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
fn other_session(claimed: &EOCheckedTurn) -> String {
    let project_id = claimed.record_mut(|record| {
        record
            .session
            .project_id
            .clone()
            .expect("the claimed root has a project")
    });
    create_test_project_session(&claimed.state, Agent::Codex, &project_id, &claimed.root)
}

/// A full list, every entry named by `named_by` for a claim that has ended.
fn fill_list(claimed: &EOCheckedTurn, store: &EngramAuthorityStoreKey, named_by: &str) {
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let store = naming_store(&claimed);
    let other = other_session(&claimed);
    fill_list(&claimed, &store, &other);
    let error = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label), no_held_claims()],
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let store = naming_store(&claimed);
    let other = other_session(&claimed);
    fill_list(&claimed, &store, &other);
    let mut binding = test_control_work_binding("ended", 1);
    binding.work_id = "work-ended-0".to_owned();
    binding.claim_id = "claim-ended-0".to_owned();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&other).unwrap();
        inner.sessions[index].engram.work_binding = Some(binding.clone());
        inner.sessions[index].engram.historical_control.insert("engramRoutingToken".into(), json!("other-token"));
    }
    claimed
        .canonical_root_readback(
            &other,
            Some(&binding),
            Some(EngramNamedRootState::None),
            1,
        )
        .unwrap();
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
fn source_root_journal_keeps_a_real_cached_observation_until_its_last_reference_ends() {
    let label = "journal-cached-ordinary-observation";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    claimed.begin("Observe this named root.");
    let observed = claimed.finish();
    assert_ordinary_observation_has_no_native_credit(&claimed, &observed);
    request_naming(&claimed, label, None, vec![naming_claim(label)]).unwrap();
    // Use the real persisted image for the negative reference control,
    // without deleting live in-flight observation custody.
    let mut image = PersistedState::from_inner(&claimed.state.inner.lock().unwrap()).into_inner().unwrap();
    let index = image.find_session_index(&claimed.session_id).unwrap();
    assert!(!image.sessions[index].engram.unadmitted_reports.is_empty());
    image.sessions[index].engram.work_binding = None;
    image.sessions[index].engram.unadmitted_active_turn = None;
    image.sessions[index].engram.unadmitted_closing_jobs.clear();
    image.sessions[index].engram.active_turn_checks.clear();
    engram_compact_root_journal(&mut image);
    assert_eq!(image.engram_named_root_journal.len(), 1, "the actual immutable observation pins its claim");
    image.sessions[index].engram.unadmitted_reports.clear();
    engram_compact_root_journal(&mut image);
    assert!(image.engram_named_root_journal.is_empty());
}

#[test]
fn source_root_journal_reclaims_released_history_only_after_authoritative_readback() {
    let label = "journal-released-history";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let named = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .unwrap();
    let (binding,) = claimed.record_mut(|record| {
        (
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
        .canonical_root_readback(
            &claimed.session_id,
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    request_naming(&claimed, label, None, vec![naming_claim(label)]).unwrap();
    let restored = load_state(claimed.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(restored.engram_source_root_generation, high_water);
    assert_eq!(restored.engram_named_root_journal.len(), 1);
    let renamed = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let error = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
fn a_claim_tells_the_agent_where_its_ordinary_turns_are_measured_in_its_first_prompt() {
    let label = "naming-bind-line";
    let claimed = EOCheckedTurn::idle(label, true);
    // A real public name and clear establish the producer's canonical absence.
    let worktree = naming_worktree(&claimed.root);
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    request_naming(&claimed, label, None, vec![naming_claim(label)]).unwrap();
    let prompt = claimed.begin_prompt("Explain the current source basis.").prompt;
    assert!(prompt.contains(&format!("Engram source basis for w-{label}:"))
        && prompt.contains("no worktree named"), "{prompt}");
    assert_eq!(pending_line(&claimed), None, "a delivered line is not repeated");
    assert_ordinary_turn_has_no_native_requests(&claimed);
    claimed.end();
}

#[test]
fn a_line_set_after_the_prompt_was_built_waits_for_the_next_prompt() {
    let label = "naming-late-line";
    let claimed = EOCheckedTurn::idle(label, true);
    let dispatch = match claimed.state.dispatch_turn(&claimed.session_id, SendMessageRequest {
        text: "Deliver the pending source line.".into(), expanded_text: None,
        attachments: Vec::new(), source_session_id: None, source_mailbox: None,
    }).unwrap() { DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("the idle ordinary root must dispatch") };
    // Set between the prompt's build and its delivery, as a later naming request does.
    claimed.record_mut(|record| {
        record
            .engram
            .set_pending_source_root_line("[TermAl] a later line".to_owned());
    });

    deliver_turn_dispatch(&claimed.state, dispatch)
        .expect("the begun root turn should reach the runtime");

    let prompt = delivered_naming_prompt(&claimed);
    assert!(!prompt.contains("a later line"), "{prompt}");
    assert_eq!(
        pending_line(&claimed).as_deref(),
        Some("[TermAl] a later line"),
        "the later line is kept for the next prompt, not lost"
    );
    assert_ordinary_turn_has_no_native_requests(&claimed);
    claimed.end();
}

#[test]
fn a_line_the_prompt_carried_given_again_after_its_build_stays_the_last_line() {
    // The prompt carries a name; before the runtime accepts it the name is
    // cleared and given again. The acceptance must not take the name given
    // again, or the clear would end the next prompt's lines.
    let label = "naming-line-again";
    let claimed = EOCheckedTurn::idle(label, true);
    claimed.record_mut(|record| {
        record
            .engram
            .set_pending_source_root_line("[TermAl] named".to_owned());
    });
    let dispatch = match claimed.state.dispatch_turn(&claimed.session_id, SendMessageRequest {
        text: "Deliver the pending source line.".into(), expanded_text: None,
        attachments: Vec::new(), source_session_id: None, source_mailbox: None,
    }).unwrap() { DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("the idle ordinary root must dispatch") };
    claimed.record_mut(|record| {
        record
            .engram
            .set_pending_source_root_line("[TermAl] cleared".to_owned());
        record
            .engram
            .set_pending_source_root_line("[TermAl] named".to_owned());
    });

    deliver_turn_dispatch(&claimed.state, dispatch)
        .expect("the begun root turn should reach the runtime");

    let prompt = delivered_naming_prompt(&claimed);
    assert!(prompt.contains("[TermAl] named"), "{prompt}");
    assert!(!prompt.contains("[TermAl] cleared"), "{prompt}");
    assert_eq!(
        pending_line(&claimed).as_deref(),
        Some("[TermAl] cleared\n[TermAl] named"),
        "the next prompt tells the clear and ends on the name given again"
    );
    assert_ordinary_turn_has_no_native_requests(&claimed);
    claimed.end();
}

#[test]
fn historical_control_disablement_does_not_drop_an_ordinary_waiting_line() {
    let claimed = EOCheckedTurn::idle("naming-passive-disablement", true);
    claimed.record_mut(|record| {
        record.engram.set_pending_source_root_line("[TermAl] a waiting line".into());
        record.engram.historical_control.insert("engramDisabledReason".into(), json!("unparsable historical control response"));
    });
    let history = claimed.record_mut(|record| record.engram.historical_control.clone());
    assert_eq!(pending_line(&claimed).as_deref(), Some("[TermAl] a waiting line"));
    let prompt = claimed.begin_prompt("Continue the ordinary turn.").prompt;
    assert!(prompt.contains("[TermAl] a waiting line"), "{prompt}");
    assert_eq!(pending_line(&claimed), None);
    claimed.record_mut(|record| assert_eq!(record.engram.historical_control, history));
    assert_ordinary_turn_has_no_native_requests(&claimed);
    claimed.end();
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
fn a_clear_after_an_ordinary_opening_keeps_and_seals_its_original_root() {
    ordinary_opening_keeps_its_root("clear");
}

#[test]
fn a_rename_after_an_ordinary_opening_keeps_its_original_root() {
    ordinary_opening_keeps_its_root("rename");
}

#[test]
fn a_pending_rename_preserves_the_ordinary_opening_and_withholds_current_authority() {
    ordinary_opening_keeps_its_root("pending");
}

#[test]
fn an_unknown_read_preserves_the_ordinary_opening_and_withholds_current_authority() {
    ordinary_opening_keeps_its_root("unknown");
}

#[test]
fn a_delayed_canonical_read_cannot_replace_a_delivered_ordinary_successor() {
    let label = "ordinary-delayed-canonical-reader";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    let (target, fence) = {
        let inner = claimed.state.inner.lock().unwrap();
        (AppState::engram_source_root_target_locked(&inner, &claimed.session_id).unwrap().unwrap(),
            EngramRootReadFence::capture(&inner))
    };
    claimed.begin("The first real provider turn.");
    let first = claimed.record_mut(|record| record.engram.observed_turn_ref().unwrap().to_owned());
    assert_ordinary_observation_has_no_native_credit(&claimed, &claimed.finish());
    claimed.begin("The delivered successor.");
    let successor = claimed.record_mut(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    assert_ne!(successor.host_turn_ref, first);
    claimed.state.reconcile_engram_named_root_with_route_classified_until(
        &claimed.session_id, target.work_binding.as_ref(), Some(EngramNamedRootState::None),
        &fence, Some(&target.connection), claimed.state.engram_budget_clock().now() + Duration::from_secs(2),
    ).unwrap_or_else(|failure| panic!("canonical root reconciliation failed: {}", failure.into_transport()));
    claimed.record_mut(|record| assert_eq!(record.engram.unadmitted_active_turn.as_ref(), Some(&successor)));
    assert_ordinary_observation_has_no_native_credit(&claimed, &claimed.finish());
}

fn ordinary_opening_keeps_its_root(transition: &str) {
    let label = format!("ordinary-opening-{transition}");
    let claimed = EOCheckedTurn::idle(&label, true);
    let worktree = naming_worktree(&claimed.root);
    let named = request_naming(&claimed, &label, Some(&worktree), vec![naming_claim(&label)]).unwrap();
    claimed.begin("Observe this turn's original root.");
    let (turn_ref, opening) = claimed.record_mut(|record| (
        record.engram.observed_turn_ref().unwrap().to_owned(),
        record.engram.observed_opening().unwrap().clone(),
    ));
    let before = content_revision_of(&worktree);
    let binding = opening.binding.clone();
    match transition {
        "clear" => { request_naming(&claimed, &label, None, vec![naming_claim(&label)]).unwrap(); }
        "rename" => { request_naming(&claimed, &label, Some(&claimed.root), vec![naming_claim(&label)]).unwrap(); }
        "pending" => {
            claimed.transport.named_roots.lock().unwrap().as_mut().unwrap().lose_next_reply = true;
            request_naming(&claimed, &label, Some(&claimed.root), vec![naming_claim(&label)]).unwrap_err();
            assert!(claimed.state.inner.lock().unwrap().engram_named_root_journal[0].pending.is_some());
        }
        "unknown" => {
            claimed.canonical_root_readback(&claimed.session_id, Some(&binding),
                Some(EngramNamedRootState::Unknown), u64::MAX).unwrap();
        }
        _ => unreachable!(),
    }
    claimed.record_mut(|record| {
        assert_eq!(record.engram.observed_turn_ref(), Some(turn_ref.as_str()));
        let current = record.engram.observed_opening().unwrap();
        assert_eq!(current.provenance, opening.provenance);
        assert_eq!(current.binding, opening.binding);
        let root = record.engram.observed_source_root().unwrap();
        assert_eq!(root.generation, named.generation);
        assert_eq!(Some(&root.root), named.root.as_ref());
        assert_eq!(root.sealed_revision.is_some(), matches!(transition, "clear" | "rename"));
    });
    assert_eq!(content_revision_of(&worktree), before, "naming metadata does not synthesize a content edit");
    if matches!(transition, "pending" | "unknown") {
        assert_eq!(claimed.state.engram_turn_root_capture(&claimed.session_id), EngramRootCapture::Unconfirmed);
    } else {
        assert_eq!(claimed.state.engram_turn_root_capture(&claimed.session_id), opening.provenance);
    }
    assert_ordinary_turn_has_no_native_requests(&claimed);
    claimed.end();
}

#[test]
fn a_watcher_event_in_a_named_root_beside_the_workdir_counts_only_during_its_ordinary_turn() {
    let label = "naming-watcher-sibling";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let canonical = fs::canonicalize(&worktree).expect("the worktree canonicalizes");
    // The session works in a directory beside the named root, not above it.
    let workdir = claimed.root.join("beside");
    fs::create_dir_all(&workdir).expect("the workdir exists");
    claimed.record_mut(|record| {
        record.session.workdir = engram_source_root_display(
            &fs::canonicalize(&workdir)
                .expect("the workdir canonicalizes")
                .to_string_lossy(),
        );

    });
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    claimed.begin("Observe edits in the named root.");
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
        claimed.record_mut(|record| {
            record
                .active_turn_file_changes
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        }),
        [in_root.clone()],
        "the turn's root counts; a path outside it and the workdir does not"
    );

    assert_ordinary_observation_has_no_native_credit(&claimed, &claimed.finish());
    claimed.record_mut(|record| record.active_turn_file_changes.clear());
    claimed
        .state
        .record_active_turn_file_changes(&events(&[&in_root]));
    assert!(
        claimed.record_mut(|record| record.active_turn_file_changes.is_empty()),
        "a completed ordinary trace cannot own a later watcher event"
    );
}

#[test]
fn the_naming_route_answers_with_the_handlers_statuses() {
    let label = "naming-route";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    naming_store(&claimed);
    claimed
        .transport
        .replace_held_claims([Ok(naming_claim(label))]);
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let canonical = fs::canonicalize(&worktree).expect("the worktree canonicalizes");
    let project_root = fs::canonicalize(&claimed.root).expect("the root canonicalizes");
    let other = other_session(&claimed);
    let workdir = claimed.root.join("beside");
    fs::create_dir_all(&workdir).expect("the workdir exists");
    claimed.record_mut(|record| {
        record.session.workdir = engram_source_root_display(
            &fs::canonicalize(&workdir)
                .expect("the workdir canonicalizes")
                .to_string_lossy(),
        );

    });
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    claimed.begin("The ordinary named turn receives watcher scopes.");
    let scope_root = canonical_workspace_file_watch_root(&project_root.to_string_lossy())
        .expect("the real watcher scope exists");
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
    let canonical_edited = canonical.join("README.md");
    let edited = normalize_user_facing_path(&canonical_edited);
    let events = [&canonical_edited, &edited]
        .into_iter()
        .flat_map(|path| {
            workspace_file_changes_from_path(path, WorkspaceFileChangeKind::Modified, &scopes)
        })
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 4, "both aliases reach both real watcher scopes");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.session_id.as_deref() == Some(other.as_str()))
            .count(),
        2,
        "each alias has a copy attributed to the other session"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.session_id.is_none())
            .count(),
        2,
        "each alias also has its unscoped duplicate"
    );
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
        claimed.record_mut(|record| {
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
    // A sibling sharing the root's text prefix is still outside the root.
    let outside = canonical.with_file_name("wt-outside").join("README.md");
    let outside_events =
        workspace_file_changes_from_path(&outside, WorkspaceFileChangeKind::Modified, &scopes);
    assert_eq!(outside_events.len(), 2);
    claimed.state.record_active_turn_file_changes(&outside_events);
    claimed.record_mut(|record| {
        assert_eq!(
            record.engram.observed_source_root().unwrap().root,
            canonical.to_string_lossy(),
            "watcher comparison never changes the canonical named-root identity"
        );
    });
    // The set is the turn's "files changed" summary, so the card lists it.
    let card = claimed.record_mut(|record| {
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
fn an_ordinary_named_opening_marks_a_check_already_open_in_that_root() {
    // The delivered ordinary opening installs its named root and marks another
    // session's check that is still open in that root.
    let label = "naming-admission-overlap";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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

    let dispatch = match claimed.state.dispatch_turn(&claimed.session_id, SendMessageRequest {
        text: "Observe the named root.".into(), expanded_text: None, attachments: Vec::new(),
        source_session_id: None, source_mailbox: None,
    }).unwrap() {
        DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("the idle ordinary root must dispatch"),
    };
    assert!(
        !overlapped(),
        "the turn-start sweep sees only the workdir, a worktree of its own"
    );
    deliver_turn_dispatch(&claimed.state, dispatch)
        .expect("the begun root turn should reach the runtime");
    let _ = delivered_naming_prompt(&claimed);

    assert!(
        overlapped(),
        "the ordinary opening that installs the root marks the check"
    );
    pending.finish(None);
    assert_ordinary_turn_has_no_native_requests(&claimed);
    claimed.end();
}

#[test]
fn a_delegated_session_is_not_told_to_name_a_root_it_cannot_name() {
    let root_line = engram_source_root_line(None, "ordinary-work", "C:/root", false);
    assert!(root_line.contains("termal_name_source_root"));
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let store = naming_store(&claimed);
    let held = naming_claim(label);
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

    let error = request_naming(&claimed, label, Some(&worktree), vec![held])
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .expect("named");
    request_naming(&claimed, label, None, vec![naming_claim(label)]).expect("cleared");
    request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
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
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    naming_store(&claimed);
    let binding = naming_claim(label).items[0]
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
        .replace_held_claims([Ok(naming_claim(label))]);
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
        let claimed = EOCheckedTurn::idle(&label, true);
        let first = naming_worktree(&claimed.root);
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
        let mut held = naming_claim(&label);
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
        request_naming(&claimed, &label, Some(&first), vec![held.clone()]).expect("first root named");
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
        request_naming(&claimed, &label, Some(&first), vec![held.clone()])
            .expect("explicitly select the first claim through the current naming owner");
        if case == "expired" {
            held.items.pop();
            claimed.transport.replace_held_claims([Ok(held)]);
        }
        claimed.begin("Observe placement for the current held claim.");
        claimed.record_mut(|record| record.engram.pending_source_root_line = None);
        claimed.state.note_engram_command_started(
            &claimed.session_id,
            &EngramObservationProvenance::Ambient,
            "test",
            Some("cargo test"),
            Some(second.to_str().expect("UTF-8 fixture path")),
        );
        let line = pending_line(&claimed).expect("test routing is explained");
        let checks = claimed.record_mut(|record| record.engram.active_turn_checks.len());
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
            .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.record_mut(|record| record.runtime.runtime_token().unwrap()))
            .expect("the ordinary turn closes without rerouting its facts");
        assert_ordinary_turn_has_no_native_requests(&claimed);
    }
}

#[test]
fn ordinary_root_checks_carry_the_shared_fixture_toolchain_label_settled_at_start() {
    let label = "ordinary-fixture-toolchain";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    claimed.begin("Observe the toolchain-labelled check.");
    let mut recorder = claimed.recorder();
    let cwd = worktree.to_string_lossy();
    recorder.command_started_in("fixture-toolchain-test", "cargo test", Some("cargo test"), Some(&cwd)).unwrap();
    let toolchain = claimed.record_mut(|record| {
        assert_eq!(record.engram.active_turn_checks.len(), 1);
        record.engram.active_turn_checks[0].toolchain.clone()
    });
    assert!(toolchain.is_ready());
    assert_eq!(toolchain.wait_until(std::time::Instant::now()),
        Some(Some("rustc 1.90.0 (fixture 2025-09-14); cargo 1.90.0 (fixture 2025-07-30)".into())));
    recorder.command_completed_with_exit("fixture-toolchain-test", "cargo test",
        "test result: ok. 1 passed", CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    claimed.wait_for_snapshots();
    assert_ordinary_observation_has_no_native_credit(&claimed, &claimed.finish());
}

#[test]
fn ordinary_capture_owners_keep_fixture_files_alive_after_the_turn_wrapper_drops() {
    let claimed = EOCheckedTurn::idle("ordinary-capture-lifetime", true);
    let temp_root = claimed.state.test_temp_root.as_ref().unwrap().path().to_owned();
    let workers = claimed.record_mut(|record| record.engram.capture_workers.clone());
    let state_owner = claimed.state.clone();
    let worker_root = temp_root.clone();
    let (ready, started) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let capture = EngramCapture::spawn(&workers, &Arc::new(CaptureLifetime::default()), move || {
        ready.send(()).unwrap();
        released.recv_timeout(DEADLOCK_GUARD).unwrap();
        let exists = worker_root.exists();
        drop(state_owner);
        exists
    });
    started.recv_timeout(DEADLOCK_GUARD).unwrap();
    drop(claimed);
    assert!(temp_root.exists(), "the actual AppState capture owner retains the temporary files");
    release.send(()).unwrap();
    assert_eq!(capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD), Some(true));
    assert!(!temp_root.exists(), "the last actual state owner releases the temporary root");
}

#[test]
fn focusing_another_named_claim_selects_its_next_ordinary_opening_and_check_facts() {
    let label = "focused-named-claim";
    let claimed = EOCheckedTurn::idle(label, true);
    let first = naming_worktree(&claimed.root);
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
    let mut held = naming_claim(label);
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
    request_naming(&claimed, label, Some(&first), vec![held.clone()]).expect("first root named");
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
    request_naming(&claimed, label, Some(&first), vec![held.clone()])
        .expect("explicitly select the first claim before its ordinary opening");
    claimed.begin("The first ordinary claim owns this opening.");
    claimed.record(|record| assert_eq!(record.engram.work_binding, Some(first_binding.clone())));
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.record_mut(|record| record.runtime.runtime_token().unwrap()))
        .expect("first turn completes");

    held.items[0].focused = false;
    held.items[1].focused = true;
    let selected = select_engram_held_binding(
        held.items.clone(),
        held.omitted,
        EngramBindingPreference {
            current: Some(&first_binding),
        },
    );
    assert_eq!(
        selected,
        Some(second_binding.clone()),
        "focus wins over current binding"
    );
    claimed.transport.set_current_work_binding(selected.clone());
    claimed.transport.enable_named_roots(&claimed.session_id, &second_binding);
    claimed.record_mut(|record| record.engram.work_binding = selected);
    claimed.transport.replace_held_claims([Ok(held)]);
    claimed.begin("A subsequent ordinary opening uses the explicitly selected association.");
    claimed.record_mut(|record| {
        assert_eq!(record.engram.work_binding, Some(second_binding.clone()));
        assert_eq!(
            record
                .engram
                .observed_source_root()
                .map(|root| root.short_ref.as_str()),
            Some("w-second")
        );
    });
    let expected_workspace = fs::canonicalize(&second).unwrap().to_string_lossy().into_owned();
    let opening = claimed.record(|record| record.engram.observed_opening().unwrap().provenance.clone());
    let EngramRootCapture::Recorded { generation, state, workspace_id } = opening else {
        panic!("the focused opening has an acknowledged named root");
    };
    assert_eq!(state, EngramSourceRootState::Named);
    assert_eq!(workspace_id, expected_workspace);
    let wait = || {
        let captures = claimed.record_mut(|record| {
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
        captures.into_iter().map(|capture|
            capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD))
            .collect::<Vec<_>>()
    };
    claimed.state.note_engram_command_started(
        &claimed.session_id,
        &EngramObservationProvenance::Ambient,
        "focused-test",
        Some("cargo test"),
        Some(second.to_str().expect("UTF-8 fixture path")),
    );
    let _ = wait();
    assert_eq!(
        claimed.record_mut(|record| record.engram.active_turn_checks.len()),
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
    let bases = wait();
    assert_eq!(bases.len(), 2, "the actual check owns its start and end measurements");
    for (phase, basis) in ["start", "end"].into_iter().zip(bases) {
        let basis = basis.expect("the existing capture wait completed")
            .expect("the actual check source was measured");
        assert_eq!(basis.workspace_id, expected_workspace, "{phase}: the check measured B");
    }
    let turn_ref = claimed.record(|record| record.engram.unadmitted_active_turn.as_ref()
        .unwrap().host_turn_ref.clone());
    // Read both real producer occurrences without changing the shared helper ABI.
    let (notices_tx, notices_rx) = mpsc::channel();
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .observation_notices = Some(notices_tx);
    claimed.end();
    let request = loop {
        let observed = receive(&notices_rx, "focused ordinary turn observation");
        if observed["occurrence"]["kind"] == "unadmitted_turn"
            && observed["occurrence"]["host_turn_ref"] == turn_ref {
            break observed;
        }
    };
    assert_eq!(request["binding"], json!(second_binding));
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["observed_result"], "passed");
    assert!(checks[0].get("source_basis").is_none(),
        "an ordinary passing check does not claim source verification");
    let sighting_key = format!("termal-source-sighting:{turn_ref}");
    let sighting = loop {
        let observed = receive(&notices_rx, "focused measured source sighting");
        if observed["occurrence"]["kind"] == "source_sighting"
            && observed["idempotency_key"] == sighting_key {
            break observed;
        }
    };
    assert_eq!(sighting["operation"], "execution_observe");
    assert_eq!(sighting["binding"], json!(second_binding));
    let basis = &sighting["occurrence"]["source_basis"];
    assert_eq!(basis["workspace_id"].as_str(), Some(expected_workspace.as_str()));
    assert_eq!(basis["source_root_generation"], json!(generation));
    assert_eq!(basis["source_root_state"], "named");
    assert!(sighting.get("verification_evidence").is_none());
    assert_ordinary_observation_has_no_native_credit(&claimed, &request);
}

#[test]
fn source_root_naming_says_the_root_takes_effect_next_turn_and_evaluation_needs_a_turn_in_it() {
    let label = "naming-notice-next-turn";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let named = request_naming(
        &claimed,
        label,
        Some(&worktree),
        vec![naming_claim(label)],
    )
    .unwrap();
    assert_eq!(named.evaluation_notice, Some(EngramSourceRootEvaluationNotice::NamedRootSightingRequired));
    assert_eq!(serde_json::to_value(&named).unwrap()["evaluationNotice"], "named_root_sighting_required");
    assert!(
        named.notice.contains("takes effect at your next turn")
            && named.notice.contains(
                EngramSourceRootEvaluationNotice::NamedRootSightingRequired.message()
            ),
        "the naming response says when the root applies and when evaluation can run: {}",
        named.notice
    );
}

#[test]
fn a_root_named_during_an_ordinary_turn_is_prospective_and_never_relabels_its_opening() {
    let label = "ordinary-in-turn-naming";
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    claimed.begin("Start before a root is named.");
    let original = claimed.record_mut(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    assert!(original.opening_root.as_ref().unwrap().source_root.is_none());
    let named = request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    claimed.record_mut(|record| {
        let current = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert_eq!(current.host_turn_ref, original.host_turn_ref);
        assert_eq!(current.opening_root, original.opening_root);
    });
    assert_ordinary_turn_has_no_native_requests(&claimed);
    claimed.end();
    claimed.begin("The name takes effect on the next real ordinary opening.");
    claimed.record_mut(|record| {
        assert_ne!(record.engram.observed_turn_ref(), Some(original.host_turn_ref.as_str()));
        let root = record.engram.observed_source_root().unwrap();
        assert_eq!(root.generation, named.generation);
        assert_eq!(Some(&root.root), named.root.as_ref());
    });
    assert_ordinary_observation_has_no_native_credit(&claimed, &claimed.finish());
}
// These fixtures use the real public naming owner and a real provider opening.
// The optional placement reader gets its own explicit held-claims replies.
fn ordinary_other_root_notice_fixture(
    label: &str,
) -> (EOCheckedTurn, PathBuf, EngramControlWorkBinding) {
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let mut held = naming_claim(label);
    let current = held.items[0].control_binding.clone().unwrap();
    request_naming(&claimed, label, Some(&worktree), vec![held.clone()]).unwrap();
    let other = test_control_work_binding(&format!("other-placement-{label}"), 1);
    held.items.push(EngramHeldClaim {
        work_id: other.work_id.clone(),
        short_ref: "w-other-placement".into(),
        claim_id: other.claim_id.clone(),
        claim_fence: other.claim_fence,
        focused: false,
        control_binding: Some(other.clone()),
    });
    claimed.transport.register_named_root_run(&other);
    claimed.transport.replace_held_claims([Ok(held)]);
    claimed.state.name_engram_source_root(&claimed.session_id, EngramSourceRootRequest {
        work: other.work_id.clone(),
        path: Some(Some(worktree.to_string_lossy().into_owned())),
    }).unwrap();
    assert_eq!(claimed.record(|record| record.engram.work_binding.clone()), Some(other.clone()),
        "explicit naming selects the requested held association");
    claimed.state.name_engram_source_root(&claimed.session_id, EngramSourceRootRequest {
        work: current.work_id.clone(),
        path: Some(Some(worktree.to_string_lossy().into_owned())),
    }).unwrap();
    assert_eq!(claimed.record(|record| record.engram.work_binding.clone()), Some(current),
        "explicitly naming the own claim restores this fixture's measured association");
    (claimed, worktree, other)
}

#[test]
fn a_failed_optional_other_held_root_read_preserves_the_own_root_check_without_a_false_notice() {
    let (claimed, worktree, _) = ordinary_other_root_notice_fixture("optional-placement-read-failure");
    claimed.begin("Observe an ordinary check in the current root.");
    claimed.record_mut(|record| record.engram.pending_source_root_line = None);
    claimed.transport.replace_held_claims([Err(
        EngramTransportError::transport("optional held-claims read unavailable"),
    )]);
    let mut recorder = claimed.recorder();
    let cwd = worktree.to_string_lossy();
    recorder.command_started_in("optional-placement-check", "cargo test", Some("cargo test"), Some(&cwd)).unwrap();
    claimed.wait_for_snapshots();
    claimed.record(|record| {
        assert_eq!(record.engram.active_turn_checks.len(), 1,
            "failure of optional explanatory context cannot suppress the own-root check");
        assert_eq!(record.engram.active_turn_checks[0].target.root, fs::canonicalize(&worktree).unwrap());
        assert_eq!(record.engram.unadmitted_active_turn.as_ref().unwrap().commands.len(), 1);
    });
    assert_eq!(pending_line(&claimed), None,
        "an unavailable held list cannot authorize a notice that another claim is held");
    recorder.command_completed_with_exit(
        "optional-placement-check", "cargo test", "running 1 test\ntest result: ok. 1 passed",
        CommandStatus::Success, EngramCommandExit::Code(0),
    ).unwrap();
    claimed.wait_for_snapshots();
    let request = claimed.finish();
    assert_eq!(request["occurrence"]["observed_checks"].as_array().unwrap().len(), 1);
    assert_ordinary_observation_has_no_native_credit(&claimed, &request);
}

#[test]
fn other_root_notice_is_current_component_rejects_generation_work_and_connection_successors() {
    // Narrow component control: the scripted held-read seam has no callback
    // representing concurrent provider handoff. This tests the production
    // reader's publication predicate, not an end-to-end interleaving or a
    // claim that an actual concurrent notice was published or suppressed.
    for case in ["generation", "work-association", "connection"] {
        let label = format!("notice-read-successor-{case}");
        let (claimed, _, other) = ordinary_other_root_notice_fixture(&label);
        claimed.begin("Capture the ordinary owner of the optional notice.");
        claimed.record_mut(|record| record.engram.pending_source_root_line = None);
        let captured = {
            let inner = claimed.state.inner.lock().unwrap();
            let index = inner.find_session_index(&claimed.session_id).unwrap();
            EngramOtherRootNoticeRead::capture(&inner, index).unwrap()
        };
        let held_roots = captured.read();
        assert_eq!(held_roots.len(), 1);
        assert_eq!(held_roots[0].claim_id, other.claim_id);
        {
            let inner = claimed.state.inner.lock().unwrap();
            assert!(captured.is_current(&inner, inner.find_session_index(&claimed.session_id).unwrap()));
        }
        match case {
            "generation" => claimed.record_mut(|record| record.active_turn_generation += 1),
            "work-association" => claimed.record_mut(|record| record.engram.work_binding = Some(other.clone())),
            "connection" => change_engram_settings(&claimed.state, |settings| {
                settings.binary_path = Some("successor-notice-engram.exe".into());
            }),
            _ => unreachable!(),
        }
        {
            let inner = claimed.state.inner.lock().unwrap();
            let index = inner.find_session_index(&claimed.session_id).unwrap();
            assert!(!captured.is_current(&inner, index),
                "{case}: completed held-read data cannot authorize publication for a successor");
        }
        assert_eq!(pending_line(&claimed), None,
            "the component probe itself publishes no notice");
        // Restore only the actual fields changed by this component control
        // so the original real provider turn can close through its owner.
        claimed.record_mut(|record| {
            record.active_turn_generation = captured.turn_generation;
            record.engram.work_binding = captured.target.work_binding.clone();
        });
        change_engram_settings(&claimed.state, |settings| *settings = captured.target.settings.clone());
        {
            let inner = claimed.state.inner.lock().unwrap();
            assert!(captured.is_current(&inner, inner.find_session_index(&claimed.session_id).unwrap()),
                "{case}: the unchanged original owner is the positive control");
        }
        assert_ordinary_turn_has_no_native_requests(&claimed);
        claimed.end();
    }
}
