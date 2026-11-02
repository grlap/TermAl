//! Ordinary provider outcomes, measured source facts and named-root cuts.
//! A runtime result is observed after execution; it grants no verification.
use super::super::run_git_test_command;
use super::*;

#[path = "engram_source_observations.rs"]
mod source_observations;
#[path = "engram_drift_reaches_store.rs"]
mod drift_reaches_store;
#[path = "engram_source_root_naming.rs"]
mod source_root_naming;

fn content_revision_of(root: &FsPath) -> String {
    content_revision(root).expect("the real content revision is available").1
}


fn observed_turn_ref(turn: &EOCheckedTurn) -> String {
    turn.record(|record| record.engram.unadmitted_active_turn.as_ref().unwrap().host_turn_ref.clone())
}

fn ordinary_turn_requests(turn: &EOCheckedTurn, turn_ref: &str) -> Vec<Value> {
    turn.transport.requests().into_iter().map(|sent| sent.request)
        .filter(|request| request["operation"] == "execution_observe"
            && request["occurrence"]["kind"] == "unadmitted_turn"
            && request["occurrence"]["host_turn_ref"] == turn_ref)
        .collect()
}

fn assert_observed_result(turn: &EOCheckedTurn, turn_ref: &str, expected: &str) -> Value {
    let request = turn.observation_for(turn_ref);
    assert_ordinary_observation_has_no_native_credit(turn, &request);
    assert_eq!(request["occurrence"]["outcome"], expected);
    assert_eq!(request["causality"]["kind"], "unknown");
    assert_eq!(ordinary_turn_requests(turn, turn_ref), [request.clone()]);
    request
}

fn wait_for_observation_delivery(turn: &EOCheckedTurn) {
    let guard = phase_sync::PollGuard::new();
    loop {
        if turn.record(|record| record.engram.unadmitted_active_turn.is_none()
            && record.engram.unadmitted_closing_jobs.is_empty()
            && !record.engram.unadmitted_worker_running
            && record.engram.unadmitted_reports.iter().all(|report| report.envelopes.iter()
                .all(|envelope| matches!(envelope.delivery, EngramRuntimeObservationDelivery::Recorded { .. })))) { break; }
        guard.wait("the actual ordinary observation delivery completes");
    }
}

// Explicit passive-history fixture, not an endpoint produced by current EO.
// Source/time are measured for this fixture and persisted through the real
// schema. Current ordinary receipts neither create nor advance this field.
fn install_passive_measured_baseline(turn: &EOCheckedTurn, root: &FsPath) -> EngramRuntimeCheckpointBasis {
    let measured = engram_place_source_basis(&EngramBasisPlace::Workdir(root.to_string_lossy().into_owned())).unwrap();
    let baseline = EngramRuntimeCheckpointBasis {
        binding: turn.record(|record| record.engram.work_binding.clone().unwrap()),
        endpoint: EngramMeasuredBaseline { workspace_id: measured.workspace_id,
            source_revision: measured.source_revision, observed_at: unadmitted_observation_timestamp() },
    };
    {
        let mut inner = turn.state.inner.lock().unwrap();
        inner.session_mut(&turn.session_id).unwrap().engram.runtime_checkpoint = Some(baseline.clone());
        turn.state.commit_locked(&mut inner).unwrap();
    }
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()];
    assert_eq!(record.engram.runtime_checkpoint.as_ref(), Some(&baseline));
    baseline
}

fn begin_with_passive_measured_baseline(label: &str) -> EOCheckedTurn {
    let turn = EOCheckedTurn::idle(label, true);
    turn.acknowledge_unnamed_root();
    install_passive_measured_baseline(&turn, &turn.root);
    turn.begin("Observe from the previously recorded source endpoint.");
    turn
}

fn add_observed_worktree(turn: &EOCheckedTurn) -> PathBuf {
    fs::write(turn.root.join(".gitignore"), ".worktrees/\n").unwrap();
    run_git_test_command(&turn.root, &["add", ".gitignore", ".engram-project"]);
    run_git_test_command(&turn.root, &["commit", "--quiet", "-m", "declare observed worktree"]);
    let worktree = turn.root.join(".worktrees").join("wt");
    run_git_test_command(&turn.root, &["worktree", "add", "--quiet", "-b", "wt", worktree.to_str().unwrap()]);
    worktree
}

fn name_observed_worktree(turn: &EOCheckedTurn, worktree: &FsPath) -> EngramSourceRootResponse {
    let work = turn.record(|record| record.engram.work_binding.as_ref().unwrap().work_id.clone());
    turn.state.name_engram_source_root(&turn.session_id, EngramSourceRootRequest {
        work, path: Some(Some(worktree.to_string_lossy().into_owned())),
    }).expect("ordinary work services name the actual worktree")
}

