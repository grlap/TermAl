//! Distinguish missing source measurements, stranded turns and Claude hazards.
//! Only the subprocess reply is scripted; naming, provider delivery, closing
//! capture, report persistence and observation settlement use their real owners.
use super::*;
use crate::tests::engram_source_command::source_envelope;
use crate::tests::engram_source_owner::{source_owner_output, source_owner_reply};
use crate::tests::run_git_test_command_output;

struct NamedTurns {
    turn: EOCheckedTurn,
    root: PathBuf,
    named: EngramSourceRootResponse,
    owner: Arc<Mutex<Option<EngramSourceId>>>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl NamedTurns {
    fn new(label: &str) -> Self {
        let turn = EOCheckedTurn::idle(label, true);
        let root = add_observed_worktree(&turn);
        let named = name_observed_worktree(&turn, &root);
        let owner = Arc::new(Mutex::new(Some(Self::head(&root))));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let replies = owner.clone();
        let observed_calls = calls.clone();
        turn.state
            .inner
            .lock()
            .unwrap()
            .test_observation_source_owner = Some(Arc::new(move |_, _, args, _, _| {
            if args[0] == "--version" {
                return Ok(source_owner_reply(args));
            }
            observed_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let source = replies
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| anyhow::anyhow!("controlled source measurement failure"))?;
            let mut reply = source_envelope(FsPath::new(&args[2]), "clean");
            reply["source"] = json!(format!("git-commit:{}", source.commit));
            reply["tree"] = json!(source.tree);
            reply["source_id"] = serde_json::to_value(source).unwrap();
            Ok(source_owner_output(0, serde_json::to_vec(&reply).unwrap()))
        }));
        Self {
            turn,
            root,
            named,
            owner,
            calls,
        }
    }

    fn head(root: &FsPath) -> EngramSourceId {
        let commit = run_git_test_command_output(root, &["rev-parse", "HEAD"]);
        let tree = run_git_test_command_output(root, &["show", "-s", "--format=%T", "HEAD"]);
        serde_json::from_value(json!({
            "commit": commit.trim(), "tree": tree.trim(), "changes": [], "submodules": [],
        }))
        .unwrap()
    }

    fn move_head(&self) -> EngramSourceId {
        fs::write(self.root.join("README.md"), "new committed body\n").unwrap();
        run_git_test_command(&self.root, &["add", "README.md"]);
        run_git_test_command(
            &self.root,
            &["commit", "--quiet", "-m", "advance observed input"],
        );
        let source = Self::head(&self.root);
        *self.owner.lock().unwrap() = Some(source.clone());
        source
    }

    fn close(&self) -> Vec<Value> {
        let observed = self.turn.finish();
        let turn_ref = observed["occurrence"]["host_turn_ref"].as_str().unwrap();
        wait_for_observation_delivery(&self.turn);
        self.turn.record(|record| {
            record
                .engram
                .unadmitted_reports
                .iter()
                .find(|report| report.host_turn_ref == turn_ref)
                .unwrap()
                .envelopes
                .iter()
                .map(|envelope| envelope.request.clone())
                .collect()
        })
    }

    fn assert_sighting(&self, requests: &[Value], source: &EngramSourceId) {
        let sightings: Vec<_> = requests
            .iter()
            .filter(|request| request["occurrence"]["kind"] == "source_sighting")
            .collect();
        assert_eq!(
            sightings.len(),
            1,
            "one independent measured sighting: {requests:#?}"
        );
        let basis = &sightings[0]["occurrence"]["source_basis"];
        assert_eq!(basis["source_id"], serde_json::to_value(source).unwrap());
        assert_eq!(
            basis["workspace_id"],
            self.named.root.as_ref().unwrap().as_str()
        );
        assert_eq!(basis["source_root_generation"], self.named.generation);
        assert_eq!(basis["source_root_state"], "named");
        assert!(
            self.turn
                .record(|record| matches!(&record.engram.named_root,
            Some(EngramNamedRootState::Bound { generation, .. })
                if *generation == self.named.generation as i64))
        );
        assert_ordinary_turn_has_no_native_requests(&self.turn);
    }

    fn assert_no_sighting(requests: &[Value]) {
        assert_eq!(
            requests.len(),
            1,
            "only the ordinary outcome remains: {requests:#?}"
        );
        assert_eq!(requests[0]["occurrence"]["kind"], "unadmitted_turn");
        assert_eq!(requests[0]["occurrence"]["outcome"], "succeeded");
        assert!(requests[0]["occurrence"]["source_change"].is_null());
    }
}

