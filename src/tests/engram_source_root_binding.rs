//! Naming selects the next opening without changing the running turn's owner.
//! Exercises the public naming owner, real ordinary checks and persisted reload.
use super::*;

struct TwoClaims {
    turn: EOCheckedTurn,
    first_root: PathBuf,
    second_root: PathBuf,
    first: EngramControlWorkBinding,
    second: EngramControlWorkBinding,
}

impl TwoClaims {
    fn new(label: &str) -> Self {
        let turn = EOCheckedTurn::idle(label, true);
        let first_root = naming_worktree(&turn.root);
        let second_root = turn.root.join(".worktrees/second");
        run_git_test_command(
            &turn.root,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "second",
                second_root.to_str().unwrap(),
            ],
        );
        let mut held = naming_claim(label);
        let first = held.items[0].control_binding.clone().unwrap();
        let second = test_control_work_binding("other-named-claim", 1);
        held.items.push(EngramHeldClaim {
            work_id: second.work_id.clone(),
            short_ref: "w-other-named-claim".into(),
            claim_id: second.claim_id.clone(),
            claim_fence: second.claim_fence,
            focused: false,
            control_binding: Some(second.clone()),
        });
        request_naming(&turn, label, Some(&first_root), vec![held])
            .expect("the focused claim's root is named");
        turn.transport.register_named_root_run(&second);
        Self {
            turn,
            first_root,
            second_root,
            first,
            second,
        }
    }

    fn name_other(&self) {
        let named = self
            .turn
            .state
            .name_engram_source_root(
                &self.turn.session_id,
                EngramSourceRootRequest {
                    work: self.second.work_id.clone(),
                    path: Some(Some(self.second_root.to_string_lossy().into_owned())),
                },
            )
            .expect("another held claim can select its root for a later opening");
        assert_eq!(
            named.root.as_deref(),
            fs::canonicalize(&self.second_root).unwrap().to_str()
        );
    }

    fn check_first(&self, key: &str) {
        self.check_in(key, &self.first_root);
    }

    fn check_in(&self, key: &str, root: &FsPath) {
        let mut recorder = self.turn.recorder();
        recorder
            .command_started_in(key, "cargo test", Some("cargo test"), root.to_str())
            .unwrap();
        self.turn.wait_for_snapshots();
        recorder
            .command_completed_with_exit(
                key,
                "cargo test",
                "test result: ok. 1 passed",
                CommandStatus::Success,
                EngramCommandExit::Code(0),
            )
            .unwrap();
        self.turn.wait_for_snapshots();
    }

    fn assert_restore_uses_existing_binding_rule(&self) {
        let mut restored = load_state(self.turn.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let record =
            &restored.sessions[restored.find_session_index(&self.turn.session_id).unwrap()];
        assert!(
            record.engram.work_binding.is_none(),
            "a reload must not manufacture a persisted runtime work association"
        );
        assert!(
            record.engram.pending_naming_selection.is_none(),
            "pending selection has the same runtime-only durability as the binding"
        );
        for (binding, path) in [
            (&self.first, &self.first_root),
            (&self.second, &self.second_root),
        ] {
            assert!(
                restored
                    .engram_work_source_roots
                    .iter()
                    .any(|root| root.claim_id == binding.claim_id
                        && FsPath::new(&root.root) == fs::canonicalize(path).unwrap()),
                "each claim's own named root survives reload"
            );
        }
        // Restart drops the process-local association. Reconstruct focus from
        // the actual ordinary work-service reader, not the last named root.
        // This fixture installs that returned association in the loaded image;
        // it does not exercise provider restart or replay an interrupted turn.
        let target = AppState::engram_source_root_target_locked(
            &self.turn.state.inner.lock().unwrap(),
            &self.turn.session_id,
        )
        .unwrap()
        .unwrap();
        let focused = target
            .adapter
            .read_work_binding(
                &target.connection,
                EngramBindingPreference { current: None },
                DEADLOCK_GUARD,
            )
            .unwrap();
        assert_eq!(
            focused.as_ref(),
            Some(&self.first),
            "the ordinary service still focuses A"
        );
        restored
            .session_mut(&self.turn.session_id)
            .unwrap()
            .engram
            .work_binding = focused;
        let next = runtime_observation_opening_root(&restored, &self.turn.session_id).unwrap();
        assert_eq!(next.binding, self.first);
        assert_eq!(
            FsPath::new(&next.source_root.unwrap().root),
            fs::canonicalize(&self.first_root).unwrap()
        );
    }

    fn assert_next_turn(&self, binding: &EngramControlWorkBinding, root: &FsPath) {
        let (stamp, revision) = {
            let inner = self.turn.state.inner.lock().unwrap();
            (
                inner.sessions[inner.find_session_index(&self.turn.session_id).unwrap()]
                    .mutation_stamp,
                inner.revision,
            )
        };
        self.turn
            .begin("The next ordinary turn uses its selected claim.");
        self.turn.record(|record| {
            assert!(record.mutation_stamp > stamp);
            assert!(
                record.engram.pending_naming_selection.is_none(),
                "the opening's successful commit consumes the selection once"
            );
            let opening = record
                .engram
                .unadmitted_active_turn
                .as_ref()
                .unwrap()
                .opening_root
                .as_ref()
                .unwrap();
            assert_eq!(&opening.binding, binding);
            assert_eq!(
                FsPath::new(&opening.source_root.as_ref().unwrap().root),
                fs::canonicalize(root).unwrap()
            );
        });
        assert!(self.turn.state.inner.lock().unwrap().revision > revision);
        // Read the canonical row before boot recovery gives the interrupted
        // active trace its separate disposition and removes it from memory.
        let saved: PersistedSessionRecord = {
            let db = open_sqlite_state_read_connection(&self.turn.state.persistence_path).unwrap();
            let raw: String = db
                .query_row(
                    "SELECT value_json FROM sessions WHERE id = ?1",
                    [&self.turn.session_id],
                    |row| row.get(0),
                )
                .unwrap();
            serde_json::from_str(&raw).unwrap()
        };
        assert_eq!(
            &saved
                .engram_unadmitted_active_turn
                .as_ref()
                .unwrap()
                .opening_root
                .as_ref()
                .unwrap()
                .binding,
            binding,
            "the new opening and its exact association were committed"
        );
        self.check_in("next-turn-check", root);
        // This producer fixture accepts one historical claim per session.
        // Registering B's run alone does not move that observation scope.
        // Configure the expected report owner without moving canonical focus
        // or changing the host's binding, opening, or measured check.
        self.turn
            .transport
            .enable_named_roots(&self.turn.session_id, binding);
        let request = self.turn.finish();
        assert_eq!(request["binding"], json!(binding));
        assert_eq!(
            request["occurrence"]["observed_checks"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn naming_another_claim_mid_turn_keeps_later_checks_owned_and_reported_by_the_opening_claim() {
    let fixture = TwoClaims::new("mid-turn-other-claim");
    fixture
        .turn
        .begin("The focused claim owns this ordinary turn.");
    let opening = fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .unwrap()
            .opening_root
            .clone()
    });
    fixture.check_first("before-other-name");
    assert_eq!(
        fixture
            .turn
            .record(|record| record.engram.active_turn_checks.len()),
        1
    );

    fixture.name_other();
    fixture.check_first("after-other-name");
    fixture.turn.record(|record| {
        assert_eq!(
            record.engram.active_turn_checks.len(),
            2,
            "the same running turn must retain its later check after another claim is named"
        );
        assert_eq!(record.engram.work_binding.as_ref(), Some(&fixture.first));
        assert_eq!(
            record
                .engram
                .unadmitted_active_turn
                .as_ref()
                .unwrap()
                .opening_root,
            opening
        );
    });
    let request = fixture.turn.finish();
    assert_eq!(request["binding"], json!(fixture.first));
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(
        checks.len(),
        2,
        "both actual checks reach the original claim's report"
    );
    assert!(
        checks
            .iter()
            .all(|check| check["observed_result"] == "passed")
    );
    assert_ordinary_observation_has_no_native_credit(&fixture.turn, &request);
    fixture.assert_restore_uses_existing_binding_rule();
    fixture.assert_next_turn(&fixture.second, &fixture.second_root);
    wait_for_observation_delivery(&fixture.turn);
    fixture.assert_next_turn(&fixture.first, &fixture.first_root);
}

#[test]
fn naming_another_claim_between_turns_selects_its_next_opening() {
    let fixture = TwoClaims::new("idle-other-claim");
    fixture.name_other();
    fixture.assert_restore_uses_existing_binding_rule();
    fixture.turn.record(|record| {
        assert_eq!(
            record.engram.work_binding.as_ref(),
            Some(&fixture.second),
            "an explicit name between turns selects the requested held association"
        );
    });
    fixture.assert_next_turn(&fixture.second, &fixture.second_root);
}

#[test]
fn later_turns_without_another_name_or_focus_change_keep_the_original_claim() {
    let fixture = TwoClaims::new("no-naming-control");
    fixture.assert_next_turn(&fixture.first, &fixture.first_root);
    fixture.assert_next_turn(&fixture.first, &fixture.first_root);
}

#[test]
fn a_successful_name_of_the_opening_claim_supersedes_a_pending_other_claim() {
    let fixture = TwoClaims::new("superseded-pending-name");
    fixture.turn.begin("Keep this turn's original work.");
    fixture.name_other();
    fixture
        .turn
        .state
        .name_engram_source_root(
            &fixture.turn.session_id,
            EngramSourceRootRequest {
                work: fixture.first.work_id.clone(),
                path: Some(Some(fixture.first_root.to_string_lossy().into_owned())),
            },
        )
        .unwrap();
    fixture.check_first("after-kept-name");
    let request = fixture.turn.finish();
    assert_eq!(request["binding"], json!(fixture.first));
    fixture.assert_next_turn(&fixture.first, &fixture.first_root);
}

#[test]
fn an_adopted_runtime_opening_consumes_the_pending_name_before_capturing_its_root() {
    let fixture = TwoClaims::new("adopted-next-name");
    fixture.turn.begin("The ordinary turn still belongs to A.");
    fixture.name_other();
    fixture.check_first("ordinary-before-adoption");
    assert_eq!(fixture.turn.finish()["binding"], json!(fixture.first));
    let token = fixture
        .turn
        .record(|record| record.runtime.runtime_token().unwrap());
    let generation = fixture
        .turn
        .state
        .open_unmediated_claude_turn(
            &fixture.turn.session_id,
            &token,
            ClaudeUnownedCause::NoPrompt,
        )
        .unwrap()
        .expect("an idle runtime turn is adopted");
    fixture.turn.record(|record| {
        assert_eq!(record.active_turn_generation, generation);
        assert!(record.engram.pending_naming_selection.is_none());
        let opening = record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .unwrap()
            .opening_root
            .as_ref()
            .unwrap();
        assert_eq!(opening.binding, fixture.second);
        assert_eq!(
            FsPath::new(&opening.source_root.as_ref().unwrap().root),
            fs::canonicalize(&fixture.second_root).unwrap()
        );
    });
}

impl TwoClaims {
    fn stop_into_other_before_capture(&self) -> EngramRuntimeClosingJob {
        self.turn.begin("A owns the turn that will be stopped.");
        self.check_first("only-A-check");
        self.name_other();
        {
            let mut inner = self.turn.state.inner.lock().unwrap();
            let id = inner.next_message_id();
            let record = inner.session_mut(&self.turn.session_id).unwrap();
            // Model the worker paused before picking up the new closing job.
            // Stop itself performs both terminal and successor transitions.
            record.engram.unadmitted_worker_running = true;
            // The provider has detached; Stop still owns its terminal facts.
            record.runtime = SessionRuntime::None;
            queue_prompt_on_record(
                record,
                PendingPrompt {
                    attachments: Vec::new(),
                    id,
                    timestamp: stamp_now(),
                    text: "B is the first queued successor.".into(),
                    expanded_text: None,
                    source: None,
                    engram_interrupted: false,
                    is_engram_retained: false,
                },
                Vec::new(),
            );
            self.turn.state.commit_locked(&mut inner).unwrap();
        }
        self.turn
            .state
            .stop_session_with_options(
                &self.turn.session_id,
                StopSessionOptions {
                    dispatch_queued_prompts_on_success: true,
                    pause_automatic_resumes_on_success: false,
                    orchestrator_stop_instance_id: None,
                },
            )
            .unwrap();
        self.turn.record(|record| {
            assert_eq!(record.session.status, SessionStatus::Active);
            assert!(record.queued_prompts.is_empty());
            assert_eq!(record.engram.work_binding.as_ref(), Some(&self.second));
            assert_eq!(
                record
                    .engram
                    .unadmitted_active_turn
                    .as_ref()
                    .unwrap()
                    .opening_root
                    .as_ref()
                    .unwrap()
                    .binding,
                self.second
            );
            let job = record.engram.unadmitted_closing_jobs.last().unwrap();
            assert_eq!(job.scope.binding.as_ref(), Some(&self.first));
            assert!(job.naming_successor.is_some());
            job.clone()
        })
    }
}

#[test]
fn stop_queued_successor_keeps_the_old_report_and_the_first_new_opening_separate() {
    let fixture = TwoClaims::new("stop-named-successor");
    let job = fixture.stop_into_other_before_capture();
    fixture.check_in("only-B-check", &fixture.second_root);
    let successor_check_id = fixture.turn.record(|record| {
        let successor = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert_ne!(successor.host_turn_ref, job.trace.host_turn_ref);
        // Completed snapshots remain in the session pool. Their exact turn
        // references, rather than the pool's total size, determine ownership.
        for turn in [&job.trace.host_turn_ref, &successor.host_turn_ref] {
            assert_eq!(
                record
                    .engram
                    .active_turn_checks
                    .iter()
                    .filter(|check| &check.host_turn_ref == turn)
                    .count(),
                1
            );
        }
        assert_eq!(job.trace.commands.len(), 1);
        assert_eq!(successor.commands.len(), 1);
        assert_ne!(
            job.trace.commands[0].check.host_check_id,
            successor.commands[0].check.host_check_id
        );
        successor.commands[0].check.host_check_id.clone()
    });
    fixture
        .turn
        .record_mut(|record| record.engram.unadmitted_worker_running = false);
    fixture
        .turn
        .state
        .start_runtime_observation_worker(&fixture.turn.session_id);
    let request = fixture.turn.observation_for(&job.trace.host_turn_ref);
    assert_eq!(request["binding"], json!(fixture.first));
    assert_eq!(
        request["occurrence"]["observed_checks"][0]["host_check_id"],
        json!(job.trace.commands[0].check.host_check_id)
    );
    assert_eq!(
        request["occurrence"]["observed_checks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_ordinary_observation_has_no_native_credit(&fixture.turn, &request);
    wait_for_observation_delivery(&fixture.turn);
    fixture.turn.record(|record| {
        assert!(
            record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .all(|old| old.trace.host_turn_ref != job.trace.host_turn_ref)
        );
        assert_eq!(record.engram.work_binding.as_ref(), Some(&fixture.second));
    });
    fixture
        .turn
        .transport
        .enable_named_roots(&fixture.turn.session_id, &fixture.second);
    let successor = fixture.turn.finish();
    assert_eq!(successor["binding"], json!(fixture.second));
    assert_eq!(
        successor["occurrence"]["observed_checks"][0]["host_check_id"],
        json!(successor_check_id)
    );
    assert_eq!(
        successor["occurrence"]["observed_checks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_ordinary_observation_has_no_native_credit(&fixture.turn, &successor);
}

#[test]
fn closing_selection_witness_refuses_unrelated_scope_changes_and_cross_turn_checks() {
    let fixture = TwoClaims::new("closing-witness-fences");
    let job = fixture.stop_into_other_before_capture();
    fixture.check_in("B-check-cannot-enter-A-job", &fixture.second_root);
    let mut inner = fixture.turn.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
    assert!(
        job.require_current_for_capture(&inner, &fixture.turn.session_id)
            .is_ok()
    );
    let original = inner.sessions[index].clone();
    let successor_checks = original
        .engram
        .unadmitted_active_turn
        .as_ref()
        .unwrap()
        .commands
        .clone();
    for change in [
        "binding", "root", "workdir", "checks", "unbound", "identity", "reset", "delete",
    ] {
        inner.sessions[index] = original.clone();
        match change {
            "binding" => {
                inner.sessions[index].engram.work_binding =
                    Some(test_control_work_binding("unrelated-claim", 1))
            }
            "root" => inner.sessions[index].engram.named_root = Some(EngramNamedRootState::Unknown),
            "workdir" => inner.sessions[index].session.workdir.push_str("-changed"),
            "checks" => {
                inner.sessions[index].engram.unadmitted_closing_jobs[0]
                    .trace
                    .commands = successor_checks.clone()
            }
            "unbound" => {
                inner.sessions[index].engram.unadmitted_closing_jobs[0]
                    .scope
                    .binding = None
            }
            "identity" => inner.sessions[index].engram.unadmitted_closing_jobs[0]
                .trace
                .host_turn_ref
                .push_str("-other"),
            "reset" => {
                inner.sessions[index].runtime_stop_in_progress = true;
            }
            "delete" => inner.sessions[index].engram.observation_delete_requested = true,
            _ => unreachable!(),
        }
        let candidate = if matches!(change, "checks" | "unbound" | "identity") {
            inner.sessions[index].engram.unadmitted_closing_jobs[0].clone()
        } else {
            job.clone()
        };
        if change == "unbound" {
            assert!(!candidate.capture_binding_is_current(
                &inner,
                &fixture.turn.session_id,
                &fixture.first
            ));
        } else {
            assert!(
                candidate
                    .require_current_for_capture(&inner, &fixture.turn.session_id)
                    .is_err(),
                "{change}"
            );
        }
    }
    inner.sessions[index] = original.clone();
    let root_index = inner
        .engram_work_source_roots
        .iter()
        .position(|root| root.claim_id == fixture.first.claim_id)
        .unwrap();
    let root = inner.engram_work_source_roots[root_index].clone();
    inner.engram_work_source_roots[root_index].generation += 1;
    assert!(
        job.require_current_for_capture(&inner, &fixture.turn.session_id)
            .is_err()
    );
    inner.engram_work_source_roots[root_index] = root;
    let project = inner.sessions[index].session.project_id.clone().unwrap();
    let settings = inner
        .projects
        .iter_mut()
        .find(|row| row.id == project)
        .unwrap()
        .engram
        .as_mut()
        .unwrap();
    let binary = settings.binary_path.clone();
    settings.binary_path = Some(
        fixture
            .turn
            .root
            .join("other-owner")
            .to_string_lossy()
            .into_owned(),
    );
    assert!(
        job.require_current_for_capture(&inner, &fixture.turn.session_id)
            .is_err()
    );
    inner
        .projects
        .iter_mut()
        .find(|row| row.id == project)
        .unwrap()
        .engram
        .as_mut()
        .unwrap()
        .binary_path = binary;
    assert!(
        job.require_current_for_capture(&inner, &fixture.turn.session_id)
            .is_ok()
    );
    let restored: EngramRuntimeClosingJob =
        serde_json::from_value(serde_json::to_value(&job).unwrap()).unwrap();
    assert!(restored.naming_successor.is_none());
    assert!(
        restored
            .require_current_for_capture(&inner, &fixture.turn.session_id)
            .is_err()
    );
    // Leave the paused fixture quiet; no worker was launched by this test.
    inner.sessions[index].engram.unadmitted_closing_jobs.clear();
    inner.sessions[index].engram.unadmitted_worker_running = false;
}