// Use the same ordinary fixture's AppState, transport and actual provider
// receiver. The production delegation owner creates the child and worktree.
fn ordinary_delegation_child(label: &str, isolated: bool) -> EOCheckedTurn {
    let parent = EOCheckedTurn::idle(label, true);
    run_git_test_command(&parent.root, &["add", ".engram-project"]);
    run_git_test_command(&parent.root, &["commit", "--quiet", "-m", "declare observed child project"]);
    let policy = if isolated {
        DelegationWritePolicy::IsolatedWorktree {
            base: None, owned_paths: vec!["src".to_owned()],
            worktree_path: Some(parent.root.parent().unwrap().join(format!("{label}-child"))
                .to_string_lossy().into_owned()),
        }
    } else {
        DelegationWritePolicy::ReadOnly
    };
    let created = parent.state.create_read_only_delegation(&parent.session_id, CreateDelegationRequest {
        prompt: "Observe this child's own workspace.".to_owned(),
        title: Some("Ordinary source observation child".to_owned()), cwd: None,
        agent: Some(Agent::Codex), model: None, mode: Some(DelegationMode::Reviewer),
        write_policy: Some(policy),
    }).unwrap();
    let child = created.delegation.child_session_id;
    match receive(&parent._runtime_rx.lock().unwrap(), "actual delegation provider prompt") {
        CodexRuntimeCommand::Prompt { session_id, command } => {
            assert_eq!(session_id, child);
            assert!(command.prompt.contains("Observe this child's own workspace."));
        }
        _ => panic!("the production delegation must hand its prompt to the provider"),
    }
    let root = {
        let inner = parent.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
        assert!(record.runtime.runtime_token().is_some());
        assert!(record.engram.unadmitted_active_turn.is_some());
        PathBuf::from(&record.session.workdir)
    };
    if isolated {
        assert_ne!(fs::canonicalize(&root).unwrap(), fs::canonicalize(&parent.root).unwrap());
    } else {
        assert_eq!(fs::canonicalize(&root).unwrap(), fs::canonicalize(&parent.root).unwrap());
    }
    let binding = parent.record(|record| record.engram.work_binding.clone().unwrap());
    parent.transport.enable_named_roots(&child, &binding);
    parent.transport.named_roots.lock().unwrap().as_mut().unwrap().work_session_read_reply = Some(Ok(json!({
        "project_id": "github.com/example/source-root", "session_id": child,
        "work_session": true, "capabilities": ["host_work_v1"],
    })));
    let mut turn = EOCheckedTurn::from_session(parent.state.clone(), child, root,
        parent.transport.clone(), parent._runtime_rx.clone());
    turn.host = parent.host.clone();
    // The association is explicit scripted fixture input for closing facts.
    // It neither relabels the actual opening nor certifies external work.
    let opening = turn.record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    turn.record_mut(|record| record.engram.work_binding = Some(binding));
    assert_eq!(turn.record(|record| record.engram.unadmitted_active_turn.clone().unwrap()), opening);
    turn
}

#[test]
fn actual_delegation_children_report_their_measured_workspace_without_native_credit() {
    for (label, isolated, edit) in [("ordinary-isolated-edited", true, true),
        ("ordinary-readonly-quiet", false, false), ("ordinary-isolated-quiet", true, false)] {
        let turn = ordinary_delegation_child(label, isolated);
        let historical = install_passive_measured_baseline(&turn, &turn.root);
        let before = content_revision_of(&turn.root);
        assert_eq!(historical.endpoint.source_revision, before);
        if edit {
            fs::create_dir_all(turn.root.join("src")).unwrap();
            fs::write(turn.root.join("src/lib.rs"), "// actual isolated child edit\n").unwrap();
            turn.record_mut(|record| { record.active_turn_file_changes.insert(
                "src/lib.rs".into(), WorkspaceFileChangeKind::Modified); });
        }
        let after = content_revision_of(&turn.root);
        let request = turn.finish();
        assert_ordinary_observation_has_no_native_credit(&turn, &request);
        assert_eq!(request["occurrence"]["outcome"], "succeeded");
        assert_eq!(request["causality"]["kind"], "unknown");
        let change = &request["occurrence"]["source_change"];
        if edit {
            assert_ne!(before, after);
            assert_eq!(change["detection"], "content_comparison");
            assert_eq!(change["baseline"], serde_json::to_value(&historical.endpoint).unwrap());
            assert_eq!(change["sighting"]["source_basis"]["source_revision"], after);
            assert_eq!(PathBuf::from(change["workspace_id"].as_str().unwrap()),
                fs::canonicalize(&turn.root).unwrap());
            chrono::DateTime::parse_from_rfc3339(change["sighting"]["observed_at"].as_str().unwrap()).unwrap();
        } else {
            assert_eq!(before, after, "the quiet child turn wrote no source artifact between measurements");
            assert!(change.is_null(), "equal measured contents cannot invent a mutation: {request:#}");
        }
        wait_for_observation_delivery(&turn);
        assert_eq!(content_revision_of(&turn.root), after,
            "closing the actual child must not write host artifacts into its source root");
        assert_eq!(turn.record(|record| record.engram.runtime_checkpoint.clone()), Some(historical));
    }
}