#[test]
fn host_delivered_turn_re_sights_a_moved_head_without_renaming() {
    let fixture = NamedTurns::new("named-turn-head-move");
    let first = NamedTurns::head(&fixture.root);
    fixture.turn.begin("Observe commit A.");
    fixture.assert_sighting(&fixture.close(), &first);
    let next = fixture.move_head();
    assert_ne!(first.commit, next.commit);
    assert_ne!(first.tree, next.tree);
    fixture
        .turn
        .begin("Observe commit B through the same name.");
    fixture.assert_sighting(&fixture.close(), &next);
    fixture
        .turn
        .begin("A quiet later turn still supplies a sighting.");
    let quiet = fixture.close();
    fixture.assert_sighting(&quiet, &next);
    assert!(quiet[0]["occurrence"]["source_change"].is_null());
}

#[test]
fn measurement_failure_omits_only_that_turn_sighting_and_the_next_turn_recovers() {
    let fixture = NamedTurns::new("named-turn-owner-failure");
    let first = NamedTurns::head(&fixture.root);
    fixture.turn.begin("Establish commit A.");
    fixture.assert_sighting(&fixture.close(), &first);
    let next = fixture.move_head();
    *fixture.owner.lock().unwrap() = None;
    let before = fixture.calls.load(std::sync::atomic::Ordering::SeqCst);
    fixture
        .turn
        .begin("The source owner fails while observing B.");
    NamedTurns::assert_no_sighting(&fixture.close());
    assert_eq!(
        fixture.calls.load(std::sync::atomic::Ordering::SeqCst),
        before + 1,
        "H1: measurement was attempted, but its failure produced no sighting"
    );
    *fixture.owner.lock().unwrap() = Some(next.clone());
    fixture
        .turn
        .begin("The owner is available for a later B turn.");
    fixture.assert_sighting(&fixture.close(), &next);
}

