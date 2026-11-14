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
        Self::with_named_root(label, true)
    }

    fn with_named_root(label: &str, named: bool) -> Self {
        let turn = EOCheckedTurn::idle(label, true);
        let first_root = if named {
            naming_worktree(&turn.root)
        } else {
            turn.root.clone()
        };
        prepare_naming_worktree_container(&turn.root);
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
        if named {
            request_naming(&turn, label, Some(&first_root), vec![held])
                .expect("the focused claim's root is named");
        } else {
            naming_store(&turn);
            turn.transport.replace_held_claims([Ok(held)]);
        }
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

#[path = "engram_source_root_closing_selection.rs"]
mod closing;