#[test]
fn public_delete_keeps_an_actual_changed_child_until_its_ordinary_report_is_settled() {
    let turn = ordinary_delegation_child("ordinary-delete-changed-child", true);
    let baseline = install_passive_measured_baseline(&turn, &turn.root);
    let opening = turn.record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    fs::create_dir_all(turn.root.join("src")).unwrap();
    fs::write(turn.root.join("src/lib.rs"), "// edit before public Delete\n").unwrap();
    let changed_revision = content_revision_of(&turn.root);
    assert_ne!(changed_revision, baseline.endpoint.source_revision);

    let before_delete = turn.record(|record| (
        record.engram.unadmitted_active_turn.clone(), record.session.status,
        record.active_turn_generation, record.runtime.runtime_token(),
    ));
    let error = turn.state.kill_session(&turn.session_id).err()
        .expect("public Delete must retain an active ordinary observation owner");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("Session kept"));
    turn.record(|record| {
        // Ordinary-generation terminal close is newly reachable here compared
        // with the adopted-Claude master fixture. Diagnose transfer versus loss
        // without assuming a callback ran or weakening the active-owner check.
        assert_eq!(record.engram.unadmitted_active_turn.as_ref(), Some(&opening),
            "Delete custody snapshot: {}", json!({
                "openingTurn": opening.host_turn_ref,
                "openingGeneration": opening.generation,
                "beforeDelete": format!("{before_delete:?}"),
                "deleteError": error.message,
                "status": record.session.status,
                "runtimeToken": format!("{:?}", record.runtime.runtime_token()),
                "activeTurnGeneration": record.active_turn_generation,
                "activeTraceGeneration": record.engram.unadmitted_active_turn.as_ref().map(|trace| trace.generation),
                "adoptedClaudeGeneration": record.adopted_claude_turn_generation,
                "runtimeStopInProgress": record.runtime_stop_in_progress,
                "deleteRequested": record.engram.observation_delete_requested,
                "closingJobs": record.engram.unadmitted_closing_jobs.iter()
                    .filter(|job| job.trace.host_turn_ref == opening.host_turn_ref)
                    .map(|job| json!({
                        "turn": job.trace.host_turn_ref, "generation": job.trace.generation,
                        "outcome": job.trace.observed_outcome,
                        "scope": job.scope.label(), "storeProject": job.scope.store.project_id,
                        "terminalFailure": job.terminal_failure.is_some(),
                    })).collect::<Vec<_>>(),
                "reports": record.engram.unadmitted_reports.iter()
                    .filter(|report| report.host_turn_ref == opening.host_turn_ref)
                    .map(|report| json!({
                        "turn": report.host_turn_ref, "generation": report.generation,
                        "version": report.version, "observingSession": report.observing_session,
                        "storeProject": report.store.project_id,
                        "envelopes": report.envelopes.iter().map(|envelope| json!({
                            "kind": envelope.request["occurrence"]["kind"],
                            "outcome": envelope.request["occurrence"]["outcome"],
                            "binding": envelope.request["binding"],
                            "sourceScope": envelope.request["occurrence"]["source_change"],
                            "delivery": match &envelope.delivery {
                                EngramRuntimeObservationDelivery::Pending { .. } => "pending",
                                EngramRuntimeObservationDelivery::Recorded { .. } => "recorded",
                                EngramRuntimeObservationDelivery::Refused { .. } => "refused",
                                EngramRuntimeObservationDelivery::LocalFailure { .. } => "local_failure",
                            },
                        })).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                "nonReportNotices": record.session.messages.iter().filter_map(|message| {
                    let Message::Text { id, text, expanded_text: Some(detail), .. } = message else { return None; };
                    if !text.contains("not reported") || !detail.contains(&opening.host_turn_ref) { return None; }
                    let trace: Value = serde_json::from_str(detail).unwrap_or(Value::Null);
                    Some(json!({
                        "messageId": id, "turn": trace["host_turn_ref"],
                        "generation": trace["generation"], "outcome": trace["observed_outcome"],
                        "scope": "unavailable before non-report disposition",
                        "disposition": if text.contains("host restarted") {
                            "interrupted before closing capture"
                        } else { "closing scope unavailable; trace retained in transcript" },
                    }))
                }).collect::<Vec<_>>(),
            }));
        assert!(record.engram.observation_delete_requested);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert_eq!(record.session.status, SessionStatus::Active);
    });
    // Inspect the canonical row before boot recovery gives an interrupted
    // active turn its separate, uncredited recovery disposition.
    let durable: PersistedSessionRecord = {
        let db = open_sqlite_state_read_connection(&turn.state.persistence_path).unwrap();
        let raw: String = db.query_row("SELECT value_json FROM sessions WHERE id = ?1",
            [&turn.session_id], |row| row.get(0)).unwrap();
        serde_json::from_str(&raw).unwrap()
    };
    assert!(durable.engram_observation_delete_requested);
    assert!(durable.orchestrator_auto_dispatch_blocked);
    assert_eq!(durable.engram_unadmitted_active_turn.as_ref(), Some(&opening));
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let saved = &loaded.sessions[loaded.find_visible_session_index(&turn.session_id).unwrap()];
    assert!(saved.engram.observation_delete_requested);
    assert!(saved.engram.unadmitted_active_turn.is_none());
    let (notice, recovered) = saved.session.messages.iter().find_map(|message| {
        let Message::Text { text, expanded_text: Some(detail), .. } = message else { return None; };
        let trace: EngramRuntimeObservedTurn = serde_json::from_str(detail).ok()?;
        (trace.host_turn_ref == opening.host_turn_ref).then_some((text, trace))
    }).expect("boot recovery retains the original trace in its non-report notice");
    assert_eq!(recovered, opening);
    assert!(notice.contains("not reported") && notice.contains("host restarted")
        && notice.contains("uncredited"));
    assert!(saved.engram.unadmitted_closing_jobs.iter()
        .all(|job| job.trace.host_turn_ref != opening.host_turn_ref));
    assert!(saved.engram.unadmitted_reports.iter()
        .all(|report| report.host_turn_ref != opening.host_turn_ref));

    // Delete no longer manufactures an exit checkpoint or an unknown outcome.
    // Stop is the actual terminal owner and reports its cancelled outcome.
    turn.state.stop_session(&turn.session_id).unwrap();
    let request = assert_observed_result(&turn, &opening.host_turn_ref, "cancelled");
    let change = &request["occurrence"]["source_change"];
    assert_eq!(change["detection"], "content_comparison");
    assert_eq!(change["baseline"], serde_json::to_value(&baseline.endpoint).unwrap());
    assert_eq!(change["sighting"]["source_basis"]["source_revision"], changed_revision);
    wait_for_observation_delivery(&turn);
    turn.state.kill_session(&turn.session_id).expect("settled ordinary reporting permits public Delete");
    assert!(turn.state.inner.lock().unwrap().find_session_index(&turn.session_id).is_none());
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    let exists: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
        [&turn.session_id], |row| row.get(0)).unwrap();
    assert!(!exists, "successful Delete removes the canonical session row");
}

#[test]
fn a_worktree_root_over_engrams_basis_bound_leaves_the_report_without_a_basis() {
    let workdir = FsPath::new("C:/w");
    let at_bound = format!("C:/{}", "w".repeat(ENGRAM_SOURCE_BASIS_MAX_BYTES - 3));
    assert_eq!(engram_bounded_source_basis(workdir, FsPath::new(&at_bound), "revision".into()),
        Some(EngramExecutionSourceBasis { source_root_generation: None, source_root_state: None,
            workspace_id: at_bound.clone(), source_revision: "revision".into() }));
    assert_eq!(engram_bounded_source_basis(workdir, FsPath::new(&format!("{at_bound}w")), "revision".into()), None);
}

#[test]
fn ordinary_source_comparison_uses_actual_contents_and_retains_its_exact_endpoint() {
    for (edit, watcher) in [(false, false), (false, true), (true, false), (true, true)] {
        let turn = begin_with_passive_measured_baseline("ordinary-source-endpoint");
        let before = content_revision_of(&turn.root);
        if edit { fs::write(turn.root.join("README.md"), "changed during the ordinary turn\n").unwrap(); }
        if watcher {
            turn.record_mut(|record| {
                record.active_turn_file_changes.insert("README.md".into(), WorkspaceFileChangeKind::Modified);
            });
        }
        let after = content_revision_of(&turn.root);
        let request = turn.finish();
        assert_ordinary_observation_has_no_native_credit(&turn, &request);
        assert_eq!(request["occurrence"]["outcome"], "succeeded");
        let change = &request["occurrence"]["source_change"];
        if edit {
            assert_ne!(before, after);
            assert_eq!(change["detection"], "content_comparison");
            assert_eq!(change["baseline"]["source_revision"], before);
            assert_eq!(change["sighting"]["source_basis"]["source_revision"], after);
            assert_eq!(PathBuf::from(change["workspace_id"].as_str().unwrap()), fs::canonicalize(&turn.root).unwrap());
            chrono::DateTime::parse_from_rfc3339(change["sighting"]["observed_at"].as_str().unwrap()).unwrap();
            fs::write(turn.root.join("later.txt"), "after the observed endpoint\n").unwrap();
            assert_ne!(after, content_revision_of(&turn.root));
            assert_eq!(change["sighting"]["source_basis"]["source_revision"], after);
        } else {
            assert_eq!(before, after);
            assert!(change.is_null(), "a watcher hint cannot invent changed content: {request:#}");
        }
    }
}

#[test]
fn ordinary_terminal_callbacks_report_their_actual_outcome_once() {
    for (terminal, expected) in [("success", "succeeded"), ("failure", "failed"),
        ("stop", "cancelled"), ("exit-error", "failed"), ("exit-silent", "unknown"),
        ("marked-error", "failed"), ("atomic-failure", "failed")] {
        let turn = EOCheckedTurn::start(terminal, true);
        let turn_ref = observed_turn_ref(&turn);
        let (token, generation) = turn.record(|record|
            (record.runtime.runtime_token().unwrap(), record.active_turn_generation));
        fs::write(turn.root.join("README.md"), "changed before the terminal callback\n").unwrap();
        match terminal {
            "success" => { turn.state.finish_turn_ok_if_runtime_matches(&turn.session_id, &token).unwrap(); }
            "failure" => { turn.state.fail_turn_if_runtime_matches(&turn.session_id, &token, "provider failure").unwrap(); }
            "stop" => { turn.state.stop_session(&turn.session_id).unwrap(); }
            "exit-error" => { turn.state.handle_runtime_exit_if_matches(&turn.session_id, &token, Some("provider exit failed")).unwrap(); }
            "exit-silent" => { turn.state.handle_runtime_exit_if_matches(&turn.session_id, &token, None).unwrap(); }
            "marked-error" => { turn.state.mark_turn_error_if_runtime_matches(&turn.session_id, &token, "provider error").unwrap(); }
            "atomic-failure" => { assert!(turn.state.fail_turn_if_runtime_matches_or_missing(
                &turn.session_id, &token, generation, "turn/start failed").unwrap()); }
            _ => unreachable!(),
        }
        let request = assert_observed_result(&turn, &turn_ref, expected);
        assert!(!request["occurrence"]["source_change"].is_null(), "terminal outcome cannot erase a measured source change");
        turn.state.handle_runtime_exit_if_matches(&turn.session_id, &token, Some("duplicate callback")).unwrap();
        assert_eq!(ordinary_turn_requests(&turn, &turn_ref), [request]);
    }
}

#[test]
fn an_unclaimed_ordinary_turn_closes_without_inventing_a_work_observation() {
    let turn = EOCheckedTurn::start("ordinary-unclaimed-close", false);
    turn.end();
    let guard = phase_sync::PollGuard::new();
    loop {
        if turn.record(|record| record.engram.unadmitted_active_turn.is_none()
            && record.engram.unadmitted_closing_jobs.is_empty() && !record.engram.unadmitted_worker_running) { break; }
        guard.wait("the canonical no-claim result closes the ordinary trace");
    }
    assert!(!turn.transport.requests().iter().any(|sent| sent.request["operation"] == "execution_observe"));
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[test]
fn a_failed_closing_measurement_keeps_the_job_without_a_synthetic_watcher_change() {
    let turn = EOCheckedTurn::start("ordinary-missing-closing-basis", true);
    let turn_ref = observed_turn_ref(&turn);
    turn.record_mut(|record| { record.active_turn_file_changes.insert(
        "README.md".into(), WorkspaceFileChangeKind::Modified); });
    turn.state.inner.lock().unwrap().test_engram_source_capture = Some(Arc::new(|_, _| None));
    let (installed, installed_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let inner = turn.state.inner.clone();
    let session = turn.session_id.clone();
    let expected_turn = turn_ref.clone();
    let held = std::sync::atomic::AtomicBool::new(false);
    *turn.state.runtime_observation_lifetime.install_hook.lock().unwrap() = Some(Arc::new(move |phase| {
        if phase != "closing_installed" { return; }
        let failed = {
            let inner = inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()].engram.unadmitted_closing_jobs.iter()
                .find(|job| job.trace.host_turn_ref == expected_turn && job.last_error.is_some()).cloned()
        };
        if let Some(failed) = failed
            && !held.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let _ = installed.send(failed);
            // Releasing or dropping the sender also releases failure teardown.
            let _ = release_rx.lock().unwrap().recv_timeout(phase_sync::DEADLOCK_GUARD);
        }
    }));
    turn.end();
    let failed = phase_sync::receive(&installed_rx, "the exact failed closing job is installed before retry");
    assert_eq!(failed.trace.host_turn_ref, turn_ref);
    assert_eq!(failed.last_error.as_deref(), Some("runtime closing source could not be measured"));
    assert!(chrono::DateTime::parse_from_rfc3339(&failed.retry_at).is_ok());
    turn.record(|record| {
        assert!(record.engram.unadmitted_closing_jobs.iter().any(|job| job == &failed));
        assert!(record.engram.unadmitted_active_turn.is_none());
        assert!(record.engram.unadmitted_reports.is_empty());
    });
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()];
    assert!(record.engram.unadmitted_closing_jobs.iter().any(|job| job == &failed),
        "SQLite retains the exact failed closing job at the controlled boundary");
    assert!(record.engram.unadmitted_reports.is_empty());
    assert!(ordinary_turn_requests(&turn, &turn_ref).is_empty(),
        "a watcher hint supplies neither a measured sighting nor a synthetic source_change");
    assert_ordinary_turn_has_no_native_requests(&turn);
    *turn.state.runtime_observation_lifetime.install_hook.lock().unwrap() = None;
    drop(release);
    turn.state.runtime_observation_lifetime.stop_and_join();
}

