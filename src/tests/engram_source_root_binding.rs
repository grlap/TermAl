//! Naming a held claim's root does not select that claim for this session.
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
            .expect("another held claim can name its own root without selecting it");
        assert_eq!(
            named.root.as_deref(),
            fs::canonicalize(&self.second_root).unwrap().to_str()
        );
    }

    fn check_first(&self, key: &str) {
        let mut recorder = self.turn.recorder();
        recorder
            .command_started_in(
                key,
                "cargo test",
                Some("cargo test"),
                self.first_root.to_str(),
            )
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

    fn assert_reloaded_focus_is_first(&self) {
        let mut restored = load_state(self.turn.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let record =
            &restored.sessions[restored.find_session_index(&self.turn.session_id).unwrap()];
        assert!(
            record.engram.work_binding.is_none(),
            "a reload must not manufacture a persisted runtime work association"
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

    fn assert_next_turn_is_first(&self) {
        self.turn
            .begin("The next ordinary turn keeps the focused claim.");
        self.turn.record(|record| {
            let opening = record
                .engram
                .unadmitted_active_turn
                .as_ref()
                .unwrap()
                .opening_root
                .as_ref()
                .unwrap();
            assert_eq!(opening.binding, self.first);
            assert_eq!(
                FsPath::new(&opening.source_root.as_ref().unwrap().root),
                fs::canonicalize(&self.first_root).unwrap()
            );
        });
        self.check_first("next-turn-check");
        let request = self.turn.finish();
        assert_eq!(request["binding"], json!(self.first));
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
    fixture.assert_reloaded_focus_is_first();
    fixture.assert_next_turn_is_first();
}

#[test]
fn naming_another_claim_between_turns_preserves_focus_across_reload_and_the_next_opening() {
    let fixture = TwoClaims::new("idle-other-claim");
    fixture.name_other();
    fixture.assert_reloaded_focus_is_first();
    fixture.turn.record(|record| {
        assert_eq!(
            record.engram.work_binding.as_ref(),
            Some(&fixture.first),
            "naming B between turns must not select B instead of the focused claim A"
        );
    });
    fixture.assert_next_turn_is_first();
}