#[test]
fn stranded_predecessor_is_preserved_at_close_and_the_next_turn_re_sights_without_renaming() {
    let fixture = NamedTurns::new("named-turn-stranded-generation");
    let first = NamedTurns::head(&fixture.root);
    fixture.turn.begin("Establish commit A.");
    let predecessor = fixture
        .turn
        .record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    fixture.assert_sighting(&fixture.close(), &first);
    let next = fixture.move_head();
    // Reproduce custody left by a missed terminal path. Do not discard or
    // relabel that older trace to make the successor's observation pass.
    fixture
        .turn
        .record_mut(|record| record.engram.unadmitted_active_turn = Some(predecessor.clone()));
    let before = fixture.calls.load(std::sync::atomic::Ordering::SeqCst);
    fixture
        .turn
        .begin("A new host-delivered turn cannot replace old custody.");
    assert!(
        fixture
            .turn
            .record(|record| record.active_turn_generation > predecessor.generation)
    );
    fixture.turn.end();
    fixture.turn.record(|record| {
        assert!(record.engram.unadmitted_active_turn.is_none());
        assert!(record.engram.unadmitted_closing_jobs.is_empty());
        assert_eq!(
            record.engram.unadmitted_reports.len(),
            1,
            "no closing report is invented for an opening the stale trace hid"
        );
        let archived = record
            .session
            .messages
            .iter()
            .find_map(|message| match message {
                Message::Text {
                    text,
                    expanded_text: Some(details),
                    ..
                } if text.contains(&predecessor.host_turn_ref)
                    && text.contains("belongs to an earlier turn") =>
                {
                    Some(details)
                }
                _ => None,
            })
            .expect("the predecessor's exact facts remain in the transcript");
        assert_eq!(
            serde_json::from_str::<EngramRuntimeObservedTurn>(archived).unwrap(),
            predecessor
        );
    });
    assert_eq!(
        fixture.calls.load(std::sync::atomic::Ordering::SeqCst),
        before,
        "retiring stale custody does not fabricate a source measurement"
    );
    let loaded = load_state(fixture.turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let saved = &loaded.sessions[loaded.find_session_index(&fixture.turn.session_id).unwrap()];
    assert!(saved.engram.unadmitted_active_turn.is_none());
    assert!(saved.session.messages.iter().any(|message| matches!(message,
        Message::Text { expanded_text: Some(details), .. }
            if serde_json::from_str::<EngramRuntimeObservedTurn>(details).ok().as_ref() == Some(&predecessor))),
        "the exact archived predecessor survives a durable reload");
    fixture
        .turn
        .begin("Capture the same named root after stale custody was retired.");
    fixture.assert_sighting(&fixture.close(), &next);
}

#[test]
fn a_current_generation_trace_survives_a_repeated_start_and_closes_normally() {
    let fixture = NamedTurns::new("named-turn-current-generation");
    let source = NamedTurns::head(&fixture.root);
    fixture
        .turn
        .begin("Keep the current turn's observation owner.");
    let current = fixture
        .turn
        .record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    {
        let mut inner = fixture.turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
        start_ordinary_runtime_observation(&mut inner, index);
    }
    assert_eq!(
        fixture
            .turn
            .record(|record| record.engram.unadmitted_active_turn.clone()),
        Some(current.clone())
    );
    let requests = fixture.close();
    fixture.assert_sighting(&requests, &source);
    assert_eq!(
        requests[0]["occurrence"]["host_turn_ref"],
        current.host_turn_ref
    );
    assert!(!fixture.turn.record(|record| record.session.messages.iter().any(|message|
        matches!(message, Message::Text { text, .. } if text.contains("belongs to an earlier turn")))));
}

#[test]
fn codex_parent_claude_child_hazard_depends_on_write_policy_and_clears_for_a_later_turn() {
    for read_only in [true, false] {
        let fixture = NamedTurns::new(&format!("named-turn-claude-child-{read_only}"));
        let first = NamedTurns::head(&fixture.root);
        fixture
            .turn
            .begin("Establish commit A before the Claude child.");
        fixture.assert_sighting(&fixture.close(), &first);
        let next = fixture.move_head();
        fixture
            .turn
            .begin("Observe B while a Claude child has outstanding work.");
        let (delegation, child) =
            crate::tests::delegation_support::install_required_review_delegation(
                &fixture.turn.state,
                &fixture.turn.session_id,
            );
        let (runtime, _commands) = test_claude_runtime_handle("named-turn-claude-child");
        let token = RuntimeToken::Claude(runtime.runtime_id.clone());
        {
            let mut inner = fixture.turn.state.inner.lock().unwrap();
            let parent_index = inner.find_session_index(&fixture.turn.session_id).unwrap();
            let parent_project = inner.sessions[parent_index].session.project_id.clone();
            let record = inner.session_mut(&child).unwrap();
            record.session.agent = Agent::Claude;
            record.session.project_id = parent_project;
            record.session.workdir = fixture.root.to_string_lossy().into_owned();
            record.session.status = SessionStatus::Active;
            record.active_turn_generation = 1;
            record.runtime = SessionRuntime::Claude(runtime);
            let child_delegation = inner
                .delegations
                .iter_mut()
                .find(|item| item.id == delegation)
                .unwrap();
            child_delegation.agent = Agent::Claude;
            child_delegation.cwd = fixture.root.to_string_lossy().into_owned();
            if !read_only {
                child_delegation.write_policy = DelegationWritePolicy::SharedWorktree {
                    owned_paths: vec!["README.md".into()],
                };
            }
        }
        fixture
            .turn
            .state
            .note_engram_session_worktree_off_lock(&child);
        let provenance = ClaudeObservationProvenance {
            token: token.clone(),
            origin: ClaudeWorkOrigin::Attempt { turn_generation: 1 },
        };
        let started = claude_frame_work(
            &json!({"type":"assistant", "message":{"content":[
                {"type":"tool_use", "id":"child-work", "name":"Bash",
                    "input":{"command":"git status", "run_in_background":true}}
            ]}}),
            false,
        );
        fixture
            .turn
            .state
            .admit_claude_frame(&child, &provenance, false, &started);
        assert_eq!(
            fixture.turn.record(|record| record
                .engram
                .unadmitted_active_turn
                .as_ref()
                .unwrap()
                .source_attribution_uncertain),
            !read_only,
            "H3: read-only children cannot mix a Codex parent's named source"
        );
        let during = fixture.close();
        if read_only {
            fixture.assert_sighting(&during, &next);
        } else {
            NamedTurns::assert_no_sighting(&during);
        }
        let ended = claude_frame_work(
            &json!({"type":"system", "subtype":"task_notification",
            "tool_use_id":"child-work", "status":"completed"}),
            false,
        );
        fixture
            .turn
            .state
            .settle_claude_frame(&child, &token, &ended);
        {
            let inner = fixture.turn.state.inner.lock().unwrap();
            let child_index = inner.find_session_index(&child).unwrap();
            assert!(!inner.sessions[child_index].claude_outstanding.any());
        }
        fixture
            .turn
            .begin("The next turn follows the child's correlated completion.");
        assert!(!fixture.turn.record(|record| {
            record
                .engram
                .unadmitted_active_turn
                .as_ref()
                .unwrap()
                .source_attribution_uncertain
        }));
        fixture.assert_sighting(&fixture.close(), &next);
    }
}