#[test]
fn ordinary_work_targets_capture_the_fixture_clock_chosen_before_enablement() {
    let scripted = EOCheckedTurn::idle("ordinary-target-scripted-clock", true);
    let target = AppState::engram_source_root_target_locked(
        &scripted.state.inner.lock().unwrap(), &scripted.session_id).unwrap().unwrap();
    assert!(matches!(target.budget_clock, EngramBudgetClock::Scripted(_)));
    let before = target.budget_clock.now();
    scripted.state.engram_budget_clock().advance(Duration::from_millis(7));
    assert_eq!(target.budget_clock.now(), before + Duration::from_millis(7));
    // Real on purpose: this control pins the production clock variant, with
    // no sleeps, backdating, latency threshold or transport deadline claim.
    let real = EOCheckedTurn::idle_real("ordinary-target-real-clock", true);
    let target = AppState::engram_source_root_target_locked(
        &real.state.inner.lock().unwrap(), &real.session_id).unwrap().unwrap();
    assert!(matches!(target.budget_clock, EngramBudgetClock::Real));
}

#[test]
fn a_named_ordinary_turn_measures_the_worktree_and_ignores_a_main_checkout_edit() {
    for edit_named in [false, true] {
        let turn = EOCheckedTurn::idle("ordinary-named-endpoint", true);
        let worktree = add_observed_worktree(&turn);
        let named = name_observed_worktree(&turn, &worktree);
        assert_eq!(named.generation, 1);
        assert_eq!(named.source_revision, Some(content_revision_of(&worktree)));
        install_passive_measured_baseline(&turn, &worktree);
        let prompt = turn.begin_prompt("Observe the named worktree.").prompt;
        assert!(prompt.contains("its named source root"), "{prompt}");
        let path = if edit_named { worktree.join("README.md") } else { turn.root.join("README.md") };
        fs::write(&path, "changed in one checkout\n").unwrap();
        turn.record_mut(|record| { record.active_turn_file_changes.insert(
            path.to_string_lossy().into_owned(), WorkspaceFileChangeKind::Modified); });
        let request = turn.finish();
        assert_ordinary_observation_has_no_native_credit(&turn, &request);
        let change = &request["occurrence"]["source_change"];
        if edit_named {
            assert_eq!(change["sighting"]["source_basis"]["workspace_id"], named.root.unwrap());
            assert_eq!(change["sighting"]["source_basis"]["source_revision"], content_revision_of(&worktree));
            assert_eq!(change["sighting"]["source_basis"]["source_root_generation"], 1);
        } else { assert!(change.is_null(), "{request:#}"); }
    }
}

