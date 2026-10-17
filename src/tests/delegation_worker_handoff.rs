//! Worker result metadata, ordinary Git locators and reported-test provenance.
//! Does not own worker admission, automatic integration or verification minting.
//! Reuses the slice-one Worker fixture; no live provider editing is claimed.

use super::delegation_support::install_delegation_codex_runtime;
use super::delegation_worker::WorkerFixture;
use super::*;

async fn start_worker() -> (WorkerFixture, String, String, String) {
    let fixture = WorkerFixture::new();
    let (status, body) = fixture.spawn("worker", fixture.isolated_policy(None)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let delegation = body["delegation"]["id"]
        .as_str()
        .expect("delegation")
        .to_owned();
    let child = body["childSession"]["id"]
        .as_str()
        .expect("child")
        .to_owned();
    let base = body["delegation"]["baseRevision"]
        .as_str()
        .expect("base")
        .to_owned();
    let command =
        recv_within_guard(&fixture.input_rx, "worker handoff prompt").expect("worker prompt");
    assert!(matches!(command, CodexRuntimeCommand::Prompt { .. }));
    (fixture, delegation, child, base)
}

fn push_child_message(fixture: &WorkerFixture, child: &str, message: Message) {
    let saved = {
        let mut inner = fixture.state.inner.lock().expect("state lock");
        match inner.find_session_index(child) {
            Some(index) => match inner.session_mut_by_index(index) {
                Some(record) => {
                    push_message_on_record(record, message);
                    fixture
                        .state
                        .commit_locked(&mut inner)
                        .map_err(|error| error.to_string())
                }
                None => Err("missing fixture child record".to_owned()),
            },
            None => Err("missing fixture child".to_owned()),
        }
    };
    saved.expect("child message persisted without an assertion under StateInner");
}

fn child_text(fixture: &WorkerFixture, child: &str, author: Author, text: &str) {
    let id = {
        let mut inner = fixture.state.inner.lock().expect("state lock");
        inner.next_message_id()
    };
    push_child_message(
        fixture,
        child,
        Message::Text {
            id,
            timestamp: stamp_now(),
            author,
            text: text.to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
            source: None,
        },
    );
}

fn child_command(fixture: &WorkerFixture, child: &str, command: &str) {
    let id = {
        let mut inner = fixture.state.inner.lock().expect("state lock");
        inner.next_message_id()
    };
    push_child_message(
        fixture,
        child,
        Message::Command {
            id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            command: command.to_owned(),
            command_language: Some("shell".to_owned()),
            output: "transcript status alone is not a native test result".to_owned(),
            output_language: Some("text".to_owned()),
            status: CommandStatus::Success,
        },
    );
}

fn finish_worker(fixture: &WorkerFixture, child: &str, status: &str, notes: &str) {
    let token = {
        let inner = fixture.state.inner.lock().expect("state lock");
        inner
            .find_session_index(child)
            .and_then(|index| inner.sessions[index].runtime.runtime_token())
    }
    .expect("attached fixture runtime");
    child_text(
        fixture,
        child,
        Author::Assistant,
        &format!(
            "## Result\nStatus: {status}\n\nSummary:\nWorker handoff.\n\nFindings:\n- None\n\nNotes:\n{notes}"
        ),
    );
    fixture
        .state
        .finish_turn_ok_if_runtime_matches(child, &token)
        .expect("production turn completion");
}

async fn get_json(
    fixture: &WorkerFixture,
    parent: &str,
    delegation: &str,
    suffix: &str,
) -> (StatusCode, Value) {
    request_json(
        &app_router(fixture.state.clone()),
        Request::builder()
            .uri(format!(
                "/api/sessions/{parent}/delegations/{delegation}/{suffix}"
            ))
            .body(Body::empty())
            .expect("GET request"),
    )
    .await
}

fn result_notes(value: &Value) -> String {
    value["result"]["notes"]
        .as_array()
        .map(|notes| {
            notes
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn assert_source_metadata(value: &Value, fixture: &WorkerFixture, base: &str) {
    assert_eq!(
        value["result"]["baseRevision"], base,
        "host-recorded base: {value}"
    );
    assert_eq!(
        value["result"]["worktreePath"],
        fixture.worktree.to_string_lossy().as_ref()
    );
}

#[tokio::test]
async fn delegation_worker_handoff_prompt_requests_git_locator_and_honest_test_provenance() {
    let fixture = WorkerFixture::new();
    let (status, body) = fixture.spawn("worker", fixture.isolated_policy(None)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let prompt = match recv_within_guard(&fixture.input_rx, "worker guidance").expect("prompt") {
        CodexRuntimeCommand::Prompt { command, .. } => command.prompt,
        _ => panic!("expected worker prompt"),
    };
    assert!(prompt.contains(body["delegation"]["baseRevision"].as_str().expect("base")));
    for required in [
        "Notes:",
        "git diff",
        "untracked",
        "child-reported",
        "host-recorded verification",
        "unavailable",
    ] {
        assert!(prompt.contains(required), "missing {required}: {prompt}");
    }
    let record = {
        let inner = fixture.state.inner.lock().expect("state lock");
        inner.delegations.first().cloned()
    }
    .expect("worker record");
    let mut reviewer = record.clone();
    reviewer.mode = DelegationMode::Reviewer;
    reviewer.write_policy = DelegationWritePolicy::ReadOnly;
    let reviewer_prompt = build_delegation_prompt(&reviewer);
    assert!(reviewer_prompt.contains("Write policy: read-only"));
    assert!(!reviewer_prompt.contains("Worker handoff"));
}

#[tokio::test]
async fn delegation_worker_handoff_result_exposes_host_basis_and_parent_scoped_paged_output() {
    let (fixture, delegation, child, base) = start_worker().await;
    let notes = format!(
        "- Git handoff: git -C \"{}\" diff {base}; uncommitted workspace locator, not snapshot.\n- Child-reported native test outcome: unavailable (not run).\n- workerOutputUnavailable: true (child prose, not host attribution).",
        fixture.worktree.display()
    );
    finish_worker(&fixture, &child, "completed", &notes);
    let (status, result) = get_json(&fixture, &fixture.parent, &delegation, "result").await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_source_metadata(&result, &fixture, &base);
    assert_eq!(result["result"]["delegationId"], delegation);
    assert_eq!(result["result"]["childSessionId"], child);
    assert_eq!(result["result"]["workerOutputUnavailable"], false);
    assert!(result_notes(&result).contains("Git handoff:"));
    let unrelated = test_session_id(&fixture.state, Agent::Codex);
    let (denied, _) = get_json(&fixture, &unrelated, &delegation, "result").await;
    assert_eq!(denied, StatusCode::NOT_FOUND);
    let mut offset = 0;
    let mut output = String::new();
    loop {
        let (status, page) = get_json(
            &fixture,
            &fixture.parent,
            &delegation,
            &format!("result/output?offsetBytes={offset}&limitBytes=256"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert_eq!(page["delegationId"], delegation);
        assert_eq!(page["childSessionId"], child);
        assert_eq!(page["summaryFallback"], false);
        output.push_str(page["output"].as_str().expect("output page"));
        if page["complete"] == true {
            break;
        }
        let next = page["nextOffsetBytes"].as_u64().expect("next byte offset");
        assert!(next > offset);
        offset = next;
    }
    assert!(output.contains(&notes));
}

#[tokio::test]
async fn delegation_worker_handoff_native_success_and_failure_remain_child_reported() {
    for expected in [0, 1] {
        let (fixture, delegation, child, _) = start_worker().await;
        if expected == 1 {
            fs::write(fixture.worktree.join("README.md"), "edited\n").expect("child edit");
        }
        // This is a real local native result; the provider report is scripted.
        let native = git_command()
            .arg("-C")
            .arg(&fixture.worktree)
            .args(["diff", "--exit-code", "--", "README.md"])
            .output()
            .expect("native Git");
        assert_eq!(native.status.code(), Some(expected));
        let artifact = fixture.worktree.join("native-test-output.txt");
        fs::write(&artifact, &native.stdout).expect("fixture-owned native output");
        let note = format!(
            "- Child-reported native test outcome: git diff --exit-code -- README.md; exit={expected}; artifact={}.\n- Git handoff: unavailable (test-only fixture).",
            artifact.display()
        );
        finish_worker(&fixture, &child, "completed", &note);
        let (_, result) = get_json(&fixture, &fixture.parent, &delegation, "result").await;
        let notes = result_notes(&result);
        assert!(notes.contains(&format!("exit={expected}")), "{result}");
        assert!(notes.contains("not host-recorded verification"), "{result}");
        assert!(result["result"].get("verification").is_none());
    }
}

#[tokio::test]
async fn delegation_worker_handoff_missing_report_does_not_promote_command_success() {
    let (fixture, delegation, child, _) = start_worker().await;
    child_command(&fixture, &child, "cargo test old-transcript-status");
    finish_worker(
        &fixture,
        &child,
        "completed",
        "- No handoff evidence supplied.",
    );
    let (_, result) = get_json(&fixture, &fixture.parent, &delegation, "result").await;
    let notes = result_notes(&result).to_ascii_lowercase();
    assert!(
        notes.contains("git handoff") && notes.contains("unavailable"),
        "{result}"
    );
    assert!(
        notes.contains("native test outcome") && notes.contains("unavailable"),
        "{result}"
    );
    assert!(notes.contains("not host-recorded verification"), "{result}");
    assert!(result["result"].get("verification").is_none());
}

#[tokio::test]
async fn delegation_worker_handoff_followup_excludes_previous_native_claim_and_commands() {
    let (fixture, delegation, child, _) = start_worker().await;
    child_command(&fixture, &child, "cargo test first-attempt-only");
    finish_worker(
        &fixture,
        &child,
        "completed",
        "- Git handoff: first-attempt-only.\n- Child-reported native test outcome: first-attempt-only exit=0.",
    );
    let (_, first) = get_json(&fixture, &fixture.parent, &delegation, "result").await;
    assert_eq!(first["result"]["status"], "completed");
    install_delegation_codex_runtime(&fixture.state, "worker-handoff-followup-runtime");
    let (status, response): (StatusCode, Value) = request_json(
        &app_router(fixture.state.clone()),
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{}/delegations/{delegation}/followup",
                fixture.parent
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"message":"Now a separate follow-up; no new test was run."}"#,
            ))
            .expect("follow-up request"),
    )
    .await;
    assert!(status.is_success(), "{status}: {response}");
    child_command(&fixture, &child, "git status current-attempt-only");
    finish_worker(
        &fixture,
        &child,
        "completed",
        "- No native test was run for this follow-up.",
    );
    let (_, current) = get_json(&fixture, &fixture.parent, &delegation, "result").await;
    let commands = current["result"]["commandsRun"]
        .as_array()
        .expect("current commands");
    assert!(
        commands.iter().all(|entry| !entry["command"]
            .as_str()
            .unwrap_or_default()
            .contains("first-attempt-only")),
        "{current}"
    );
    assert!(
        commands
            .iter()
            .any(|entry| entry["command"] == "git status current-attempt-only")
    );
    assert!(!result_notes(&current).contains("first-attempt-only"));
    assert!(
        result_notes(&current)
            .to_ascii_lowercase()
            .contains("unavailable")
    );
    let (_, page) = get_json(
        &fixture,
        &fixture.parent,
        &delegation,
        "result/output?limitBytes=8192",
    )
    .await;
    assert!(
        !page["output"]
            .as_str()
            .expect("current output")
            .contains("first-attempt-only")
    );
    let mut trimmed = {
        let inner = fixture.state.inner.lock().expect("state lock");
        inner
            .sessions
            .iter()
            .find(|record| record.session.id == child)
            .map(|record| record.session.clone())
    }
    .expect("child snapshot");
    trimmed
        .messages
        .retain(|message| message.user_prompt_text().is_none());
    assert!(
        worker_current_attempt_messages(&trimmed).is_none(),
        "a missing prompt must not fall back to the whole transcript"
    );
}

#[tokio::test]
async fn delegation_worker_handoff_failure_and_cancel_keep_basis_and_honest_availability() {
    for canceled in [false, true] {
        let (fixture, delegation, child, base) = start_worker().await;
        if canceled {
            fixture
                .state
                .cancel_delegation(&fixture.parent, &delegation)
                .expect("cancel");
        } else {
            finish_worker(
                &fixture,
                &child,
                "failed",
                "- Git handoff: unavailable (blocked).\n- Child-reported native test outcome: exit=7; failed fixture check.",
            );
        }
        let (_, result) = get_json(&fixture, &fixture.parent, &delegation, "result").await;
        assert_source_metadata(&result, &fixture, &base);
        assert_eq!(
            result["result"]["status"],
            if canceled { "canceled" } else { "failed" }
        );
        let notes = result_notes(&result);
        assert!(
            notes.to_ascii_lowercase().contains("unavailable"),
            "{result}"
        );
        if !canceled {
            assert!(
                notes.contains("exit=7"),
                "failed native report lost: {result}"
            );
        }
        assert!(fixture.worktree.join(".git").is_file());
    }
}

async fn assert_unstarted_followup_handoff(prior_status: &str, canceled: bool) {
    let (fixture, delegation, child, base) = start_worker().await;
    child_command(&fixture, &child, "cargo test previous-attempt-only");
    let id = {
        let mut inner = fixture.state.inner.lock().expect("state lock");
        inner.next_message_id()
    };
    push_child_message(
        &fixture,
        &child,
        Message::FileChanges {
            id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            title: "Previous attempt observations".to_owned(),
            files: vec![FileChangeSummaryEntry {
                path: "previous-attempt-only.rs".to_owned(),
                kind: WorkspaceFileChangeKind::Modified,
            }],
        },
    );
    finish_worker(
        &fixture,
        &child,
        prior_status,
        "- Git handoff: previous-attempt-only.\n- Child-reported native test outcome: previous-attempt-only exit=7.\n- workerOutputUnavailable: false (child prose).",
    );
    let (_, previous) = get_json(&fixture, &fixture.parent, &delegation, "result").await;
    assert_eq!(previous["result"]["status"], prior_status);
    assert_eq!(previous["result"]["workerOutputUnavailable"], false);
    assert!(result_notes(&previous).contains("previous-attempt-only"));
    assert_eq!(
        previous["result"]["commandsRun"][0]["command"],
        "cargo test previous-attempt-only"
    );
    assert!(
        previous["result"]["observedWorkspaceChanges"]
            .as_array()
            .expect("previous observations")
            .iter()
            .any(|path| path == "previous-attempt-only.rs")
    );

    install_delegation_codex_runtime(&fixture.state, "unstarted-worker-followup");
    {
        let mut inner = fixture.state.inner.lock().expect("state lock");
        if let Some(index) = inner.find_session_index(&child) {
            inner.sessions[index].engram.project_reset_in_progress = true;
        }
        if let Some(index) = inner.find_session_index(&fixture.parent) {
            inner.sessions[index].session.status = SessionStatus::Active;
        }
    }
    let admitted = fixture
        .state
        .followup_delegation(
            &fixture.parent,
            &delegation,
            "A new queued attempt".to_owned(),
        )
        .expect("accepted queued follow-up");
    assert_eq!(admitted.delegation.status, DelegationStatus::Running);
    let prompt_id = admitted
        .delegation
        .queued_followup_prompt_id
        .expect("queued identity");
    let boundary_absent = {
        let inner = fixture.state.inner.lock().expect("state lock");
        inner.find_session_index(&child).is_some_and(|index| {
            let child = &inner.sessions[index];
            child
                .queued_prompts
                .iter()
                .any(|queued| queued.pending_prompt.id == prompt_id)
                && !child
                    .session
                    .messages
                    .iter()
                    .any(|message| message.id() == prompt_id)
        })
    };
    assert!(
        boundary_absent,
        "admitted prompt must not be in the transcript yet"
    );
    fixture
        .state
        .create_delegation_wait(
            &fixture.parent,
            CreateDelegationWaitRequest {
                delegation_ids: vec![delegation.clone()],
                mode: DelegationWaitMode::All,
                title: None,
            },
        )
        .expect("parent wait for this attempt");
    if canceled {
        fixture
            .state
            .cancel_delegation(&fixture.parent, &delegation)
            .expect("cancel queued attempt");
    } else {
        // Force the same delayed-start failure seam as the existing admission
        // regression: test_app_state forbids spawning a missing runtime.
        install_single_wire_codex_fixture(&fixture.state, None);
        {
            let mut inner = fixture.state.inner.lock().expect("state lock");
            if let Some(index) = inner.find_session_index(&child) {
                inner.sessions[index].engram.project_reset_in_progress = false;
                inner.sessions[index].runtime = SessionRuntime::None;
            }
        }
        let error = fixture
            .state
            .start_next_queued_turn_off_lock(&child, true, false)
            .err()
            .expect("delayed runtime startup must fail before promotion");
        assert!(format!("{error:#}").contains("spawn"), "{error:#}");
    }
    let (status, current) = get_json(&fixture, &fixture.parent, &delegation, "result").await;
    assert_eq!(status, StatusCode::OK, "{current}");
    assert_source_metadata(&current, &fixture, &base);
    assert_eq!(
        current["result"]["status"],
        if canceled { "canceled" } else { "failed" }
    );
    assert_eq!(current["result"]["workerOutputUnavailable"], true);
    for field in ["commandsRun", "observedWorkspaceChanges", "changedFiles"] {
        assert!(
            current["result"]
                .get(field)
                .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty)),
            "{field}: {current}"
        );
    }
    let notes = current["result"]["notes"].as_array().expect("host notes");
    assert!(notes.iter().any(|note| {
        note.as_str()
            .is_some_and(|note| note.starts_with("Git handoff: unavailable"))
    }));
    assert!(
        notes.iter().any(|note| note.as_str().is_some_and(
            |note| note.starts_with("Child-reported native test outcome: unavailable")
        ))
    );
    assert!(result_notes(&current).contains("attempt did not start"));
    assert!(!result_notes(&current).contains("previous-attempt-only"));
    let (_, page) = get_json(
        &fixture,
        &fixture.parent,
        &delegation,
        "result/output?limitBytes=8192",
    )
    .await;
    assert_eq!(page["summaryFallback"], true, "{page}");
    assert_eq!(page["output"], current["result"]["summary"]);
    assert!(
        !page["output"]
            .as_str()
            .expect("fallback")
            .contains("previous-attempt-only")
    );
    let fan_in = {
        let inner = fixture.state.inner.lock().expect("state lock");
        inner
            .find_session_index(&fixture.parent)
            .map(|index| {
                inner.sessions[index]
                    .queued_prompts
                    .iter()
                    .map(|queued| queued.pending_prompt.text.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    assert!(!fan_in.is_empty(), "terminal wait must queue parent fan-in");
    assert!(
        fan_in
            .iter()
            .all(|text| !text.contains("previous-attempt-only"))
    );
    assert!(
        fan_in
            .iter()
            .any(|text| text.contains("attempt did not start"))
    );
    let loaded = load_state(&fixture.state.persistence_path)
        .expect("SQLite reload")
        .expect("state");
    let saved = loaded
        .delegations
        .iter()
        .find(|record| record.id == delegation)
        .and_then(|record| record.result.as_ref())
        .expect("saved terminal result");
    assert_eq!(saved.worker_output_unavailable, Some(true));
    assert!(
        !serde_json::to_string(saved)
            .expect("saved JSON")
            .contains("previous-attempt-only")
    );
}

#[tokio::test]
async fn delegation_worker_handoff_completed_then_queued_start_failure_has_no_old_evidence() {
    assert_unstarted_followup_handoff("completed", false).await;
}

#[tokio::test]
async fn delegation_worker_handoff_failed_then_queued_start_failure_has_no_old_evidence() {
    assert_unstarted_followup_handoff("failed", false).await;
}

#[tokio::test]
async fn delegation_worker_handoff_completed_then_queued_cancel_has_no_old_evidence() {
    assert_unstarted_followup_handoff("completed", true).await;
}

#[tokio::test]
async fn delegation_worker_handoff_failed_then_queued_cancel_has_no_old_evidence() {
    assert_unstarted_followup_handoff("failed", true).await;
}

#[tokio::test]
async fn delegation_worker_handoff_new_file_diff_is_explicit_and_parent_git_is_untouched() {
    let (fixture, delegation, child, base) = start_worker().await;
    fs::write(fixture.source.join("parent-only.txt"), "keep parent dirt\n").expect("parent dirt");
    let parent_head = run_git_test_command_output(&fixture.source, &["rev-parse", "HEAD"]);
    let parent_tree = run_git_test_command_output(&fixture.source, &["write-tree"]);
    let parent_status = run_git_test_command_output(
        &fixture.source,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    fs::write(
        fixture.worktree.join("new-worker-file.txt"),
        "worker change\n",
    )
    .expect("new worker file");
    let plain = run_git_test_command_output(&fixture.worktree, &["diff", &base]);
    assert!(
        !plain.contains("new-worker-file.txt"),
        "ordinary diff omits untracked files"
    );
    // Explicit child-side preparation, never a side effect of completing/GET.
    run_git_test_command(
        &fixture.worktree,
        &["add", "-N", "--", "new-worker-file.txt"],
    );
    let diff = run_git_test_command_output(&fixture.worktree, &["diff", "--binary", &base]);
    assert!(diff.contains("new-worker-file.txt") && diff.contains("+worker change"));
    let child_index = fs::read(run_git_test_command_output(
        &fixture.worktree,
        &["rev-parse", "--git-path", "index"],
    ))
    .expect("child index");
    finish_worker(
        &fixture,
        &child,
        "completed",
        &format!(
            "- Git handoff: git diff --binary {base}; new-worker-file.txt explicitly included with child-side intent-to-add, mutable workspace locator.\n- Child-reported native test outcome: unavailable."
        ),
    );
    let (_, result) = get_json(&fixture, &fixture.parent, &delegation, "result").await;
    assert!(result_notes(&result).contains("intent-to-add"));
    assert_eq!(
        run_git_test_command_output(&fixture.source, &["rev-parse", "HEAD"]),
        parent_head
    );
    assert_eq!(
        run_git_test_command_output(&fixture.source, &["write-tree"]),
        parent_tree
    );
    assert_eq!(
        run_git_test_command_output(
            &fixture.source,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        parent_status
    );
    assert_eq!(
        fs::read_to_string(fixture.source.join("parent-only.txt")).expect("parent file"),
        "keep parent dirt\n"
    );
    assert_eq!(
        run_git_test_command_output(&fixture.worktree, &["rev-parse", "HEAD"]),
        base
    );
    assert_eq!(
        fs::read(run_git_test_command_output(
            &fixture.worktree,
            &["rev-parse", "--git-path", "index"]
        ))
        .expect("retained child index"),
        child_index
    );
    assert!(fixture.worktree.join(".git").is_file());
}

#[tokio::test]
async fn delegation_worker_handoff_persistence_and_legacy_result_shape_remain_compatible() {
    let (fixture, delegation, child, base) = start_worker().await;
    finish_worker(
        &fixture,
        &child,
        "completed",
        "- Git handoff: unavailable.\n- Child-reported native test outcome: unavailable.",
    );
    let (_, result) = get_json(&fixture, &fixture.parent, &delegation, "result").await;
    let saved = {
        let inner = fixture.state.inner.lock().expect("state lock");
        persist_state(&fixture.state.persistence_path, &inner)
    };
    saved.expect("synchronous persistence snapshot");
    let mut loaded = load_state(&fixture.state.persistence_path)
        .expect("SQLite load")
        .expect("state");
    let loaded_result = loaded
        .delegations
        .iter()
        .find(|record| record.id == delegation)
        .and_then(|record| record.result.as_ref())
        .expect("persisted terminal worker result");
    let value = serde_json::to_value(loaded_result).expect("result JSON");
    assert_eq!(value["baseRevision"], base);
    assert_eq!(value["workerOutputUnavailable"], false);
    assert_eq!(
        value["worktreePath"],
        fixture.worktree.to_string_lossy().as_ref()
    );
    let mut legacy = result["result"].clone();
    let object = legacy.as_object_mut().expect("legacy object");
    object.remove("baseRevision");
    object.remove("worktreePath");
    object.remove("workerOutputUnavailable");
    let decoded: DelegationResult = serde_json::from_value(legacy).expect("legacy result");
    let reencoded = serde_json::to_value(&decoded).expect("legacy reserialization");
    assert!(reencoded.get("baseRevision").is_none());
    assert!(reencoded.get("worktreePath").is_none());
    assert!(reencoded.get("workerOutputUnavailable").is_none());
    let index = loaded
        .find_delegation_index(&delegation)
        .expect("loaded delegation");
    loaded.delegations[index].result = Some(decoded);
    loaded.mark_delegation_mutated(index);
    persist_state(&fixture.state.persistence_path, &loaded).expect("persist legacy result");
    let legacy_loaded = load_state(&fixture.state.persistence_path)
        .expect("legacy reload")
        .expect("state");
    let result = legacy_loaded
        .delegations
        .iter()
        .find(|record| record.id == delegation)
        .and_then(|record| record.result.as_ref())
        .expect("legacy persisted result");
    assert_eq!(result.worker_output_unavailable, None);
    assert!(
        serde_json::to_value(result)
            .expect("legacy JSON")
            .get("workerOutputUnavailable")
            .is_none()
    );
}