#[test]
fn a_name_given_during_an_ordinary_turn_keeps_the_opening_cut_and_applies_next_turn() {
    let turn = EOCheckedTurn::idle("ordinary-name-between-cuts", true);
    let worktree = add_observed_worktree(&turn);
    turn.begin("Observe the main checkout first.");
    let original = turn.record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    name_observed_worktree(&turn, &worktree);
    turn.record(|record| {
        let active = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert_eq!(active.host_turn_ref, original.host_turn_ref);
        assert_eq!(serde_json::to_value(active).unwrap(), serde_json::to_value(&original).unwrap(),
            "naming cannot mutate the already observed opening");
        let line = record.engram.pending_source_root_line.as_deref().unwrap();
        assert!(line.contains("its named source root") && !line.contains("pushd"), "{line}");
    });
    // A changed canonical root can keep the first closing job pending; it
    // cannot upgrade its captured root. The next turn gets a fresh trace.
    turn.end();
    let prompt = turn.begin_prompt("Use the newly named worktree.").prompt;
    assert!(prompt.contains("its named source root"), "{prompt}");
    let next = observed_turn_ref(&turn);
    assert_ne!(next, original.host_turn_ref);
    fs::write(worktree.join("README.md"), "changed only in the next named turn\n").unwrap();
    let request = turn.finish();
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
    assert_eq!(request["occurrence"]["source_change"]["sighting"]["source_basis"]["source_root_generation"], 1);
}

#[test]
fn closing_an_ordinary_turn_cannot_reselect_a_root_cleared_after_its_opening() {
    for clear in [false, true] {
        let turn = EOCheckedTurn::idle("ordinary-clear-after-opening", true);
        let worktree = add_observed_worktree(&turn);
        let named = name_observed_worktree(&turn, &worktree);
        turn.begin("Observe the named root without changing its selection.");
        let original = turn.record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
        assert_eq!(original.opening_root.as_ref().unwrap().source_root.as_ref().unwrap().generation,
            named.generation);
        if clear {
            let work = turn.record(|record| record.engram.work_binding.as_ref().unwrap().work_id.clone());
            let cleared = turn.state.name_engram_source_root(&turn.session_id,
                EngramSourceRootRequest { work, path: None }).unwrap();
            assert!(cleared.root.is_none());
            assert_eq!(cleared.generation, 0);
        }
        let expected = turn.state.inner.lock().unwrap().engram_work_source_roots.clone();
        if clear { assert!(expected.is_empty()); }
        else {
            assert_eq!(expected.len(), 1);
            assert_eq!(expected[0].generation, named.generation);
            assert_eq!(Some(&expected[0].root), named.root.as_ref());
        }
        let sealed_revision = content_revision_of(&worktree);
        turn.record(|record| {
            let current = record.engram.unadmitted_active_turn.as_ref().unwrap();
            assert_eq!(current.host_turn_ref, original.host_turn_ref);
            let opening = current.opening_root.as_ref().unwrap();
            let root = opening.source_root.as_ref().unwrap();
            assert_eq!(root.root, original.opening_root.as_ref().unwrap().source_root.as_ref().unwrap().root);
            assert_eq!(root.generation, named.generation);
            if clear {
                assert_eq!(root.sealed_revision.as_deref(), Some(sealed_revision.as_str()),
                    "a documented clear seals the earlier root, without relabeling its generation");
            } else {
                assert_eq!(current, &original);
            }
        });
        turn.end();
        let guard = phase_sync::PollGuard::new();
        loop {
            let settled = turn.record(|record| record.engram.unadmitted_active_turn.is_none()
                && !record.engram.unadmitted_worker_running
                && record.engram.unadmitted_closing_jobs.iter().all(|job| job.last_error.is_some())
                && (!record.engram.unadmitted_reports.is_empty()
                    || !record.engram.unadmitted_closing_jobs.is_empty()));
            if settled { break; }
            guard.wait("the actual closing worker reports or retains its original changed-scope job");
        }
        assert_eq!(turn.state.inner.lock().unwrap().engram_work_source_roots, expected,
            "ordinary closing facts cannot republish or replace the current canonical selection");
        assert_ordinary_turn_has_no_native_requests(&turn);
    }
}

#[cfg(windows)]
fn dedicated_turn(label: &str) -> EOCheckedTurn {
    let mut turn = EOCheckedTurn::start(label, true);
    turn.state.agent_runtime_spawning_enabled = true;
    turn
}

#[cfg(windows)]
#[test]
fn dedicated_cleanup_failure_keeps_unknown_outcome_and_duplicate_callbacks_add_no_report() {
    let turn = dedicated_turn("ordinary-dedicated-clean-root");
    let turn_ref = observed_turn_ref(&turn);
    let owner = super::delegation_process_cleanup::ProductionWorkerTree::spawn(
        &turn.state, &turn.session_id, &turn.root, false);
    owner.tree.termination_failures.store(1, std::sync::atomic::Ordering::SeqCst);
    owner.exit_root();
    let request = assert_observed_result(&turn, &turn_ref, "unknown");
    let token = turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Error);
        assert!(record.orchestrator_auto_dispatch_blocked);
        record.runtime.runtime_token().unwrap()
    });
    turn.state.handle_runtime_exit_if_matches(&turn.session_id, &token, Some("cleanup-only retry notice")).unwrap();
    turn.state.stop_session(&turn.session_id).unwrap();
    assert!(owner.tree.is_confirmed());
    assert_eq!(ordinary_turn_requests(&turn, &turn_ref), [request]);
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[cfg(windows)]
#[test]
fn dedicated_provider_failure_is_not_rewritten_by_clean_root_exit_or_cleanup() {
    let turn = dedicated_turn("ordinary-dedicated-provider-failure");
    let turn_ref = observed_turn_ref(&turn);
    let owner = super::delegation_process_cleanup::ProductionWorkerTree::spawn(
        &turn.state, &turn.session_id, &turn.root, true);
    let token = turn.record(|record| record.runtime.runtime_token().unwrap());
    owner.tree.termination_failures.store(1, std::sync::atomic::Ordering::SeqCst);
    owner.fail_initialize();
    let request = assert_observed_result(&turn, &turn_ref, "failed");
    owner.exit_root();
    assert!(owner.tree.is_confirmed());
    owner.assert_peers_exited();
    assert!(turn.record(|record| record.runtime.runtime_token().is_none()));
    turn.state.handle_runtime_exit_if_matches(&turn.session_id, &token, Some("duplicate provider exit")).unwrap();
    assert_eq!(ordinary_turn_requests(&turn, &turn_ref), [request]);
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[cfg(windows)]
#[test]
fn dedicated_initialize_failure_is_observed_before_its_root_waiter_cleanup() {
    let turn = dedicated_turn("ordinary-dedicated-provider-first");
    let turn_ref = observed_turn_ref(&turn);
    let owner = super::delegation_process_cleanup::ProductionWorkerTree::spawn(
        &turn.state, &turn.session_id, &turn.root, true);
    owner.fail_initialize();
    owner.await_exit();
    assert!(owner.tree.is_confirmed());
    assert_observed_result(&turn, &turn_ref, "failed");
}

#[cfg(windows)]
#[test]
fn dedicated_cleanup_does_not_overwrite_an_already_completed_ordinary_outcome() {
    let turn = dedicated_turn("ordinary-dedicated-completed");
    let turn_ref = observed_turn_ref(&turn);
    let owner = super::delegation_process_cleanup::ProductionWorkerTree::spawn(
        &turn.state, &turn.session_id, &turn.root, false);
    let token = turn.record(|record| record.runtime.runtime_token().unwrap());
    turn.state.finish_turn_ok_if_runtime_matches(&turn.session_id, &token).unwrap();
    let request = assert_observed_result(&turn, &turn_ref, "succeeded");
    owner.tree.termination_failures.store(1, std::sync::atomic::Ordering::SeqCst);
    owner.exit_root();
    assert_eq!(ordinary_turn_requests(&turn, &turn_ref), [request]);
    turn.state.stop_session(&turn.session_id).unwrap();
    assert!(owner.tree.is_confirmed());
}

#[cfg(windows)]
#[test]
fn dedicated_acp_pending_call_drain_keeps_the_root_exit_outcome_unknown() {
    let turn = dedicated_turn("ordinary-dedicated-acp-drain");
    let turn_ref = observed_turn_ref(&turn);
    let (claimed_tx, claimed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    TEST_DEDICATED_EXIT_GATE.with(|slot| *slot.borrow_mut() = Some(TestStopFenceGate { claimed_tx, release_rx }));
    let owner = super::delegation_process_cleanup::ProductionWorkerTree::spawn(
        &turn.state, &turn.session_id, &turn.root, true);
    let pending = owner.pending.as_ref().unwrap().lock().unwrap();
    owner.release_root();
    phase_sync::receive(&claimed_rx, "actual root waiter paused before cleanup");
    owner.tree.terminate_attempt("Kimi", |_| Ok(()), false).unwrap();
    assert!(owner.tree.is_confirmed());
    drop(pending);
    owner.wait_for_writer();
    let token_survived = turn.record(|record| record.runtime.runtime_token().is_some());
    release_tx.send(()).unwrap();
    owner.await_exit();
    assert!(token_survived, "cleanup-drained writer cannot erase the root waiter's token");
    assert_observed_result(&turn, &turn_ref, "unknown");
}
