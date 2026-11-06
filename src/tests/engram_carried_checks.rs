// Carried background/detached launch observations: exact artifact discovery,
// original terminal digest, conservative interference, shell/worktree scope,
// bounded retention and restart disclosure. Ordinary closing keeps these
// facts separate from explicit native verification; it does not consume a
// launch at a checkpoint. Uses the actual ordinary turn fixture owned by
// src/tests/engram_turn_checks.rs, whose in-turn checks remain separate.
use super::*;

const GATE: &str = "node scripts/test-launcher.mjs full";
const DETACHED_GATE: &str = "node scripts/test-launcher.mjs full --detach";

fn ordinary_carried_turn(label: &str) -> (EOCheckedTurn, PathBuf) {
    let turn = EOCheckedTurn::start_named(label);
    let worktree = turn.root.join(".worktrees").join("wt");
    (turn, worktree)
}

fn ordinary_launch_gate(turn: &EOCheckedTurn, worktree: &FsPath, detached: bool) {
    let command = if detached { DETACHED_GATE } else { GATE };
    let cwd = fs::canonicalize(worktree).unwrap().to_string_lossy().into_owned();
    let mut recorder = turn.recorder();
    recorder.command_started_in("gate", command, Some(command), Some(&cwd)).unwrap();
    turn.wait_for_snapshots();
    recorder.command_completed_with_exit("gate", command, "STARTED", CommandStatus::Success,
        if detached { EngramCommandExit::Code(0) } else { EngramCommandExit::NotFinished }).unwrap();
    turn.wait_for_snapshots();
    turn.record(|record| assert_eq!(record.engram.carried_checks.len(), 1));
}

// Control only the already-existing provider-boundary clock, not source
// ownership or a check's snapshots. Keep the two launches inside the real
// matcher's slack without a wall-clock race or a sleep.
struct LaunchCustodyClock(Option<(String, Option<String>)>);

impl LaunchCustodyClock {
    fn at(started_at: &str) -> Self {
        Self(TEST_ENGRAM_COMMAND_START_TIMING.with(|slot| {
            slot.replace(Some((started_at.to_owned(), None)))
        }))
    }

    fn advance_to(&self, started_at: &str) {
        TEST_ENGRAM_COMMAND_START_TIMING.with(|slot| {
            slot.replace(Some((started_at.to_owned(), None)));
        });
    }
}

impl Drop for LaunchCustodyClock {
    fn drop(&mut self) {
        TEST_ENGRAM_COMMAND_START_TIMING.with(|slot| slot.replace(self.0.take()));
    }
}

fn linked_unbound_launch_cannot_lend_its_run(description_only: bool) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let label = if description_only { "linked-custody-description" } else { "linked-custody-start" };
    let turn = EOCheckedTurn::start(label, false);
    let b = turn.root.join(".worktrees").join("b");
    run_git_test_command(&turn.root, &[
        "worktree", "add", "--quiet", "-b", "b", b.to_str().unwrap(),
    ]);
    let b = fs::canonicalize(b).unwrap();
    let cwd = b.to_string_lossy().into_owned();
    assert_ne!(engram_exact_path_key(&turn.root), engram_exact_path_key(&b));
    let first_time = chrono::Utc::now() - chrono::Duration::seconds(4);
    let at = |offset| (first_time + chrono::Duration::seconds(offset))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let clock = LaunchCustodyClock::at(&at(0));
    let provenance = turn.record(|record| EngramObservationProvenance::Runtime {
        token: record.runtime.runtime_token().unwrap(),
        turn_generation: record.active_turn_generation,
    });
    let mut recorder = turn.recorder();
    recorder.set_observation_provenance(provenance.clone());
    if description_only {
        recorder.command_started_in("old", "pending command", None, Some(&cwd)).unwrap();
        recorder.command_described("old", Some(DETACHED_GATE), Some(&cwd)).unwrap();
    } else {
        recorder.command_started_in("old", DETACHED_GATE, Some(DETACHED_GATE), Some(&cwd)).unwrap();
    }
    clock.advance_to(&at(1));
    recorder.command_started_in("old", DETACHED_GATE, Some(DETACHED_GATE), Some(&cwd)).unwrap();
    turn.record(|record| {
        assert_eq!(record.engram.carried_unmatched_launches,
            vec![(engram_path_key(&b), at(0))],
            "a repeated provider frame keeps one original B/start exclusion");
    });
    recorder.command_completed_with_exit("old", DETACHED_GATE, "STARTED",
        CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    let old = {
        let inner = turn.state.inner.lock().unwrap();
        inner.test_run_observations.values().find(|row| {
            row.summary.owner_session_id.as_deref() == Some(turn.session_id.as_str())
                && row.tool_identity == sha256_hex(b"old")
        }).expect("the original provider execution is recorded").clone()
    };
    assert_eq!(old.summary.started_at.as_deref(), Some(at(0).as_str()));
    assert!(old.summary.worktree.is_empty(), "location observation cannot widen credit eligibility");
    assert!(old.work_binding.is_none() && old.source_root.is_none());
    assert!(old.artifact.is_none() && old.evidence.is_empty() && old.final_unbound_proof.is_none());
    turn.record(|record| {
        assert!(record.engram.work_binding.is_none());
        assert!(record.engram.active_turn_checks.is_empty() && record.engram.carried_checks.is_empty());
    });

    let run_id = "test-original-linked-launch";
    let run = review_runs(&b).join(run_id);
    fs::create_dir_all(&run).unwrap();
    let request = serde_json::to_vec(&json!({
        "runId": run_id, "root": cwd, "owner": turn.session_id,
        "full": true, "detached": true, "started": at(1),
        "expectedFingerprint": "f".repeat(64),
        "stages": [{ "name": "rust-tests", "kind": "test" }],
    })).unwrap();
    fs::write(run.join("request.json"), &request).unwrap();
    fs::write(run.join("results.json"), serde_json::to_vec(&json!({
        "runId": run_id, "state": "running", "started": at(1),
        "stages": [{ "name": "rust-tests", "state": "unrun" }],
    })).unwrap()).unwrap();
    assert!(engram_terminal_record_digest(&run).is_none());
    // An unclaimed turn closes quietly; there is no attributed report to await.
    turn.end();
    let guard = phase_sync::PollGuard::new();
    while turn.record(|record| record.engram.unadmitted_worker_running
        || runtime_observation_has_removal_duty(record))
    {
        guard.wait(format_args!("the original ordinary observation settles before naming B"));
    }
    turn.record(|record| {
        assert!(record.engram.unadmitted_active_turn.is_none());
        assert!(record.engram.unadmitted_closing_jobs.is_empty());
        assert!(record.engram.unadmitted_reports.is_empty());
    });
    assert_ordinary_turn_has_no_native_requests(&turn);

    // Claim association and root naming go through the actual ordinary work
    // service. No active trace, binding or check owner is rewritten by hand.
    let binding = test_control_work_binding(&format!("eo-check-{label}"), 1);
    turn.transport.set_current_work_binding(Some(binding.clone()));
    turn.transport.replace_held_claims([Ok(EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: binding.work_id.clone(), short_ref: binding.work_id.clone(),
            claim_id: binding.claim_id.clone(), claim_fence: binding.claim_fence,
            focused: true, control_binding: Some(binding.clone()),
        }], omitted: 0,
    })]);
    turn.state.name_engram_source_root(&turn.session_id, EngramSourceRootRequest {
        work: binding.work_id.clone(), path: Some(Some(cwd.clone())),
    }).expect("the newly held claim names the real linked worktree B");
    turn.record(|record| assert_eq!(record.engram.work_binding.as_ref(), Some(&binding)));
    turn.begin("Launch a distinct invocation in the now named source B.");
    clock.advance_to(&at(2));
    let mut later = turn.recorder();
    later.set_observation_provenance(turn.record(|record| EngramObservationProvenance::Runtime {
        token: record.runtime.runtime_token().unwrap(), turn_generation: record.active_turn_generation,
    }));
    later.command_started_in("new", DETACHED_GATE, Some(DETACHED_GATE), Some(&cwd)).unwrap();
    turn.wait_for_snapshots();
    later.command_completed_with_exit("new", DETACHED_GATE, "STARTED",
        CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    // The old Running artifact is a real match for the unchanged root/owner/
    // ten-second reader. Only invocation ambiguity may stop the new carry.
    assert_eq!(engram_find_carried_run(&turn.session_id, &b, &at(2), &[],
        &std::collections::BTreeSet::new()).0, Some(run.clone()));
    turn.state.poll_engram_carried_runs();
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        let carried = &record.engram.carried_checks[0];
        assert_eq!(carried.check.key, "new");
        assert!(carried.fence.is_none(), "an unrelated writer fence must not mask borrowing");
        assert!(carried.ambiguous, "the original B invocation excludes ambiguous correlation");
        assert!(carried.run_directory.is_none() && carried.terminal_digest.is_none());
    });
    fs::write(run.join("results.json"), serde_json::to_vec(&json!({
        "runId": run_id, "state": "passed", "started": at(1),
        "ended": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "expectedFingerprint": "f".repeat(64), "before": "f".repeat(64),
        "after": "f".repeat(64), "exitCode": 0,
        "stages": [{ "name": "rust-tests", "state": "passed", "code": 0 }],
    })).unwrap()).unwrap();
    let digest = engram_terminal_record_digest(&run).unwrap();
    assert!(engram_read_carried_run(&run, &digest).is_ok(), "the older artifact validates on its own");
    assert_eq!(fs::read(run.join("request.json")).unwrap(), request);
    assert_carried_ambiguity(&turn);
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    let inner = turn.state.inner.lock().unwrap();
    assert_eq!(inner.test_run_observations.get(&old.execution_id), Some(&old),
        "later binding/naming/carry must not lend the old execution any authority or artifact");
}

#[test]
fn unbound_linked_worktree_launch_cannot_be_borrowed_after_naming_it() {
    linked_unbound_launch_cannot_lend_its_run(false);
}

#[test]
fn first_description_of_unbound_linked_launch_keeps_its_original_custody() {
    linked_unbound_launch_cannot_lend_its_run(true);
}

fn assert_carried_ambiguity(turn: &EOCheckedTurn) {
    let identity = turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        let carried = &record.engram.carried_checks[0];
        assert!(carried.ambiguous, "another possibly launched run must prevent correlation");
        (carried.run_directory.clone(), carried.terminal_digest.clone())
    });
    turn.state.poll_engram_carried_runs();
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        let carried = &record.engram.carried_checks[0];
        assert!(carried.ambiguous, "another possibly launched run must prevent correlation");
        assert_eq!((carried.run_directory.clone(), carried.terminal_digest.clone()), identity,
            "an ambiguous launch cannot acquire another run or a new terminal record");
    });
}

fn assert_carried_artifact_custody(turn: &EOCheckedTurn, run: &FsPath, passed: bool) -> Value {
    let digest = engram_terminal_record_digest(run).expect("the exact fixture artifact is terminal");
    let verdict = engram_read_carried_run(run, &digest).expect("the artifact validates independently");
    assert_eq!(verdict.passed, passed);
    assert_eq!(verdict.exit_code, if passed { 0 } else { 101 });
    assert_eq!(verdict.fingerprint, "f".repeat(64));
    turn.state.poll_engram_carried_runs();
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        let carried = &record.engram.carried_checks[0];
        assert!(!carried.ambiguous && carried.fence.is_none());
        assert_eq!(carried.run_directory.as_deref(), Some(run));
        assert_eq!(carried.terminal_digest.as_deref(), Some(digest.as_str()));
    });
    let observation = turn.finish();
    let facts = observation["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(facts.len(), 1, "{observation:#}");
    assert_eq!(facts[0]["check_kind"], "test");
    assert_eq!(facts[0]["host_check_id"], unadmitted_host_check_id(
        observation["occurrence"]["host_turn_ref"].as_str().unwrap(), "gate"));
    assert_eq!(facts[0]["observed_result"], "indeterminate",
        "a launch acknowledgement is not a test result");
    assert!(facts[0].get("finished_at").is_none());
    assert_ordinary_observation_has_no_native_credit(turn, &observation);
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1,
            "ordinary closing never consumes an artifact into verification");
        assert_eq!(record.engram.carried_checks[0].terminal_digest.as_deref(), Some(digest.as_str()));
    });
    observation
}

fn assert_carried_fence_custody(turn: &EOCheckedTurn, why: &str) -> String {
    let cause = turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        record.engram.carried_checks[0].fence.clone().expect("the actual recorder fenced this launch")
    });
    assert!(cause.contains(why), "{cause}");
    assert_ordinary_observation_has_no_native_credit(turn, &turn.finish());
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        assert_eq!(record.engram.carried_checks[0].fence.as_deref(), Some(cause.as_str()),
            "ordinary closing preserves the original cause without consuming verification");
    });
    cause
}

fn assert_carried_artifact_rejected(turn: &EOCheckedTurn, run: &FsPath, why: &str) {
    turn.state.poll_engram_carried_runs();
    let digest = turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        let carried = &record.engram.carried_checks[0];
        assert_eq!(carried.run_directory.as_deref(), Some(run));
        carried.terminal_digest.clone().expect("the real poll pins a terminal identity")
    });
    let error = engram_read_carried_run(run, &digest).expect_err("the real artifact reader rejects it");
    assert!(error.contains(why), "{error}");
    assert_ordinary_observation_has_no_native_credit(turn, &turn.finish());
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        assert_eq!(record.engram.carried_checks[0].terminal_digest.as_deref(), Some(digest.as_str()));
        assert!(record.engram.carried_consumed_runs.is_empty());
    });
}

#[test]
fn late_launch_basis_completion_does_not_turn_artifact_custody_into_verification() {
    let (turn, worktree) = ordinary_carried_turn("ordinary-late-launch-basis");
    ordinary_launch_gate(&turn, &worktree, false);
    let original = turn.record(|record| record.engram.carried_checks[0].check.start_basis.clone());
    let basis = original.wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
        .expect("the actual original launch capture finished");
    let pending = Arc::new(EngramBasisCapture::default());
    turn.record_mut(|record| record.engram.carried_checks[0].check.start_basis = pending.clone());
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let digest = engram_terminal_record_digest(&run).unwrap();
    turn.state.poll_engram_carried_runs();
    assert_eq!(pending.wait_until(std::time::Instant::now()), None);
    let observation = turn.finish();
    assert_ordinary_observation_has_no_native_credit(&turn, &observation);
    let facts = observation["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0]["observed_result"], "indeterminate");
    assert!(facts[0].get("finished_at").is_none());
    pending.finish(basis);
    turn.state.poll_engram_carried_runs();
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        let carried = &record.engram.carried_checks[0];
        assert_eq!(carried.run_directory.as_deref(), Some(run.as_path()));
        assert_eq!(carried.terminal_digest.as_deref(), Some(digest.as_str()));
        assert!(record.engram.carried_consumed_runs.is_empty());
    });
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[path = "engram_quoted_pipe_carry.rs"]
mod quoted_pipe;

#[path = "engram_launch_window_retirement.rs"]
mod launch_window_retirement;

#[path = "engram_description_window_retirement.rs"]
mod description_window_retirement;

#[path = "test_run_restart_evidence.rs"]
mod test_run_restart_evidence;

mod credit_outcomes {
    use super::*;
    include!("engram_carried_credit_outcomes.rs");
}

// What overlaps a launch and how its refusal names it, a child module so
// this file does not grow further.
#[path = "engram_overlap_cause.rs"]
mod overlap_cause;

// How a watcher-seen change is judged by its own time against a gate's
// launch, a child module so this file does not grow further.
#[path = "engram_watcher_change_time.rs"]
mod watcher_change_time;

#[path = "engram_cargo_target_sibling.rs"]
mod cargo_target_sibling;

#[path = "engram_watcher_metadata.rs"]
mod watcher_metadata;

#[path = "engram_watcher_provenance.rs"]
mod watcher_provenance;

#[path = "engram_watcher_pending.rs"]
mod watcher_pending;

// A Codex test carried past its turn until its end is reported, a child
// module so this file does not grow further.
#[path = "engram_reported_ends.rs"]
mod reported_ends;

// A passed batch gate bound to its other members, end to end, a child
// module so this file does not grow further.
#[path = "engram_batch_gate_flow.rs"]
mod batch_gate_flow;

// The host's guard on Claude file writes into a running gate's worktree, a
// child module so this file does not grow further.
#[path = "engram_gate_write_guard.rs"]
mod gate_write_guard;

#[test]
fn a_full_gate_launch_is_carried_dropped_or_ordinary_by_its_line_and_result() {
    use EngramLaunchDisposition::{Carry, Drop, Ordinary};
    let check = |line: &str| engram_check_command(line).expect("a recognised test");
    let disposition = |line: &str, exit| engram_launch_disposition(&check(line), exit);
    assert_eq!(disposition(GATE, EngramCommandExit::NotFinished), Carry);
    assert_eq!(disposition(GATE, EngramCommandExit::Code(0)), Ordinary);
    assert_eq!(
        disposition(DETACHED_GATE, EngramCommandExit::Code(0)),
        Carry
    );
    assert_eq!(
        disposition(DETACHED_GATE, EngramCommandExit::ReportedSuccess),
        Carry
    );
    assert_eq!(
        disposition(DETACHED_GATE, EngramCommandExit::NotFinished),
        Carry
    );
    // A detached launch that failed started no run: dropped, never recorded.
    for exit in [EngramCommandExit::Code(1), EngramCommandExit::Unknown] {
        assert!(
            matches!(disposition(DETACHED_GATE, exit), Drop(why) if why.contains("launch failed")),
            "{exit:?}"
        );
    }
    // A launch on a line that runs more than the gate is dropped: what the
    // rest wrote in that call is outside the fence.
    // A foreground one that finished is an ordinary check.
    let compound = "node scripts/test-launcher.mjs full; git checkout -- README.md";
    for (line, exit) in [
        (compound, EngramCommandExit::NotFinished),
        (
            "node scripts/test-launcher.mjs full --detach --notify session-1 && echo x > README.md",
            EngramCommandExit::NotFinished,
        ),
        (
            "node scripts/test-launcher.mjs full --detach --notify session-1 && echo x > README.md",
            EngramCommandExit::Code(0),
        ),
        (
            "node scripts/test-launcher.mjs full --detach&& git checkout -- README.md",
            EngramCommandExit::Code(0),
        ),
    ] {
        assert!(
            matches!(disposition(line, exit), Drop(why) if why.contains("runs more than the gate")),
            "{line} {exit:?}"
        );
    }
    assert_eq!(disposition(compound, EngramCommandExit::Code(0)), Ordinary);
    assert_eq!(
        disposition(
            "node scripts/test-launcher.mjs live",
            EngramCommandExit::NotFinished
        ),
        Ordinary
    );
    assert_eq!(
        disposition("cargo test", EngramCommandExit::NotFinished),
        Ordinary
    );
    // Only a full gate may be detached and still be recognised.
    assert!(engram_check_command("node scripts/test-launcher.mjs live --detach").is_none());
    assert!(
        engram_check_command("node scripts/test-launcher.mjs focused --detach -- cargo test")
            .is_none()
    );
}

#[test]
fn only_a_command_that_reads_leaves_a_carried_check_unfenced() {
    let workdir = if cfg!(windows) { r"C:\repo" } else { "/repo" };
    for reads in [
        "git status",
        "git diff --stat",
        "node scripts/test-launcher.mjs summary C:/repo/.git/review-runs/test-1",
        "cat README.md",
    ] {
        assert!(engram_command_reads_only(reads, workdir), "{reads}");
    }
    for writes in [
        "cargo build",
        "echo changed > README.md",
        "node scripts/test-launcher.mjs full",
        "node scripts/test-launcher.mjs summary a b",
        "node scripts/test-launcher.mjs summary run; rm README.md",
        "git checkout -- README.md",
        // Only the repository's own launcher, run from its root, counts.
        "node .tmp/my-test-launcher.mjs summary run",
        "node tools/scripts/test-launcher.mjs summary run",
        "node C:/elsewhere/scripts/test-launcher.mjs summary run",
        "node /elsewhere/scripts/test-launcher.mjs summary run",
    ] {
        assert!(!engram_command_reads_only(writes, workdir), "{writes}");
    }
    // The same launcher written absolute, or from the one-call form.
    let absolute = format!(
        "node {}/scripts/test-launcher.mjs summary run",
        workdir.replace('\\', "/")
    );
    assert!(engram_command_reads_only(&absolute, workdir), "{absolute}");
    let one_call = format!("pushd \"{workdir}\" && node ./scripts/test-launcher.mjs summary run");
    assert!(
        engram_command_reads_only(&one_call, "/somewhere/else"),
        "{one_call}"
    );
}

#[test]
fn a_fence_names_the_holders_command_without_its_line() {
    let cause = engram_own_command_cause(
        "curl -H \"Authorization: Bearer sk-secret-123\" https://example.com -o out.txt",
    );
    assert!(cause.contains("`curl` command"), "{cause}");
    assert!(!cause.contains("sk-secret-123"), "{cause}");
    assert!(!cause.contains("Authorization"), "{cause}");
    let directory = if cfg!(windows) { "C:/repo" } else { "/repo" };
    let one_call = engram_own_command_cause(&format!("pushd \"{directory}\" && cargo build --token=abc"));
    assert!(one_call.contains("`cargo` command"), "{one_call}");
    assert!(!one_call.contains("abc"), "{one_call}");
    // A leading assignment is skipped, whatever its value.
    for (line, program, secret) in [
        (
            "GITHUB_TOKEN=ghp_secret123 gh api repos",
            "`gh` command",
            "ghp_secret123",
        ),
        (
            "env API_KEY=abc123 npm run build",
            "`npm` command",
            "abc123",
        ),
    ] {
        let cause = engram_own_command_cause(line);
        assert!(cause.contains(program), "{cause}");
        assert!(!cause.contains(secret), "{cause}");
    }
}

/// A process that has exited, with its pid, which fits every platform's pid
/// type. The caller keeps the child: on Windows, holding its handle keeps the
/// pid from being reused while the test runs.
fn exited_process() -> (std::process::Child, u32) {
    #[cfg(windows)]
    let mut child = {
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        // No console window: a test must not flash one on the desktop.
        Command::new("cmd")
            .args(["/C", "exit 0"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("a short process should start")
    };
    #[cfg(not(windows))]
    let mut child = Command::new("true")
        .spawn()
        .expect("a short process should start");
    let pid = child.id();
    assert!(child.wait().expect("the process should exit").success());
    (child, pid)
}

/// A run directory named `run_id` under `runs`, with a request for `root`
/// started at `started` and a terminal record: `state`, its stages each
/// `stage_state`, and fingerprints expected `fingerprint`, after `after`.
fn write_run(
    runs: &FsPath,
    run_id: &str,
    root: &FsPath,
    started: &str,
    state: &str,
    after: &str,
) -> PathBuf {
    let directory = runs.join(run_id);
    fs::create_dir_all(&directory).expect("the run directory should be created");
    let stages = [
        "cargo-check",
        "typescript",
        "fingerprint-tests",
        "rust-tests",
        "vitest",
    ];
    let fingerprint = "f".repeat(64);
    fs::write(
        directory.join("request.json"),
        serde_json::to_vec(&json!({
            "runId": run_id,
            "root": root.to_string_lossy(),
            "stages": stages.iter().map(|name| json!({ "name": name })).collect::<Vec<_>>(),
            "full": true,
            "detached": false,
            "started": started,
            "expectedFingerprint": fingerprint,
        }))
        .expect("the request serializes"),
    )
    .expect("the request should write");
    let passed = state == "passed";
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec(&json!({
            "runId": run_id,
            "state": state,
            "started": started,
            "stages": stages
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    let stage_passed = passed || index < 3;
                    json!({
                        "name": name,
                        "state": if stage_passed { "passed" } else { "failed" },
                        "code": if stage_passed { 0 } else { 101 },
                    })
                })
                .collect::<Vec<_>>(),
            "expectedFingerprint": fingerprint,
            "before": fingerprint,
            "after": after,
            "exitCode": if passed { 0 } else { 101 },
            "ended": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        }))
        .expect("the results serialize"),
    )
    .expect("the results should write");
    directory
}

#[test]
fn a_run_record_is_credited_only_as_it_was_first_read_and_consistent() {
    let temp = TestTempRoot::create("termal-engram-carried-run");
    let root = temp.path().join("root");
    let fingerprint = "f".repeat(64);
    let started = "2026-09-29T00:00:00.000Z";
    let passed = write_run(
        temp.path(),
        "test-passed",
        &root,
        started,
        "passed",
        &fingerprint,
    );
    let digest = engram_terminal_record_digest(&passed).expect("a terminal record");
    let verdict = engram_read_carried_run(&passed, &digest).expect("a consistent passed run");
    assert!(verdict.passed);
    assert_eq!(verdict.exit_code, 0);
    assert_eq!(verdict.stages.len(), 5);
    assert_eq!(verdict.fingerprint, fingerprint);
    assert_eq!(
        engram_read_carried_run(&passed, &"0".repeat(64)),
        Err("its terminal record changed after the host first read it as terminal")
    );

    let failed = write_run(
        temp.path(),
        "test-failed",
        &root,
        started,
        "failed",
        &fingerprint,
    );
    let digest = engram_terminal_record_digest(&failed).expect("a terminal record");
    let verdict = engram_read_carried_run(&failed, &digest).expect("a consistent failed run");
    assert!(!verdict.passed);
    assert_eq!(verdict.exit_code, 101);

    let drifted = write_run(
        temp.path(),
        "test-drifted",
        &root,
        started,
        "passed",
        &"e".repeat(64),
    );
    let digest = engram_terminal_record_digest(&drifted).expect("a terminal record");
    assert_eq!(
        engram_read_carried_run(&drifted, &digest),
        Err("its input fingerprint after the run differs from the one before")
    );

    // A record with no fingerprint from before or after its stages (a run
    // that ended in its preflight) cannot say what input it tested.
    let unmeasured = write_run(
        temp.path(),
        "test-unmeasured",
        &root,
        started,
        "failed",
        &fingerprint,
    );
    let mut record: Value =
        serde_json::from_slice(&fs::read(unmeasured.join("results.json")).unwrap()).unwrap();
    record.as_object_mut().unwrap().remove("before");
    record.as_object_mut().unwrap().remove("after");
    fs::write(
        unmeasured.join("results.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let digest = engram_terminal_record_digest(&unmeasured).expect("a terminal record");
    assert!(
        engram_read_carried_run(&unmeasured, &digest)
            .is_err_and(|why| why.contains("no input fingerprint from before or after")),
    );

    // A record that says passed while a stage it requested did not.
    let lying = write_run(
        temp.path(),
        "test-lying",
        &root,
        started,
        "failed",
        &fingerprint,
    );
    let mut record: Value =
        serde_json::from_slice(&fs::read(lying.join("results.json")).expect("the results read"))
            .expect("the results parse");
    record["state"] = json!("passed");
    record["exitCode"] = json!(0);
    fs::write(
        lying.join("results.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let digest = engram_terminal_record_digest(&lying).expect("a terminal record");
    assert_eq!(
        engram_read_carried_run(&lying, &digest),
        Err("its terminal record says passed, but not every stage it requested passed with code 0")
    );

    // A run whose stages all passed but which requested no test stage (its
    // record gives no kinds and names neither of TermAl's test stages): a
    // foreground gate would not be credited on it either.
    let compile_only = write_run(
        temp.path(),
        "test-compile-only",
        &root,
        started,
        "passed",
        &fingerprint,
    );
    for name in ["request.json", "results.json"] {
        let mut record: Value =
            serde_json::from_slice(&fs::read(compile_only.join(name)).unwrap()).unwrap();
        record["stages"] = json!([{ "name": "cargo-check", "state": "passed", "code": 0 }]);
        fs::write(
            compile_only.join(name),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }
    let digest = engram_terminal_record_digest(&compile_only).expect("a terminal record");
    assert_eq!(
        engram_read_carried_run(&compile_only, &digest),
        Err(ENGRAM_CARRIED_RUN_NO_TEST_STAGE)
    );

    // A run still going has no terminal record.
    let running = write_run(
        temp.path(),
        "test-running",
        &root,
        started,
        "passed",
        &fingerprint,
    );
    fs::write(
        running.join("results.json"),
        serde_json::to_vec(&json!({ "runId": "test-running", "state": "running" })).unwrap(),
    )
    .unwrap();
    assert_eq!(engram_terminal_record_digest(&running), None);
    // No recorded pid: its launcher is never judged gone.
    assert_eq!(engram_carried_run_progress(&running), (None, false));
    // A recorded pid whose process has exited: gone, so the run will never
    // end.
    let (_exited, dead_pid) = exited_process();
    fs::write(
        running.join("results.json"),
        serde_json::to_vec(
            &json!({ "runId": "test-running", "state": "running", "pid": dead_pid }),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(engram_carried_run_progress(&running), (None, true));

    // Engram's launcher settles a dead run as stopped, with no `ended` and no
    // exit code: terminal all the same, and neither a pass nor a failure.
    fs::write(
        running.join("results.json"),
        serde_json::to_vec(&json!({
            "runId": "test-running",
            "state": "stopped",
            "stopped": { "stage": "rust-tests" },
        }))
        .unwrap(),
    )
    .unwrap();
    let digest = engram_terminal_record_digest(&running).expect("a stopped record is terminal");
    assert_eq!(
        engram_read_carried_run(&running, &digest),
        Err(ENGRAM_CARRIED_RUN_NEITHER)
    );

    // A run that ended stopped, or failed but interrupted, is terminal and
    // is neither a pass nor a failure.
    for (name, state, interrupted) in [
        ("test-stopped", "stopped", false),
        ("test-interrupted", "failed", true),
    ] {
        let directory = write_run(temp.path(), name, &root, started, "failed", &fingerprint);
        let mut record: Value =
            serde_json::from_slice(&fs::read(directory.join("results.json")).unwrap()).unwrap();
        record["state"] = json!(state);
        if interrupted {
            record["interrupted"] = json!(true);
        }
        fs::write(
            directory.join("results.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let digest =
            engram_terminal_record_digest(&directory).expect("an ended record is terminal");
        assert_eq!(
            engram_read_carried_run(&directory, &digest),
            Err(ENGRAM_CARRIED_RUN_NEITHER),
            "{name}"
        );
    }
}

#[test]
fn a_launch_takes_the_earliest_full_run_of_its_root_that_started_after_it() {
    let temp = TestTempRoot::create("termal-engram-carried-run");
    let root = temp.path().join("root");
    let runs = root.join(".git").join("review-runs");
    fs::create_dir_all(&runs).expect("the runs directory should be created");
    let fingerprint = "f".repeat(64);
    // Minutes apart, well beyond the slack a launch stamp is given.
    let at = |minute: u32| format!("2026-09-29T00:{minute:02}:00.000Z");
    write_run(&runs, "test-before", &root, &at(1), "passed", &fingerprint);
    write_run(
        &runs,
        "test-elsewhere",
        &temp.path().join("other"),
        &at(3),
        "passed",
        &fingerprint,
    );
    let first = write_run(&runs, "test-first", &root, &at(4), "passed", &fingerprint);
    let second = write_run(&runs, "test-second", &root, &at(5), "passed", &fingerprint);

    assert_eq!(engram_git_run_directory(&root), Some(runs.clone()));
    let none = std::collections::BTreeSet::new();
    let (found, ruled_out, _) = engram_find_carried_run("session-1", &root, &at(2), &[], &none);
    assert_eq!(found, Some(first.clone()));
    // Runs that can never be this launch's are ruled out for good.
    let mut ruled_out = ruled_out;
    ruled_out.sort();
    assert_eq!(
        ruled_out,
        [runs.join("test-before"), runs.join("test-elsewhere")]
    );
    let (found, _, _) =
        engram_find_carried_run("session-1", &root, &at(2), &[first.clone()], &none);
    assert_eq!(found, Some(second.clone()));
    assert_eq!(
        engram_find_carried_run("session-1", &root, &at(6), &[], &none).0,
        None
    );
    // A ruled-out run is not read again, so a search sees only the others.
    let skip: std::collections::BTreeSet<PathBuf> = [first.clone(), runs.join("test-second")]
        .into_iter()
        .collect();
    assert_eq!(
        engram_find_carried_run("session-1", &root, &at(2), &[], &skip).0,
        None
    );
    // A run the launcher records as another session's is never this one's;
    // a request with no owner may be anyone's.
    let mut request: Value =
        serde_json::from_slice(&fs::read(first.join("request.json")).unwrap()).unwrap();
    request["owner"] = json!("session-2");
    fs::write(
        first.join("request.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    let (found, ruled_out, _) = engram_find_carried_run("session-1", &root, &at(2), &[], &none);
    assert_eq!(found, Some(second));
    assert!(ruled_out.contains(&first), "{ruled_out:?}");
    assert_eq!(
        engram_find_carried_run("session-2", &root, &at(2), &[], &none).0,
        Some(first)
    );
}

#[test]
fn a_run_that_ends_while_its_launcher_is_checked_is_read_as_ended() {
    let temp = TestTempRoot::create("termal-engram-carried-liveness");
    let root = temp.path().join("root");
    let fingerprint = "f".repeat(64);
    let directory = write_run(
        temp.path(),
        "test-racing",
        &root,
        "2026-09-29T00:00:00.000Z",
        "passed",
        &fingerprint,
    );
    let terminal = fs::read(directory.join("results.json")).unwrap();
    let running = |pid: u64| {
        serde_json::to_vec(&json!({ "runId": "test-racing", "state": "running", "pid": pid }))
            .unwrap()
    };

    // The launcher writes its terminal record and exits between the read
    // that found none and the check that finds it gone: read again, ended.
    fs::write(directory.join("results.json"), running(41)).unwrap();
    let (digest, gone) = engram_carried_run_progress_with(&directory, |pid, _| {
        assert_eq!(pid, 41);
        fs::write(directory.join("results.json"), &terminal).unwrap();
        false
    });
    assert_eq!(digest, Some(sha256_hex(&terminal)));
    assert!(!gone);

    // A detached creator hands over to its worker between the read and the
    // check: the worker is checked too, and it is alive.
    fs::write(directory.join("results.json"), running(41)).unwrap();
    let checked = std::cell::RefCell::new(Vec::new());
    let (digest, gone) = engram_carried_run_progress_with(&directory, |pid, _| {
        checked.borrow_mut().push(pid);
        if pid == 41 {
            fs::write(directory.join("results.json"), running(42)).unwrap();
            false
        } else {
            true
        }
    });
    assert_eq!((digest, gone), (None, false));
    assert_eq!(*checked.borrow(), [41, 42]);

    // Still not terminal after the check, under the same process: gone.
    fs::write(directory.join("results.json"), running(41)).unwrap();
    assert_eq!(
        engram_carried_run_progress_with(&directory, |_, _| false),
        (None, true)
    );
}

/// The launcher's run directory for `worktree`, from Git itself.
fn review_runs(worktree: &FsPath) -> PathBuf {
    PathBuf::from(
        run_git_test_command_output(
            worktree,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "review-runs",
            ],
        )
        .trim(),
    )
}

/// Writes the gate's run for `worktree`, started after its launch, ending
/// `state` with its input fingerprint after the run `after`.
fn finish_run(worktree: &FsPath, state: &str, after: &str) -> PathBuf {
    let runs = review_runs(worktree);
    let canonical = fs::canonicalize(worktree).expect("the worktree canonicalizes");
    write_run(
        &runs,
        "test-carried",
        &canonical,
        &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        state,
        after,
    )
}

#[test]
fn a_background_gate_keeps_its_passed_artifact_without_automatic_verification() {
    let (turn, worktree) = ordinary_carried_turn("carried-background");
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn a_detached_gate_keeps_its_passed_artifact_without_automatic_verification() {
    let (turn, worktree) = ordinary_carried_turn("carried-detached");
    ordinary_launch_gate(&turn, &worktree, true);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn a_background_gate_keeps_its_failed_artifact_without_relabeling_the_launch_fact() {
    let (turn, worktree) = ordinary_carried_turn("carried-failed");
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "failed", &"f".repeat(64));
    assert_carried_artifact_custody(&turn, &run, false);
}

#[test]
fn a_gate_whose_run_has_not_ended_stays_carried() {
    let (turn, worktree) = ordinary_carried_turn("carried-running");
    ordinary_launch_gate(&turn, &worktree, false);
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(carried.run_directory.is_none() && carried.terminal_digest.is_none());
    });
}

/// Runs `command` to its end in `worktree` as the holder's own command.
fn run_own_command(turn: &EOCheckedTurn, worktree: &FsPath, key: &str, command: &str) {
    let cwd = fs::canonicalize(worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in(key, command, Some(command), Some(&cwd))
        .expect("the start should record");
    recorder
        .command_completed_with_exit(
            key,
            command,
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the end should record");
}

#[test]
fn a_command_of_a_turn_no_prompt_owns_still_fences_the_carried_gate() {
    // A turn Claude Code starts by itself has no host prompt owner, but what
    // it runs in the worktree may write there all the same
    // (`claude_runtime_turns.rs`).
    let (turn, worktree) = ordinary_carried_turn("carried-unowned-turn");
    ordinary_launch_gate(&turn, &worktree, false);
    let token = turn.record(|record| {
        record
            .runtime
            .runtime_token()
            .expect("the begun turn should own the runtime")
    });
    let ownership = new_claude_turn_ownership();
    for frame in [
        json!({"type": "system", "subtype": "task_notification", "task_id": "b1",
            "status": "completed"}),
        json!({"type": "system", "subtype": "init"}),
        json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "on it"}]}}),
    ] {
        turn.state
            .observe_claude_frame_ownership(&turn.session_id, &token, &ownership, &frame);
    }
    assert!(turn.record(unmediated_claude_turn_hides_observations));
    run_own_command(&turn, &worktree, "unowned-build", "cargo build");
    let owner = lock_claude_turn_ownership(&ownership)
        .close_for_result(&json!({"type": "result", "is_error": false}));
    turn.state
        .finish_claude_result(&turn.session_id, &token, owner, None)
        .expect("the unowned turn's result is routed");
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(
        &turn,
        "this session ran a `cargo` command there",
    );
}

#[test]
fn a_read_only_line_of_another_runtime_still_refuses_the_gate() {
    // Only a Claude session's lines are read as Bash; another runtime's
    // line may run under PowerShell, where the same text can mean a write.
    let (turn, worktree) = ordinary_carried_turn("carried-codex-read-only");
    ordinary_launch_gate(&turn, &worktree, false);
    run_own_command(&turn, &worktree, "look", "git status");
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(&turn, "this session ran a `git` command there");
}

#[test]
fn a_read_only_command_in_the_worktree_preserves_artifact_custody_without_verification() {
    let (turn, worktree) = ordinary_carried_turn("carried-read-only");
    // A Claude session's lines are its Bash tool's.
    turn.record_mut(|record| record.session.agent = Agent::Claude);
    ordinary_launch_gate(&turn, &worktree, false);
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in("look", "git status", Some("git status"), Some(&cwd))
        .expect("the start should record");
    recorder
        .command_completed_with_exit(
            "look",
            "git status",
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the end should record");
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn a_command_of_the_holder_that_writes_in_the_worktree_refuses_the_gate() {
    let (turn, worktree) = ordinary_carried_turn("carried-own-write");
    ordinary_launch_gate(&turn, &worktree, false);
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    turn.recorder()
        .command_started_in("build", "cargo build", Some("cargo build"), Some(&cwd))
        .expect("the start should record");
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(&turn, "this session ran a `cargo` command there");
}

#[test]
fn an_edit_the_holder_reports_refuses_the_gate() {
    let (turn, worktree) = ordinary_carried_turn("carried-own-edit");
    ordinary_launch_gate(&turn, &worktree, false);
    turn.state.note_engram_workspace_edit(
        &turn.session_id,
        &EngramObservationProvenance::Ambient,
        None,
    );
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(&turn, "this session reported a file edit");
}

#[test]
fn an_edit_refusal_names_the_file_the_tool_reported() {
    // Every reported edit still fences the gate, wherever it landed; the
    // refusal says which file the tool reported, so the agent can tell which
    // edit cost the credit.
    let (turn, worktree) = ordinary_carried_turn("carried-edit-named");
    ordinary_launch_gate(&turn, &worktree, false);
    let reported = turn.root.join("target").join("probe.py");
    let reported = reported.to_string_lossy().into_owned();
    turn.recorder()
        .push_diff(
            &reported,
            "Create probe",
            "+print('probe')",
            ChangeType::Create,
        )
        .expect("the edit should record");
    finish_run(&worktree, "passed", &"f".repeat(64));
    let line = assert_carried_fence_custody(&turn, "this session reported a file edit");
    assert!(
        line.contains(&format!(
            "(tool-reported target: \"{}\")",
            engram_display_reported_target(&reported)
        )),
        "the refusal names the reported file: {line}"
    );
    assert!(line.contains("probe.py\")"), "the filename is kept: {line}");
    assert!(
        line.contains("tool-reported"),
        "the target is labelled as reported: {line}"
    );
}

/// Records each reported edit in order and returns the actual first fence.
/// A terminal artifact and ordinary closing do not grant verification.
fn edit_refusal_line(
    label: &str,
    targets: impl FnOnce(&FsPath) -> Vec<String>,
) -> (String, PathBuf) {
    let (turn, worktree) = ordinary_carried_turn(label);
    ordinary_launch_gate(&turn, &worktree, false);
    for target in targets(&worktree) {
        turn.recorder()
            .push_diff(&target, "Edit probe", "+print('probe')", ChangeType::Edit)
            .expect("the edit should record");
    }
    finish_run(&worktree, "passed", &"f".repeat(64));
    let line = assert_carried_fence_custody(&turn, "this session reported a file edit");
    (line, worktree)
}

#[test]
fn an_edit_refusal_shows_a_relative_target_as_the_tool_reported_it() {
    // A relative target is shown as given: TermAl does not guess where it
    // landed, so it is never joined to the worktree, normalised or
    // canonicalised: its `..` and `.` steps stay as the tool wrote them.
    let (line, worktree) = edit_refusal_line("carried-edit-relative", |_| {
        vec!["../outside/./src/probe.rs".to_owned()]
    });

    assert!(
        line.contains("(tool-reported target: \"../outside/./src/probe.rs\")"),
        "{line}"
    );
    for placed in [
        worktree
            .join("..")
            .join("outside")
            .join("src")
            .join("probe.rs"),
        worktree.join("../outside/./src/probe.rs"),
        worktree.parent().unwrap().join("outside"),
    ] {
        assert!(
            !line.contains(&*placed.to_string_lossy()),
            "no placement is inferred: {line}"
        );
    }
}

#[test]
fn an_edit_refusal_bounds_a_long_target() {
    let long = format!("{}.rs", "a".repeat(4096));
    let (line, _) = edit_refusal_line("carried-edit-long", |_| vec![long.clone()]);

    assert!(
        line.contains("tool-reported") && line.contains("aaaaaaaa"),
        "{line}"
    );
    assert!(!line.contains(&long), "the whole target is not rendered");
    assert!(
        line.contains("a…a") && line.contains("aaa.rs\")"),
        "the file name is kept: {line}"
    );
    assert!(
        line.len() < 1500,
        "the rendered target is bounded: {} bytes",
        line.len()
    );
}

#[test]
fn an_edit_refusal_escapes_control_characters_in_its_target() {
    let hostile = "evil\nname\r\u{1b}[31m\u{7}\u{0}\u{202e}\".rs".to_owned();
    let (line, _) = edit_refusal_line("carried-edit-control", |_| vec![hostile]);

    assert!(
        line.contains(r#"(tool-reported target: "evil\nname\r\u{1b}[31m\u{7}\u{0}\u{202e}\".rs")"#),
        "{line}"
    );
    for control in ['\n', '\r', '\u{1b}', '\u{7}', '\u{0}', '\u{202e}'] {
        assert!(
            !line.contains(control),
            "control {control:?} is escaped: {line:?}"
        );
    }
}

#[test]
fn an_edit_refusal_names_the_edit_that_first_fenced_the_gate() {
    // Later edits leave the first fence as it is; the refusal names the edit
    // that cost the credit, not a list of every file the turn touched.
    let (line, _) = edit_refusal_line("carried-edit-first", |worktree| {
        vec![
            worktree.join("first.py").to_string_lossy().into_owned(),
            worktree.join("second.py").to_string_lossy().into_owned(),
        ]
    });

    assert!(line.contains("first.py"), "{line}");
    assert!(
        !line.contains("second.py"),
        "a later edit does not replace the fence: {line}"
    );
}

#[test]
fn an_edit_with_no_known_target_keeps_the_generic_refusal() {
    let (turn, worktree) = ordinary_carried_turn("carried-edit-untargeted");
    ordinary_launch_gate(&turn, &worktree, false);
    turn.state.note_engram_workspace_edit(
        &turn.session_id,
        &EngramObservationProvenance::Ambient,
        None,
    );
    finish_run(&worktree, "passed", &"f".repeat(64));
    let line = assert_carried_fence_custody(&turn, "this session reported a file edit");
    assert_eq!(line, "this session reported a file edit");
    assert!(!line.contains("tool-reported"), "{line}");
}

#[test]
fn a_write_after_the_terminal_read_does_not_rewrite_the_carried_artifact() {
    // Once the host has read the run as terminal, a write can no longer reach
    // what it tested. The launch fact and artifact custody remain separate
    // from the source change measured when this ordinary turn closes.
    let (turn, worktree) = ordinary_carried_turn("carried-after-end");
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    run_own_command(&turn, &worktree, "commit", "git commit -m done");
    turn.state.note_engram_workspace_edit(
        &turn.session_id,
        &EngramObservationProvenance::Ambient,
        None,
    );
    assert!(turn.record(|record| record.engram.carried_checks[0].fence.is_none()));
    assert_carried_artifact_custody(&turn, &run, true);

    let (turn, worktree) = ordinary_carried_turn("carried-after-end-changed");
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    run_own_command(&turn, &worktree, "edit", "sed -i s/a/b/ README.md");
    fs::write(worktree.join("README.md"), "changed after the run\n").unwrap();
    let place = turn.record(|record| record.engram.observed_source_root().unwrap().place());
    let revision = engram_place_source_basis(&place).unwrap().source_revision;
    let digest = engram_terminal_record_digest(&run).unwrap();
    let observation = turn.finish();
    assert_ordinary_observation_has_no_native_credit(&turn, &observation);
    assert_eq!(observation["occurrence"]["source_change"]["sighting"]["source_basis"]["source_revision"], revision,
        "the real source write belongs to the ordinary turn: {observation:#}");
    assert!(engram_read_carried_run(&run, &digest).unwrap().passed);
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert_eq!(carried.run_directory.as_deref(), Some(run.as_path()));
        assert_eq!(carried.terminal_digest.as_deref(), Some(digest.as_str()));
        assert!(record.engram.carried_consumed_runs.is_empty());
    });
}

#[test]
fn a_write_through_termal_in_the_worktree_refuses_the_gate() {
    let (turn, worktree) = ordinary_carried_turn("carried-host-write");
    ordinary_launch_gate(&turn, &worktree, false);
    let written = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state.note_engram_host_write(&written);
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(&turn, "TermAl wrote");
}

#[test]
fn another_session_running_a_command_in_the_worktree_refuses_the_gate() {
    let (turn, worktree) = ordinary_carried_turn("carried-other-session");
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let other = create_test_project_session(&turn.state, Agent::Codex, &project_id, &worktree);
    ordinary_launch_gate(&turn, &worktree, false);
    let ran = "git checkout -- README.md";
    SessionRecorder::new(turn.state.clone(), other.clone())
        .command_started("other-edit", ran)
        .expect("the other session's command should record");
    finish_run(&worktree, "passed", &"f".repeat(64));
    // The fence cause names the other session and the command it placed in
    // this worktree, by program and line digest, never the line or a turn.
    let cause = assert_carried_fence_custody(&turn, "ran a `git` command there (line ");
    assert_eq!(
        cause,
        format!(
            "session Test ({other}) ran a `git` command there (line {})",
            &sha256_hex(ran.as_bytes())[..12]
        )
    );
    assert!(!cause.contains("was in a turn") && !cause.contains(ran), "{cause}");
}

/// The fence of the turn's one carried gate.
fn carried_fence(turn: &EOCheckedTurn) -> Option<String> {
    turn.record(|record| record.engram.carried_checks[0].fence.clone())
}

#[test]
fn another_claude_sessions_read_only_command_preserves_the_carried_artifact() {
    // Reading a worktree while its gate runs (a status, a log, a file read)
    // writes nothing: a Claude session's line is read as Bash, as the
    // holder's own is.
    let (turn, worktree) = ordinary_carried_turn("carried-other-reads");
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let reader = create_test_project_session(&turn.state, Agent::Claude, &project_id, &worktree);
    ordinary_launch_gate(&turn, &worktree, false);
    let mut recorder = SessionRecorder::new(turn.state.clone(), reader);
    for (key, line) in [
        ("status", "git status --short"),
        ("log", "git log --oneline -5"),
        ("read", "cat README.md"),
    ] {
        recorder
            .command_started(key, line)
            .expect("the reader's command should record");
        recorder
            .command_completed_with_exit(
                key,
                line,
                "",
                CommandStatus::Success,
                EngramCommandExit::ReportedSuccess,
            )
            .expect("the reader's command should end");
        assert_eq!(turn.record(|record| record.engram.carried_checks[0].fence.clone()), None, "{line}");
    }
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_artifact_custody(&turn, &run, true);

    // Another runtime's line may run under a shell that reads it otherwise,
    // so the same text from it still refuses the gate.
    let (turn, worktree) = ordinary_carried_turn("carried-other-reads-codex");
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let reader = create_test_project_session(&turn.state, Agent::Codex, &project_id, &worktree);
    ordinary_launch_gate(&turn, &worktree, false);
    SessionRecorder::new(turn.state.clone(), reader)
        .command_started("status", "git status --short")
        .expect("the reader's command should record");
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(&turn, "ran a `git` command there (line ");
}

#[test]
fn another_sessions_command_fences_only_where_it_may_write() {
    // Placed in another worktree, a writing command leaves the gate alone.
    let (turn, worktree) = ordinary_carried_turn("carried-other-elsewhere");
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let elsewhere = sibling_worktree(&turn, "carried-other-elsewhere-sibling");
    let writer = create_test_project_session(&turn.state, Agent::Claude, &project_id, &elsewhere);
    ordinary_launch_gate(&turn, &worktree, false);
    let mut recorder = SessionRecorder::new(turn.state.clone(), writer);
    recorder
        .command_started("write", "touch notes.txt")
        .expect("the writer's command should record");
    recorder
        .command_completed_with_exit(
            "write",
            "touch notes.txt",
            "",
            CommandStatus::Success,
            EngramCommandExit::ReportedSuccess,
        )
        .expect("the writer's command should end");
    assert_eq!(turn.record(|record| record.engram.carried_checks[0].fence.clone()), None);

    // One TermAl cannot place counts where its session works, which is not
    // here: it cannot fence this launch's artifact custody.
    recorder
        .command_started("lost", UNPLACED_WRITE)
        .expect("the writer's command should record");
    assert_eq!(turn.record(|record| record.engram.carried_checks[0].fence.clone()), None);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_artifact_custody(&turn, &run, true);
}

/// A writing line TermAl cannot place: its `cd` names no literal directory.
const UNPLACED_WRITE: &str = "cd \"$SOMEWHERE\" && touch notes.txt";

#[test]
fn an_unplaced_command_refuses_a_carried_gate_where_its_session_works() {
    // In the session's own workdir.
    let (turn, worktree) = ordinary_carried_turn("carried-unplaced-workdir");
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let writer = create_test_project_session(&turn.state, Agent::Claude, &project_id, &worktree);
    ordinary_launch_gate(&turn, &worktree, false);
    SessionRecorder::new(turn.state.clone(), writer)
        .command_started("lost", UNPLACED_WRITE)
        .expect("the writer's command should record");
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(
        &turn,
        "ran a `cd` command that TermAl could not place and that may have run there (line ",
    );

    // In the named source root its turn works in, whatever its workdir.
    let (turn, worktree) = ordinary_carried_turn("carried-unplaced-named-root");
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let elsewhere = sibling_worktree(&turn, "carried-unplaced-named-root-sibling");
    let writer = create_test_project_session(&turn.state, Agent::Claude, &project_id, &elsewhere);
    ordinary_launch_gate(&turn, &worktree, false);
    let mut recorder = SessionRecorder::new(turn.state.clone(), writer.clone());
    recorder
        .command_started("lost", UNPLACED_WRITE)
        .expect("the writer's command should record");
    assert_eq!(turn.record(|record| record.engram.carried_checks[0].fence.clone()), None,
        "it works elsewhere so far");
    // This helper selects the other session's current named-root provenance;
    // it is a unit fixture, not a second provider dispatch or naming ACK.
    name_other_session_source_root(&turn, &writer, &worktree);
    recorder
        .command_started("lost-again", UNPLACED_WRITE)
        .expect("the writer's command should record");
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(
        &turn,
        "ran a `cd` command that TermAl could not place and that may have run there (line ",
    );
}

#[test]
fn a_command_whose_start_was_never_reported_fences_where_its_session_writes() {
    // A runtime that reports only a command's end gives no line to read and
    // no place but the session's own.
    let (turn, worktree) = ordinary_carried_turn("carried-other-end-only");
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let other = create_test_project_session(&turn.state, Agent::Claude, &project_id, &worktree);
    ordinary_launch_gate(&turn, &worktree, false);
    SessionRecorder::new(turn.state.clone(), other)
        .command_completed_with_exit(
            "unannounced",
            "git status",
            "",
            CommandStatus::Success,
            EngramCommandExit::ReportedSuccess,
        )
        .expect("the end should record");
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(
        &turn,
        "ran a command there whose start TermAl was not told",
    );
}

#[test]
fn a_terminal_record_changed_after_the_first_terminal_read_refuses_the_gate() {
    let (turn, worktree) = ordinary_carried_turn("carried-rewritten");
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let mut record: Value =
        serde_json::from_slice(&fs::read(run.join("results.json")).unwrap()).unwrap();
    record["note"] = json!("rewritten");
    fs::write(
        run.join("results.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    assert_carried_artifact_rejected(&turn, &run,
        "its terminal record changed after the host first read it as terminal",
    );
}

#[test]
fn an_unreported_source_change_is_measured_separately_from_a_carried_artifact() {
    let (turn, worktree) = ordinary_carried_turn("carried-moved");
    let place = turn.record(|record| record.engram.observed_source_root().unwrap().place());
    let before = engram_place_source_basis(&place).unwrap().source_revision;
    ordinary_launch_gate(&turn, &worktree, false);
    fs::write(worktree.join("README.md"), "changed behind the host\n").unwrap();
    let after = engram_place_source_basis(&place).unwrap().source_revision;
    assert_ne!(before, after);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let digest = engram_terminal_record_digest(&run).unwrap();
    assert!(engram_read_carried_run(&run, &digest).unwrap().passed,
        "an independently valid artifact does not prove the current source association");
    turn.state.poll_engram_carried_runs();
    let observation = turn.finish();
    assert_eq!(observation["occurrence"]["source_change"]["sighting"]["source_basis"]["source_revision"], after);
    let checks = observation["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["observed_result"], "indeterminate");
    assert!(checks[0].get("source_basis").is_none());
    assert_ordinary_observation_has_no_native_credit(&turn, &observation);
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks[0].terminal_digest.as_deref(), Some(digest.as_str()));
        assert!(record.engram.carried_consumed_runs.is_empty());
    });
}

#[test]
fn an_input_fingerprint_that_changed_during_the_run_refuses_the_gate() {
    let (turn, worktree) = ordinary_carried_turn("carried-drifted");
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"e".repeat(64));
    assert_carried_artifact_rejected(&turn, &run,
        "its input fingerprint after the run differs from the one before",
    );
}

#[test]
fn a_stopped_carried_artifact_is_neither_a_pass_nor_a_failure() {
    let (turn, worktree) = ordinary_carried_turn("carried-stopped");
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "failed", &"f".repeat(64));
    let mut record: Value =
        serde_json::from_slice(&fs::read(run.join("results.json")).unwrap()).unwrap();
    record["state"] = json!("stopped");
    fs::write(
        run.join("results.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    assert_carried_artifact_rejected(&turn, &run, ENGRAM_CARRIED_RUN_NEITHER);
}

#[test]
fn a_dead_launcher_cannot_supply_a_terminal_result_or_native_verification() {
    let (turn, worktree) = ordinary_carried_turn("carried-launcher-gone");
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let (_exited, dead_pid) = exited_process();
    fs::write(
        run.join("results.json"),
        serde_json::to_vec(&json!({
            "runId": "test-carried",
            "state": "running",
            "pid": dead_pid,
        }))
        .unwrap(),
    )
    .unwrap();
    turn.state.poll_engram_carried_runs();
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert_eq!(carried.run_directory.as_deref(), Some(run.as_path()));
        assert!(carried.launcher_gone && carried.terminal_digest.is_none());
        assert!(!engram_carried_check_polls(carried, chrono::Utc::now()));
    });
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        assert!(record.engram.carried_consumed_runs.is_empty());
    });
}

#[test]
fn renaming_the_current_root_does_not_relabel_a_prior_carried_launch() {
    let (turn, worktree) = ordinary_carried_turn("carried-renamed");
    // A successor root must lie inside the project, as the named one does.
    let next = turn.root.join(".worktrees").join("renamed-current");
    run_git_test_command(&turn.root, &[
        "worktree", "add", "--quiet", "-b", "renamed-current", next.to_str().unwrap(),
    ]);
    ordinary_launch_gate(&turn, &worktree, false);
    let original = turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        (carried.check.host_turn_ref.clone(), carried.root_generation, carried.check.target.root.clone())
    });
    let work = turn.record(|record| record.engram.work_binding.as_ref().unwrap().work_id.clone());
    let named = turn.state.name_engram_source_root(&turn.session_id, EngramSourceRootRequest {
        work, path: Some(Some(next.to_string_lossy().into_owned())),
    }).expect("the actual work service may name a successor root");
    assert!(named.generation > original.1);
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert_eq!((&carried.check.host_turn_ref, carried.root_generation, &carried.check.target.root),
            (&original.0, original.1, &original.2));
        assert!(record.engram.carried_consumed_runs.is_empty());
    });
    assert_ordinary_turn_has_no_native_requests(&turn);
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn a_host_restart_tells_the_holder_its_carried_gate_lost_its_credit() {
    let (turn, worktree) = ordinary_carried_turn("carried-restart");
    ordinary_launch_gate(&turn, &worktree, false);
    let persisted = turn.record(|record| {
        serde_json::to_value(PersistedSessionRecord::from_record(record))
            .expect("the record serializes")
    });
    assert_eq!(
        persisted["engramCarriedLaunches"].as_array().map(Vec::len),
        Some(1),
        "{persisted:#}"
    );
    let loaded: PersistedSessionRecord =
        serde_json::from_value(persisted).expect("the record deserializes");
    let record = loaded.into_record().expect("the record loads");

    assert!(record.engram.carried_checks.is_empty());
    let line = record
        .engram
        .pending_source_root_line
        .expect("the holder should be told");
    assert!(
        line.contains("the host restarted while the background full gate")
            && line.contains("earned no credit"),
        "{line}"
    );
}

/// Starts `command` in `worktree` and ends it `exit`, as the holder's
/// launch, without asserting what became of it.
fn launch_command(turn: &EOCheckedTurn, worktree: &FsPath, command: &str, exit: EngramCommandExit) {
    let cwd = fs::canonicalize(worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in("gate", command, Some(command), Some(&cwd))
        .expect("the launch should record");
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit("gate", command, "", CommandStatus::Success, exit)
        .expect("the launch result should record");
}

/// Asserts no check of the launch is kept or carried, and the holder was
/// told `why` it earned no credit.
fn assert_dropped_at_launch(turn: &EOCheckedTurn, why: &str) {
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    assert!(
        line.contains(ENGRAM_CHECK_CREDIT_LINE_PREFIX) && line.contains(why),
        "{line}"
    );
}

#[test]
fn a_detached_launch_that_failed_is_dropped_and_never_recorded() {
    let (turn, worktree) = ordinary_carried_turn("carried-failed-launch");
    launch_command(&turn, &worktree, DETACHED_GATE, EngramCommandExit::Code(1));
    assert_dropped_at_launch(&turn, "its detached launch failed, so no run started");
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn a_launch_on_a_line_that_runs_more_than_the_gate_is_dropped() {
    let (turn, worktree) = ordinary_carried_turn("carried-compound");
    launch_command(
        &turn,
        &worktree,
        "node scripts/test-launcher.mjs full --detach --notify session-9 && echo x",
        EngramCommandExit::Code(0),
    );
    assert_dropped_at_launch(&turn, "its line runs more than the gate");
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn a_launch_outside_a_named_root_or_beside_another_command_is_not_carried() {
    // No named root for the turn.
    let turn = EOCheckedTurn::start("carried-unnamed", true);
    launch_command(
        &turn,
        &turn.root.clone(),
        GATE,
        EngramCommandExit::NotFinished,
    );
    assert_dropped_at_launch(&turn, "not measured in a named source root");
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());

    // A named root, but the gate ran in the repository's main worktree.
    let (turn, _worktree) = ordinary_carried_turn("carried-elsewhere");
    launch_command(
        &turn,
        &turn.root.clone(),
        GATE,
        EngramCommandExit::NotFinished,
    );
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());

    // Another command running in its worktree as it launched.
    let (turn, worktree) = ordinary_carried_turn("carried-beside");
    let cwd = fs::canonicalize(&worktree).unwrap().to_string_lossy().into_owned();
    turn.recorder().command_started_in("other-writing-command", "touch notes.txt",
        Some("touch notes.txt"), Some(&cwd)).unwrap();
    launch_command(&turn, &worktree, GATE, EngramCommandExit::NotFinished);
    assert_dropped_at_launch(&turn, "another command or another writable session");
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn a_command_later_described_as_running_in_the_worktree_refuses_the_gate() {
    // An ACP runtime may give a command's directory only in a later update.
    let (turn, worktree) = ordinary_carried_turn("carried-described");
    ordinary_launch_gate(&turn, &worktree, false);
    let mut recorder = turn.recorder();
    recorder
        .command_started_in(
            "late",
            "write-things",
            Some("write-things"),
            Some(&turn.root.to_string_lossy()),
        )
        .expect("the start should record");
    assert!(turn.record(|record| record.engram.carried_checks[0].fence.is_none()));
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    recorder
        .command_described("late", None, Some(&cwd))
        .expect("the description should record");
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(&turn, "whose line TermAl was not told");
}

#[test]
fn a_file_change_the_watcher_saw_in_the_worktree_refuses_the_gate() {
    let (turn, worktree) = ordinary_carried_turn("carried-watched");
    ordinary_launch_gate(&turn, &worktree, false);
    let change = |path: PathBuf| WorkspaceFileChangeEvent {
        path: path.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Modified,
        root_path: None,
        session_id: None,
        mtime_ms: None,
        size_bytes: None,
    };
    // A change elsewhere leaves it alone.
    turn.state
        .note_engram_workspace_file_changes(&[change(turn.root.join("README.md"))]);
    assert!(turn.record(|record| record.engram.carried_checks[0].fence.is_none()));
    let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state
        .note_engram_workspace_file_changes(&[change(changed.clone())]);
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(&turn, "a file in its worktree changed");

    // The watcher reports late, so a change it reports after the run was
    // read as ended still refuses it: it may have landed while it ran.
    let (turn, worktree) = ordinary_carried_turn("carried-watched-late");
    ordinary_launch_gate(&turn, &worktree, false);
    finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state
        .note_engram_workspace_file_changes(&[change(changed)]);
    assert_carried_fence_custody(&turn, "a file in its worktree changed");
}

#[test]
fn a_gate_launched_in_the_one_call_form_keeps_artifact_custody_without_verification() {
    // The form the instructions give a Claude session, whose runtime reports
    // no directory: the line itself names where the gate runs.
    let (turn, worktree) = ordinary_carried_turn("carried-one-call");
    turn.record_mut(|record| record.session.agent = Agent::Claude);
    let root = engram_source_root_display(
        &fs::canonicalize(&worktree)
            .expect("the worktree canonicalizes")
            .to_string_lossy(),
    );
    assert!(
        engram_one_call_reads(&root),
        "a plain test directory: {root}"
    );
    let line = format!("pushd \"{root}\" && node scripts/test-launcher.mjs full");
    let mut recorder = turn.recorder();
    recorder
        .command_started("gate", &line)
        .expect("the launch should record");
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            "gate",
            &line,
            "",
            CommandStatus::Success,
            EngramCommandExit::NotFinished,
        )
        .expect("the launch result should record");
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);
    let start_basis =
        turn.record(|record| record.engram.carried_checks[0].check.start_basis.clone());
    start_basis
        .wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
        .expect("the launch snapshot was taken in time");
    // Reading its summary from the same form leaves it unfenced.
    let summary =
        format!("pushd \"{root}\" && node scripts/test-launcher.mjs summary test-carried");
    let mut recorder = turn.recorder();
    recorder.command_started("look", &summary).unwrap();
    recorder
        .command_completed_with_exit(
            "look",
            &summary,
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn another_session_starting_a_turn_leaves_a_carried_gate_alone_until_it_writes() {
    // A turn start says nothing was written: the gate is fenced only when the
    // session reports what may write, a file edit here, and then only in the
    // worktree that session works in.
    for same_worktree in [true, false] {
        let (turn, worktree) = ordinary_carried_turn("carried-turn-start");
        ordinary_launch_gate(&turn, &worktree, false);
        let other = test_session_id(&turn.state, Agent::Codex);
        let other_workdir = if same_worktree {
            fs::canonicalize(&worktree).unwrap()
        } else {
            sibling_worktree(&turn, "carried-turn-start-elsewhere")
        };
        {
            let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&other).expect("other session");
            inner.sessions[index].session.workdir = other_workdir.to_string_lossy().into_owned();
        }
        let dispatched = turn
            .state
            .dispatch_turn(
                &other,
                SendMessageRequest {
                    text: "Edit the README.".to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .expect("the other session should start a turn");
        assert!(matches!(dispatched, DispatchTurnResult::Dispatched(_)));
        assert_eq!(
            turn.record(|record| record.engram.carried_checks[0].fence.clone()),
            None,
            "a turn start alone fences nothing: same_worktree={same_worktree}"
        );

        turn.state
            .note_engram_workspace_edit(&other, &EngramObservationProvenance::Ambient, None);
        let fence = turn.record(|record| record.engram.carried_checks[0].fence.clone());
        assert_eq!(fence.is_some(), same_worktree, "{fence:?}");
        if let Some(fence) = fence {
            assert!(fence.ends_with("reported a file edit there"), "{fence}");
        }
        assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
        turn.record(|record| assert!(record.engram.carried_consumed_runs.is_empty()));
    }
}

/// The Claude holder's launch of the full gate in `worktree` in the one-call
/// form its instructions give, its result marking only the launch: the form
/// `run_in_background` takes.
fn launch_gate_in_the_background_one_call(turn: &EOCheckedTurn, worktree: &FsPath) -> String {
    let root = engram_source_root_display(
        &fs::canonicalize(worktree)
            .expect("the worktree canonicalizes")
            .to_string_lossy(),
    );
    let line = format!("pushd \"{root}\" && node scripts/test-launcher.mjs full");
    let mut recorder = turn.recorder();
    recorder
        .command_started("gate", &line)
        .expect("the launch should record");
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            "gate",
            &line,
            "",
            CommandStatus::Success,
            EngramCommandExit::NotFinished,
        )
        .expect("the launch result should record");
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);
    let start_basis =
        turn.record(|record| record.engram.carried_checks[0].check.start_basis.clone());
    start_basis
        .wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
        .expect("the launch snapshot was taken in time");
    root
}

/// Where TermAl presumes the turn's shell is, and whether it has lost it.
fn shell_place(turn: &EOCheckedTurn) -> (Option<String>, bool) {
    turn.record(|record| {
        let shell = record
            .engram
            .shell_directory
            .as_ref()
            .expect("a command was reported");
        (
            shell
                .directory
                .as_ref()
                .map(|directory| engram_path_key(FsPath::new(directory))),
            shell.lost_unbounded || !shell.lost_among.is_empty(),
        )
    })
}

#[test]
fn a_background_launch_leaves_a_claude_sessions_shell_where_it_was() {
    // Claude's Bash tool runs a background call in a shell of its own: the
    // `pushd` of the launch moves nothing, so the holder's later command runs
    // where its shell already was, not in the gate's worktree.
    let (turn, worktree) = ordinary_carried_turn("carried-background-shell");
    turn.record_mut(|record| record.session.agent = Agent::Claude);
    let workdir = turn.record(|record| engram_path_key(FsPath::new(&record.session.workdir)));
    let root = launch_gate_in_the_background_one_call(&turn, &worktree);
    assert_eq!(shell_place(&turn), (Some(workdir.clone()), false));

    let mut recorder = turn.recorder();
    recorder
        .command_started("write-elsewhere", "touch notes.txt")
        .expect("the command should record");
    recorder
        .command_completed_with_exit(
            "write-elsewhere",
            "touch notes.txt",
            "",
            CommandStatus::Success,
            EngramCommandExit::ReportedSuccess,
        )
        .expect("the command should end");
    assert_eq!(carried_fence(&turn), None);
    assert_eq!(shell_place(&turn), (Some(workdir), false));

    // The same command sent into the gate's worktree still refuses it.
    let into_the_gate = format!("pushd \"{root}\" && touch notes.txt");
    turn.recorder()
        .command_started("write-there", &into_the_gate)
        .expect("the command should record");
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(
        &turn,
        "this session ran a `touch` command there",
    );
}

#[test]
fn only_a_claude_background_call_keeps_the_shell_a_pushd_would_move() {
    // A foreground `pushd "DIR" && COMMAND` that succeeds moves the shell.
    let (turn, worktree) = ordinary_carried_turn("carried-foreground-shell");
    turn.record_mut(|record| record.session.agent = Agent::Claude);
    let root = engram_source_root_display(
        &fs::canonicalize(&worktree)
            .expect("the worktree canonicalizes")
            .to_string_lossy(),
    );
    let line = format!("pushd \"{root}\" && git status");
    let mut recorder = turn.recorder();
    recorder
        .command_started("look", &line)
        .expect("the command should record");
    recorder
        .command_completed_with_exit(
            "look",
            &line,
            "",
            CommandStatus::Success,
            EngramCommandExit::ReportedSuccess,
        )
        .expect("the command should end");
    assert_eq!(
        shell_place(&turn),
        (Some(engram_path_key(FsPath::new(&root))), false)
    );
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());

    // Another runtime's unfinished command says nothing of the shell it ran
    // in: TermAl loses the shell between where it was and where the `pushd`
    // leads, as before.
    let (turn, worktree) = ordinary_carried_turn("carried-unfinished-shell");
    turn.record_mut(|record| record.session.agent = Agent::Cursor);
    let root = engram_source_root_display(
        &fs::canonicalize(&worktree)
            .expect("the worktree canonicalizes")
            .to_string_lossy(),
    );
    let line = format!("pushd \"{root}\" && npm run watch");
    let mut recorder = turn.recorder();
    recorder
        .command_started("watch", &line)
        .expect("the command should record");
    recorder
        .command_completed_with_exit(
            "watch",
            &line,
            "",
            CommandStatus::Success,
            EngramCommandExit::NotFinished,
        )
        .expect("the command should record its launch");
    assert_eq!(shell_place(&turn), (None, true));
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn a_launch_whose_run_never_appeared_stops_searching_without_inventing_a_result() {
    let (turn, worktree) = ordinary_carried_turn("carried-missing");
    ordinary_launch_gate(&turn, &worktree, false);
    let long_ago = (chrono::Utc::now() - chrono::Duration::minutes(11))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    turn.record_mut(|record| record.engram.carried_checks[0].check.started_at = long_ago);
    turn.state.poll_engram_carried_runs();
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(carried.run_missing);
        assert!(carried.run_directory.is_none() && carried.terminal_digest.is_none());
        assert!(!engram_carried_check_polls(carried, chrono::Utc::now()));
    });
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    turn.record(|record| assert!(record.engram.carried_consumed_runs.is_empty()));
}

#[test]
fn a_run_that_never_ends_stops_polling_after_six_hours_without_verification() {
    let (turn, worktree) = ordinary_carried_turn("carried-expired");
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    fs::write(
        run.join("results.json"),
        serde_json::to_vec(&json!({ "runId": "test-carried", "state": "running" })).unwrap(),
    )
    .unwrap();
    let long_ago = (chrono::Utc::now() - chrono::Duration::hours(7))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    turn.state.poll_engram_carried_runs();
    turn.record_mut(|record| record.engram.carried_checks[0].check.started_at = long_ago);
    turn.state.poll_engram_carried_runs();
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(engram_carried_check_expired(carried, chrono::Utc::now()));
        assert!(!engram_carried_check_polls(carried, chrono::Utc::now()));
        assert_eq!(carried.run_directory.as_deref(), Some(run.as_path()));
        assert!(carried.terminal_digest.is_none());
    });
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    turn.record(|record| assert!(record.engram.carried_consumed_runs.is_empty()));
}

#[test]
fn a_fifth_launch_drops_the_oldest_carried_gate() {
    let (turn, worktree) = ordinary_carried_turn("carried-limit");
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    for index in 0..=ENGRAM_CARRIED_CHECK_LIMIT {
        let key = format!("gate-{index}");
        let mut recorder = turn.recorder();
        // Detached launches, whose results end their commands: a background
        // launch's command stays running and would overlap the next one.
        recorder
            .command_started_in(&key, DETACHED_GATE, Some(DETACHED_GATE), Some(&cwd))
            .expect("the launch should record");
        turn.wait_for_snapshots();
        recorder
            .command_completed_with_exit(
                &key,
                DETACHED_GATE,
                "",
                CommandStatus::Success,
                EngramCommandExit::Code(0),
            )
            .expect("the launch result should record");
    }
    let keys = turn.record(|record| {
        record
            .engram
            .carried_checks
            .iter()
            .map(|carried| carried.check.key.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(keys, ["gate-1", "gate-2", "gate-3", "gate-4"]);
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder of the dropped gate is told");
    assert!(
        line.contains("a newer background gate took its place"),
        "{line}"
    );
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn an_ordinary_successor_turn_does_not_take_a_predecessors_carried_artifact() {
    let (turn, worktree) = ordinary_carried_turn("carried-next-ordinary-turn");
    ordinary_launch_gate(&turn, &worktree, false);
    let first = turn.finish();
    assert_ordinary_observation_has_no_native_credit(&turn, &first);
    let original = first["occurrence"]["host_turn_ref"].as_str().unwrap().to_owned();
    let guard = phase_sync::PollGuard::new();
    loop {
        if turn.record(|record| record.engram.unadmitted_active_turn.is_none()
            && !record.engram.unadmitted_worker_running) { break; }
        guard.wait("the actual predecessor observation completes before another ordinary turn");
    }
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let digest = engram_terminal_record_digest(&run).unwrap();
    turn.state.poll_engram_carried_runs();
    turn.begin("Read the gate's ordinary summary.");
    let next = turn.record(|record| record.engram.unadmitted_active_turn.as_ref().unwrap().host_turn_ref.clone());
    assert_ne!(next, original, "the actual provider dispatch captures a fresh trace owner");
    let second = turn.finish();
    assert_eq!(second["occurrence"]["host_turn_ref"], next);
    assert!(second["occurrence"]["observed_checks"].as_array().unwrap().is_empty(),
        "a successor cannot claim the predecessor's launch or artifact as its own check");
    assert_ordinary_observation_has_no_native_credit(&turn, &second);
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        let carried = &record.engram.carried_checks[0];
        assert_eq!(carried.check.host_turn_ref, original);
        assert_eq!(carried.run_directory.as_deref(), Some(run.as_path()));
        assert_eq!(carried.terminal_digest.as_deref(), Some(digest.as_str()));
        assert!(record.engram.carried_consumed_runs.is_empty());
    });
}

#[test]
fn a_run_recovered_as_failed_and_interrupted_is_neither_credited_nor_recorded() {
    // The exact shape both launchers' `recover` writes.
    let temp = TestTempRoot::create("termal-engram-carried-recovered");
    let root = temp.path().join("root");
    let fingerprint = "f".repeat(64);
    let directory = write_run(
        temp.path(),
        "test-recovered",
        &root,
        "2026-09-29T00:00:00.000Z",
        "failed",
        &fingerprint,
    );
    let mut record: Value =
        serde_json::from_slice(&fs::read(directory.join("results.json")).unwrap()).unwrap();
    record["exitCode"] = json!(1);
    record["interrupted"] = json!(true);
    record["error"] = json!("interrupted: the launcher died before the run ended");
    record["stages"][3] = json!({
        "name": "rust-tests",
        "state": "failed",
        "error": "interrupted: the launcher died before the run ended",
        "outcome": "unknown",
    });
    record["recovered"] = json!({ "by": "recover" });
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let digest = engram_terminal_record_digest(&directory).expect("a recovered record is terminal");

    assert_eq!(
        engram_read_carried_run(&directory, &digest),
        Err(ENGRAM_CARRIED_RUN_NEITHER)
    );
}

#[test]
fn check_credit_lines_read_cleanly_and_merge_without_evicting_bind_lines() {
    let check = engram_check_command(GATE).expect("a recognised test");
    let lines = [
        engram_dropped_launch_line(&check, "its detached launch failed, so no run started"),
        engram_carried_lost_to_restart_line(&EngramCarriedLaunchMarker {
            root: "C:/repo".to_owned(),
            launched_at: "2026-09-29T00:00:00.000Z".to_owned(),
            fingerprint: "a".repeat(64),
            unknown_line: None,
            bind_members: Vec::new(),
        }),
        ENGRAM_CARRIED_RUN_NEITHER.to_owned(),
    ];
    for line in &lines {
        assert!(!line.contains("  "), "no run of spaces: {line:?}");
    }
    let mut engram = EngramSessionState::default();
    engram.set_pending_source_root_line("[TermAl] bind line".to_owned());
    for index in 0..6 {
        engram.set_pending_source_root_line(format!(
            "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} gate {index} earned no credit."
        ));
    }
    let pending = engram
        .pending_source_root_line
        .clone()
        .expect("lines are pending");
    let pending = pending.lines().collect::<Vec<_>>();
    assert_eq!(pending.len(), 2, "{pending:?}");
    assert_eq!(pending[0], "[TermAl] bind line");
    assert!(
        pending[1].contains("gate 0") && pending[1].contains("gate 5"),
        "{}",
        pending[1]
    );

    // A line a prompt in flight carries is not merged into: its delivery
    // takes it out when the prompt is accepted, and the new line waits.
    let carried_line = pending[1].to_owned();
    engram.source_root_line_delivery = Some((carried_line.clone(), 1));
    engram.set_pending_source_root_line(format!(
        "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} gate 6 earned no credit."
    ));
    let pending = engram.pending_source_root_line.clone().unwrap();
    let pending = pending.lines().collect::<Vec<_>>();
    assert!(pending.contains(&carried_line.as_str()), "{pending:?}");
    assert!(
        pending
            .iter()
            .any(|line| line.contains("gate 6") && !line.contains("gate 5")),
        "{pending:?}"
    );

    // Past the bound, the newest is kept and the front is cut.
    let mut merged = format!("{ENGRAM_CHECK_CREDIT_LINE_PREFIX} gate first earned no credit.");
    for index in 0..60 {
        merged = engram_merge_credit_lines(
            &merged,
            &format!(
                "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} gate {index} earned no credit: why {index}."
            ),
        );
        assert!(
            merged.len() <= ENGRAM_CHECK_CREDIT_LINE_MAX_BYTES,
            "{}",
            merged.len()
        );
        assert!(
            merged.starts_with(ENGRAM_CHECK_CREDIT_LINE_PREFIX),
            "{merged}"
        );
    }
    assert!(
        merged.ends_with("gate 59 earned no credit: why 59."),
        "{merged}"
    );
    assert!(!merged.contains("gate first"), "{merged}");
    assert!(merged.contains("earlier lines cut"), "{merged}");
}

#[test]
fn two_terminal_reads_that_disagree_keep_the_first_digest_and_mark_the_conflict() {
    // Polls read off the lock: a later terminal read of another record,
    // stored after the first, marks the conflict rather than being ignored.
    let (turn, worktree) = ordinary_carried_turn("carried-conflict");
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let digest = engram_terminal_record_digest(&run).expect("a terminal record");
    turn.record_mut(|record| {
        let carried = &mut record.engram.carried_checks[0];
        engram_note_carried_run_read(
            carried,
            Some(run.clone()),
            Some("0".repeat(64)),
            false,
            false,
            Vec::new(),
        );
        engram_note_carried_run_read(
            carried,
            Some(run.clone()),
            Some(digest.clone()),
            false,
            false,
            Vec::new(),
        );
        assert!(carried.terminal_conflict);
        assert_eq!(carried.terminal_digest.as_deref(), Some("0".repeat(64).as_str()));
    });
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(carried.terminal_conflict);
        assert_eq!(carried.terminal_digest.as_deref(), Some("0".repeat(64).as_str()));
        assert_eq!(carried.run_directory.as_deref(), Some(run.as_path()));
        assert!(record.engram.carried_consumed_runs.is_empty());
    });
}

#[test]
fn a_turn_a_stop_drains_from_the_queue_fences_a_carried_gate_of_its_own_worktree_only() {
    // The stop starts the queued turn itself, for a session whose worktree
    // TermAl has not resolved (it has reported nothing since TermAl
    // started): it is resolved before that turn starts, rather than taken
    // to be every worktree.
    let (turn, worktree) = ordinary_carried_turn("carried-stop-drain");
    ordinary_launch_gate(&turn, &worktree, false);
    let other = test_session_id(&turn.state, Agent::Cursor);
    let runtime = |name: &str| {
        let owner = phase_sync::ParkedProcess::spawn();
        let (input_tx, input_rx) = mpsc::channel();
        let handle = AcpRuntimeHandle {
            agent: AcpAgent::Cursor,
            runtime_id: format!("cursor-carried-{name}"),
            input_tx,
            #[cfg(windows)]
            process_tree: None,
            process: owner.process.clone(),
            turn_lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
        };
        (owner, input_rx, handle)
    };
    let (_original_owner, _original_rx, original) = runtime("original");
    let (_successor_owner, _successor_rx, successor) = runtime("successor");
    turn.state
        .install_test_acp_runtime_override(AcpAgent::Cursor, successor);
    let elsewhere = sibling_worktree(&turn, "carried-stop-drain-elsewhere");
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("other session");
        let record = &mut inner.sessions[index];
        record.session.workdir = elsewhere.to_string_lossy().into_owned();
        record.engram.workdir_worktree = None;
        record.runtime = SessionRuntime::Acp(original);
        record.session.status = SessionStatus::Active;
        record.queued_prompts.push_back(QueuedPromptRecord {
            promoted_message_index: None,
            promotion_disposition_known: true,
            engram_ordinary_handoff: false,
            engram_waiting: false,
            engram_bind: None,
            engram_evaluate: None,
            engram_interrupted: false,
            source: QueuedPromptSource::User,
            attachments: Vec::new(),
            pending_prompt: PendingPrompt {
                attachments: Vec::new(),
                id: "queued-carried-stop-drain".to_owned(),
                engram_interrupted: false,
                is_engram_retained: false,
                timestamp: stamp_now(),
                text: "Edit the README.".to_owned(),
                expanded_text: None,
                source: None,
            },
        });
        sync_pending_prompts(record);
    }
    turn.state
        .stop_session_with_options(
            &other,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: true,
                pause_automatic_resumes_on_success: false,
                orchestrator_stop_instance_id: None,
            },
        )
        .expect("the stop should start the queued turn");
    let (drained, resolved) = {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("other session");
        (
            inner.sessions[index].queued_prompts.is_empty(),
            engram_session_worktree(&inner.sessions[index]),
        )
    };
    assert!(drained, "the stop started the queued turn");
    assert_eq!(resolved, Some(engram_worktree_root(&elsewhere)));

    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0].fence.clone()),
        None
    );
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn a_conflict_recorded_after_a_carried_copy_preserves_the_original_terminal_custody() {
    let (turn, worktree) = ordinary_carried_turn("carried-late-conflict");
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let copy = turn.record(|record| record.engram.carried_checks[0].clone());
    let digest = copy.terminal_digest.clone().unwrap();
    turn.record_mut(|record| {
        engram_note_carried_run_read(&mut record.engram.carried_checks[0], Some(run.clone()),
            Some("0".repeat(64)), false, false, Vec::new());
    });
    assert!(!copy.terminal_conflict, "a captured value does not share later mutation");
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    turn.record(|record| {
        let live = &record.engram.carried_checks[0];
        assert!(live.terminal_conflict);
        assert_eq!(live.terminal_digest.as_deref(), Some(digest.as_str()));
        assert_eq!(live.check.host_turn_ref, copy.check.host_turn_ref);
        assert_eq!(live.check.sequence, copy.check.sequence);
        assert!(record.engram.carried_consumed_runs.is_empty());
    });
}

#[test]
fn a_fenced_gate_claims_no_run_and_is_no_longer_polled() {
    // A fenced launch is not read again and finds no run.
    let (turn, worktree) = ordinary_carried_turn("carried-fenced-claim");
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    for key in ["first", "second"] {
        let mut recorder = turn.recorder();
        recorder
            .command_started_in(key, DETACHED_GATE, Some(DETACHED_GATE), Some(&cwd))
            .expect("the launch should record");
        turn.wait_for_snapshots();
        recorder
            .command_completed_with_exit(
                key,
                DETACHED_GATE,
                "",
                CommandStatus::Success,
                EngramCommandExit::Code(0),
            )
            .expect("the launch result should record");
    }
    turn.record_mut(|record| {
        assert_eq!(record.engram.carried_checks.len(), 2);
        record.engram.carried_checks[0].fence = Some("a test fence".to_owned());
    });
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();

    let directories = turn.record(|record| {
        record
            .engram
            .carried_checks
            .iter()
            .map(|carried| (carried.check.key.clone(), carried.run_directory.clone()))
            .collect::<Vec<_>>()
    });
    // The fenced first finds no run; the second, launched while the first
    // had none matched, is ambiguous and takes none either.
    let _ = run;
    assert_eq!(
        directories,
        [("first".to_owned(), None), ("second".to_owned(), None)]
    );
    let now = chrono::Utc::now();
    turn.record(|record| {
        let carried = &record.engram.carried_checks;
        assert!(!engram_carried_check_polls(&carried[0], now), "fenced");
        assert!(carried[1].ambiguous);
        assert!(!engram_carried_check_polls(&carried[1], now), "ambiguous");
    });
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    turn.record(|record| assert!(record.engram.carried_consumed_runs.is_empty()));
}

#[test]
fn a_failed_gate_launched_in_the_one_call_form_keeps_its_exit() {
    // Its stages' own lines say the exit is the gate's, not its `pushd`'s.
    let (turn, worktree) = ordinary_carried_turn("carried-one-call-failed");
    turn.record_mut(|record| record.session.agent = Agent::Claude);
    let root = engram_source_root_display(
        &fs::canonicalize(&worktree)
            .expect("the worktree canonicalizes")
            .to_string_lossy(),
    );
    assert!(
        engram_one_call_reads(&root),
        "a plain test directory: {root}"
    );
    let line = format!("pushd \"{root}\" && node scripts/test-launcher.mjs full");
    let mut recorder = turn.recorder();
    recorder
        .command_started("gate", &line)
        .expect("the launch should record");
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            "gate",
            &line,
            "",
            CommandStatus::Success,
            EngramCommandExit::NotFinished,
        )
        .expect("the launch result should record");
    let start_basis =
        turn.record(|record| record.engram.carried_checks[0].check.start_basis.clone());
    start_basis
        .wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
        .expect("the launch snapshot was taken in time");
    let run = finish_run(&worktree, "failed", &"f".repeat(64));
    assert_carried_artifact_custody(&turn, &run, false);
}

#[test]
fn a_run_is_found_through_a_link_to_its_root_and_a_launch_stamped_late() {
    let temp = TestTempRoot::create("termal-engram-carried-alias");
    let root = temp.path().join("root");
    let runs = root.join(".git").join("review-runs");
    fs::create_dir_all(&runs).expect("the runs directory should be created");
    let canonical = fs::canonicalize(&root).expect("the root canonicalizes");
    let alias = temp.path().join("alias");
    link_directory(&alias, &canonical);
    let fingerprint = "f".repeat(64);
    let at = |second: u32| format!("2026-09-29T00:00:{second:02}.000Z");
    let none = std::collections::BTreeSet::new();

    // The launcher writes the directory as the line gave it.
    write_run(
        &runs,
        "test-aliased",
        &alias,
        &at(30),
        "passed",
        &fingerprint,
    );
    let aliased = canonical
        .join(".git")
        .join("review-runs")
        .join("test-aliased");
    let (found, _, _) = engram_find_carried_run("session-1", &canonical, &at(20), &[], &none);
    assert_eq!(found, Some(aliased.clone()));

    // A run that started a little before the host stamped the launch is
    // still the launch's; one well before it is not.
    let (found, _, _) = engram_find_carried_run("session-1", &canonical, &at(32), &[], &none);
    assert_eq!(found, Some(aliased.clone()));
    let (found, ruled_out, _) =
        engram_find_carried_run("session-1", &canonical, &at(59), &[], &none);
    assert_eq!(found, None);
    assert_eq!(ruled_out, [aliased]);
}

#[test]
fn concurrent_artifact_polls_keep_the_first_terminal_digest_before_a_later_read() {
    let (turn, worktree) = ordinary_carried_turn("carried-in-flight-ordinary");
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let first_digest = engram_terminal_record_digest(&run).unwrap();
    let (paused, release) = install_test_engram_carried_poll_pause(&turn.state);
    let state = turn.state.clone();
    let first = std::thread::spawn(move || state.poll_engram_carried_runs());
    let mut second = None;
    let inspected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        receive(&paused, "the actual first terminal read pauses before install");
        let mut record: Value = serde_json::from_slice(&fs::read(run.join("results.json")).unwrap()).unwrap();
        record["note"] = json!("rewritten after the first terminal read");
        fs::write(run.join("results.json"), serde_json::to_vec(&record).unwrap()).unwrap();
        let waiting = install_test_engram_carried_poll_waiting(&turn.state);
        let state = turn.state.clone();
        second = Some(std::thread::spawn(move || state.poll_engram_carried_runs()));
        receive(&waiting, "the real second poll waits for the first poll owner");
    }));
    let _ = release.send(());
    first.join().expect("the first poll is always released and joined");
    if let Some(second) = second { second.join().expect("the serialized poll joins"); }
    if let Err(panic) = inspected { std::panic::resume_unwind(panic); }
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks[0].terminal_digest.as_deref(), Some(first_digest.as_str()));
    });
    assert_carried_artifact_rejected(&turn, &run,
        "its terminal record changed after the host first read it as terminal");
}

#[test]
fn a_run_that_ended_before_the_launch_is_not_taken() {
    let temp = TestTempRoot::create("termal-engram-carried-earlier");
    let root = temp.path().join("root");
    let runs = root.join(".git").join("review-runs");
    fs::create_dir_all(&runs).expect("the runs directory should be created");
    let fingerprint = "f".repeat(64);
    let at = |second: u32| format!("2026-09-29T00:00:{second:02}.000Z");
    let none = std::collections::BTreeSet::new();
    // An earlier run that started within the slack before the stamp, but had
    // ended by it.
    let earlier = write_run(
        &runs,
        "test-earlier",
        &root,
        &at(25),
        "failed",
        &fingerprint,
    );
    let mut record: Value =
        serde_json::from_slice(&fs::read(earlier.join("results.json")).unwrap()).unwrap();
    record["ended"] = json!(at(27));
    fs::write(
        earlier.join("results.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let own = write_run(&runs, "test-own", &root, &at(31), "passed", &fingerprint);

    let (found, ruled_out, _) = engram_find_carried_run("session-1", &root, &at(30), &[], &none);
    assert_eq!(found, Some(own));
    assert!(ruled_out.contains(&earlier), "{ruled_out:?}");
}

#[test]
fn a_run_a_dropped_gate_used_is_never_taken_by_a_later_launch() {
    let (turn, worktree) = ordinary_carried_turn("carried-consumed");
    ordinary_launch_gate(&turn, &worktree, true);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    // Still going, so the slack alone would let a launch just after take it.
    fs::write(
        run.join("results.json"),
        serde_json::to_vec(&json!({ "runId": "test-carried", "state": "running" })).unwrap(),
    )
    .unwrap();
    turn.state.poll_engram_carried_runs();
    turn.record(|record| {
        assert_eq!(
            record.engram.carried_checks[0].run_directory.as_ref(),
            Some(&run)
        );
    });
    // Retire through the actual bounded carried-launch owner, without a
    // checkpoint, receipt or direct insertion of a consumed directory.
    for number in 0..ENGRAM_CARRIED_CHECK_LIMIT {
        launch_detached(&turn, &worktree, &format!("later-{number}"));
    }
    turn.state.poll_engram_carried_runs();
    let used = turn.record(|record| {
        assert!(record.engram.carried_checks.iter().all(|carried| carried.check.key != "gate"));
        assert!(record.engram.carried_checks.iter().all(|carried| carried.run_directory.as_ref() != Some(&run)));
        assert!(record.engram.carried_consumed_runs.contains(&run));
        record.engram.carried_consumed_runs.clone()
    });
    let (found, _, _) = engram_find_carried_run(&turn.session_id, &worktree,
        &(chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
        &used, &std::collections::BTreeSet::new());
    assert_eq!(found, None, "a retired locator cannot become a later launch's run");
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn a_restart_notice_is_told_once_through_the_store() {
    let (turn, worktree) = ordinary_carried_turn("carried-restart-store");
    ordinary_launch_gate(&turn, &worktree, false);
    // A gate outlives its turn: the session is idle when the host goes
    // down, so nothing else marks it to be written again.
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    // The observation worker settles its receipt after the request is seen.
    // Wait for it to finish, so no write lands between the load and the
    // persist of the loaded state below.
    let guard = phase_sync::PollGuard::new();
    loop {
        if turn.record(|record| record.engram.unadmitted_active_turn.is_none()
            && !record.engram.unadmitted_worker_running) { break; }
        guard.wait("the ordinary observation completes before the restart is simulated");
    }
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);
    turn.state.shutdown_persist_blocking();
    let path = turn.state.persistence_path.as_path();
    {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        persist_state(path, &inner).expect("the state persists");
    }
    let loaded = load_state(path)
        .expect("the state loads")
        .expect("a state was stored");
    let index = loaded
        .find_session_index(&turn.session_id)
        .expect("the session loads");
    let record = &loaded.sessions[index];
    assert!(record.engram.carried_checks.is_empty());
    assert!(
        record
            .engram
            .pending_source_root_line
            .as_deref()
            .is_some_and(|line| line.contains("the host restarted while the background full gate")),
        "{:?}",
        record.engram.pending_source_root_line
    );
    // Removing the durable marker must select this row for the first
    // persist delta even though its runtime was Idle at shutdown.
    assert_ne!(record.mutation_stamp, 0);
    let delta = collect_persist_delta_from_shared_state(&StateMutex::new(loaded), 0);
    assert!(delta.changed_sessions.iter().any(|record|
        record.session.id == turn.session_id), "the marker removal reaches the first delta");
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_via_cache_timed(&mut cache, path, &delta, &mut PersistTickTimings::default())
        .expect("the first delta persists the marker removal");
    let again = load_state(path)
        .expect("the state loads")
        .expect("a state was stored");
    let index = again
        .find_session_index(&turn.session_id)
        .expect("the session loads");
    assert_eq!(again.sessions[index].engram.pending_source_root_line, None);
    assert_eq!(again.sessions[index].mutation_stamp, 0,
        "a later clean restart has no marker removal to persist");
}

#[test]
fn a_project_settings_reset_tells_the_holder_of_a_carried_gate() {
    let (turn, worktree) = ordinary_carried_turn("carried-reset");
    ordinary_launch_gate(&turn, &worktree, false);
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);
    let project_id = turn.record(|record| {
        record
            .session
            .project_id
            .clone()
            .expect("the session has a project")
    });
    turn.state
        .update_project_engram_settings(
            &project_id,
            EngramProjectSettings {
                enabled: false,
                ..EngramProjectSettings::default()
            },
        )
        .expect("disabling Engram should succeed");

    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    assert!(
        line.contains("is no longer followed")
            && line.contains("its project's Engram settings changed before it settled")
            && !line.contains("earned no credit"),
        "{line}"
    );
    assert!(
        turn.record(|record| record.engram.work_binding.is_none()),
        "a claim made in the old store does not stay bound"
    );
}

/// Writes a run `run_id` of the gate for `worktree`, started now, that
/// ended `state`.
fn write_named_run(worktree: &FsPath, run_id: &str, state: &str) -> PathBuf {
    let runs = review_runs(worktree);
    let canonical = fs::canonicalize(worktree).expect("the worktree canonicalizes");
    write_run(
        &runs,
        run_id,
        &canonical,
        &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        state,
        &"f".repeat(64),
    )
}

/// Launches the detached full gate as `key` in `worktree`, without asserting
/// what became of it.
fn launch_detached(turn: &EOCheckedTurn, worktree: &FsPath, key: &str) {
    let cwd = fs::canonicalize(worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in(key, DETACHED_GATE, Some(DETACHED_GATE), Some(&cwd))
        .expect("the launch should record");
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            key,
            DETACHED_GATE,
            "STARTED",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the launch result should record");
}

#[test]
fn a_launch_after_one_with_no_run_matched_yet_is_refused_as_ambiguous() {
    // Two launches before a poll matched the first to its run: the run found
    // could be either's, so the second is refused, even though a passed run
    // is there, and the first is fenced by the second's launch.
    let (turn, worktree) = ordinary_carried_turn("carried-ambiguous");
    launch_detached(&turn, &worktree, "first");
    launch_detached(&turn, &worktree, "second");
    assert!(turn.record(|record| record.engram.carried_checks[1].ambiguous));
    write_named_run(&worktree, "test-first", "passed");
    turn.state.poll_engram_carried_runs();
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[1].run_directory.clone()),
        None,
        "an ambiguous launch takes no run"
    );
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    turn.record(|record| {
        assert!(record.engram.carried_checks[1].ambiguous);
        assert!(record.engram.carried_checks[1].run_directory.is_none());
        assert!(record.engram.carried_consumed_runs.is_empty());
    });
}

#[test]
fn a_launch_after_one_matched_to_its_run_takes_its_own() {
    let (turn, worktree) = ordinary_carried_turn("carried-matched-first");
    launch_detached(&turn, &worktree, "first");
    let first = write_named_run(&worktree, "test-first", "passed");
    turn.state.poll_engram_carried_runs();
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0].run_directory.clone()),
        Some(first.clone())
    );
    launch_detached(&turn, &worktree, "second");
    assert!(!turn.record(|record| record.engram.carried_checks[1].ambiguous));
    let second = write_named_run(&worktree, "test-second", "failed");
    turn.state.poll_engram_carried_runs();
    turn.record(|record| {
        let carried = &record.engram.carried_checks[1];
        assert!(!carried.ambiguous && carried.fence.is_none(),
            "the clean distinct-run control remains available without an unrelated fence");
        assert_ne!(carried.run_directory.as_ref(), Some(&first));
    });
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[1].run_directory.clone()),
        Some(second)
    );
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn a_launch_after_a_dropped_compound_launch_is_refused_as_ambiguous() {
    let (turn, worktree) = ordinary_carried_turn("carried-after-compound");
    launch_command(
        &turn,
        &worktree,
        "node scripts/test-launcher.mjs full --detach --notify session-9 && echo x",
        EngramCommandExit::Code(0),
    );
    launch_detached(&turn, &worktree, "again");
    assert!(turn.record(|record| record.engram.carried_checks[0].ambiguous));
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn a_file_change_while_the_gate_is_being_launched_fences_it() {
    let (turn, worktree) = ordinary_carried_turn("carried-watched-launch");
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in("gate", GATE, Some(GATE), Some(&cwd))
        .expect("the launch should record");
    turn.wait_for_snapshots();
    turn.state
        .note_engram_workspace_file_changes(&[WorkspaceFileChangeEvent {
            path: fs::canonicalize(&worktree)
                .unwrap()
                .join("README.md")
                .to_string_lossy()
                .into_owned(),
            kind: WorkspaceFileChangeKind::Modified,
            root_path: None,
            session_id: None,
            mtime_ms: None,
            size_bytes: None,
        }]);
    recorder
        .command_completed_with_exit(
            "gate",
            GATE,
            "",
            CommandStatus::Success,
            EngramCommandExit::NotFinished,
        )
        .expect("the launch result should record");
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(&turn, "(while it was being launched)");
}

#[test]
fn a_missing_launch_basis_never_turns_a_valid_carried_artifact_into_native_verification() {
    let (turn, worktree) = ordinary_carried_turn("carried-failed-snapshot");
    ordinary_launch_gate(&turn, &worktree, false);
    let failed = Arc::new(EngramBasisCapture::default());
    failed.finish(None);
    turn.record_mut(|record| record.engram.carried_checks[0].check.start_basis = failed);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_artifact_custody(&turn, &run, true);
    turn.record(|record| {
        assert!(matches!(record.engram.carried_checks[0].check.start_basis
            .wait_until(std::time::Instant::now()), Some(None)));
        assert!(record.engram.carried_consumed_runs.is_empty());
    });
}

#[test]
fn a_launch_after_one_refused_for_an_overlap_is_refused_as_ambiguous() {
    // A launch refused before it was carried may still have started a run,
    // which the next launch in the worktree must not take.
    let (turn, worktree) = ordinary_carried_turn("carried-after-refused");
    let mut recorder = turn.recorder();
    let cwd = fs::canonicalize(&worktree).unwrap().to_string_lossy().into_owned();
    recorder.command_started_in("other", "git status", Some("git status"), Some(&cwd)).unwrap();
    launch_detached(&turn, &worktree, "first");
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    recorder
        .command_completed_with_exit(
            "other",
            "git status",
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the other command should end");
    launch_detached(&turn, &worktree, "second");

    assert!(turn.record(|record| record.engram.carried_checks[0].ambiguous));
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn expiry_never_converts_an_already_terminal_artifact_into_native_verification() {
    let (turn, worktree) = ordinary_carried_turn("carried-expired-terminal");
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let digest = turn.record(|record| record.engram.carried_checks[0].terminal_digest.clone().unwrap());
    turn.record_mut(|record| {
        record.engram.carried_checks[0].check.started_at =
            (chrono::Utc::now() - chrono::Duration::hours(7)).to_rfc3339();
    });
    turn.state.poll_engram_carried_runs();
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(engram_carried_check_expired(carried, chrono::Utc::now()));
        assert!(!engram_carried_check_polls(carried, chrono::Utc::now()));
        assert_eq!(carried.run_directory.as_deref(), Some(run.as_path()));
        assert_eq!(carried.terminal_digest.as_deref(), Some(digest.as_str()));
        assert!(record.engram.carried_consumed_runs.is_empty());
    });
}

#[test]
fn a_run_found_already_ended_in_the_slack_is_pinned_by_that_first_read() {
    // A run that started in the slack before the stamp is read at matching;
    // when that read finds it terminal, it is the first terminal read, and
    // its digest is returned to be pinned rather than read again.
    let temp = TestTempRoot::create("termal-engram-carried-pinned");
    let root = temp.path().join("root");
    let runs = root.join(".git").join("review-runs");
    fs::create_dir_all(&runs).expect("the runs directory should be created");
    let at = |second: u32| format!("2026-09-29T00:00:{second:02}.000Z");
    let none = std::collections::BTreeSet::new();
    let run = write_run(
        &runs,
        "test-slack",
        &root,
        &at(25),
        "passed",
        &"f".repeat(64),
    );
    let mut record: Value =
        serde_json::from_slice(&fs::read(run.join("results.json")).unwrap()).unwrap();
    record["ended"] = json!(at(40));
    let bytes = serde_json::to_vec(&record).unwrap();
    fs::write(run.join("results.json"), &bytes).unwrap();

    let (found, _, first_terminal) =
        engram_find_carried_run("session-1", &root, &at(30), &[], &none);
    assert_eq!(found, Some(run.clone()));
    assert_eq!(first_terminal, Some(sha256_hex(&bytes)));

    // A run that started after the stamp is not read at matching.
    let later = write_run(
        &runs,
        "test-later",
        &root,
        &at(50),
        "passed",
        &"f".repeat(64),
    );
    let skip: std::collections::BTreeSet<PathBuf> = [run].into_iter().collect();
    let (found, _, first_terminal) =
        engram_find_carried_run("session-1", &root, &at(45), &[], &skip);
    assert_eq!(found, Some(later));
    assert_eq!(first_terminal, None);
}

#[test]
fn a_relaunch_after_the_stated_wait_is_carried_unambiguously() {
    // A refused launch arms the ambiguity window; the relaunch its line asks
    // for, two minutes on, is carried as the holder's own.
    let (turn, worktree) = ordinary_carried_turn("carried-remedy");
    let mut recorder = turn.recorder();
    let cwd = fs::canonicalize(&worktree).unwrap().to_string_lossy().into_owned();
    recorder.command_started_in("other", "git status", Some("git status"), Some(&cwd)).unwrap();
    launch_detached(&turn, &worktree, "first");
    recorder
        .command_completed_with_exit(
            "other",
            "git status",
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the other command should end");
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    assert!(
        line.contains("two minutes or more after this launch"),
        "{line}"
    );
    // The stated wait has passed.
    let earlier = (chrono::Utc::now()
        - chrono::Duration::seconds(ENGRAM_CARRIED_AMBIGUITY_SECONDS + 1))
    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    turn.record_mut(|record| {
        assert_eq!(record.engram.carried_unmatched_launches.len(), 1);
        record.engram.carried_unmatched_launches[0].1 = earlier;
    });
    launch_detached(&turn, &worktree, "again");

    assert!(!turn.record(|record| record.engram.carried_checks[0].ambiguous));
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn a_delegated_session_is_told_its_background_gate_is_not_carried() {
    let (turn, worktree) = ordinary_carried_turn("carried-delegated");
    turn.record_mut(|record| {
        record.session.parent_delegation_id = Some("delegation-parent".to_owned())
    });
    launch_detached(&turn, &worktree, "gate");

    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    assert!(
        line.contains("a delegated session's turns are measured in its workdir"),
        "{line}"
    );
    assert!(!line.contains("termal_name_source_root"), "{line}");
    // A child-shaped session whose delegation row does not exist has no
    // source-root target, so its closing capture reports nothing: end the
    // turn without waiting for an observation production never sends, and
    // check that it made no native request.
    turn.end();
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[test]
fn only_the_scratch_directory_at_its_own_worktree_root_is_scratch() {
    let root = if cfg!(windows) { "c:/repo" } else { "/repo" };
    let nested = format!("{root}/.tmp/wt-x");
    // Under the worktree's own .tmp/, at any depth, or .tmp itself.
    assert!(engram_is_worktree_scratch(
        root,
        &format!("{root}/.tmp"),
        Some("a.log")
    ));
    assert!(engram_is_worktree_scratch(
        root,
        &format!("{root}/.tmp/probe/deep"),
        Some("a")
    ));
    assert!(engram_is_worktree_scratch(root, root, Some(".tmp")));
    // Not scratch: the worktree's own files, a directory that only starts
    // with the name, or .tmp deeper in the tree.
    assert!(!engram_is_worktree_scratch(root, root, Some("README.md")));
    assert!(!engram_is_worktree_scratch(
        root,
        &format!("{root}/.tmpx"),
        Some("a")
    ));
    assert!(!engram_is_worktree_scratch(
        root,
        &format!("{root}/src/.tmp"),
        Some("a")
    ));
    // Exact: on a case-sensitive volume .TMP is another directory, which Git
    // does not ignore and which may hold source.
    assert!(!engram_is_worktree_scratch(
        root,
        &format!("{root}/.TMP"),
        Some("a")
    ));
    assert!(!engram_is_worktree_scratch(root, root, Some(".TMP")));
    assert!(!engram_is_worktree_scratch(root, root, None));
    // A worktree nested under another checkout's .tmp/ is judged against its
    // own root: its source files are not scratch, its own .tmp/ is.
    assert!(!engram_is_worktree_scratch(
        &nested,
        &format!("{nested}/src"),
        Some("lib.rs")
    ));
    assert!(engram_is_worktree_scratch(
        &nested,
        &format!("{nested}/.tmp"),
        Some("a")
    ));
}

#[test]
fn a_scratch_write_under_tmp_leaves_a_carried_gate_alone_and_a_source_write_does_not() {
    let (turn, worktree) = ordinary_carried_turn("carried-scratch");
    ordinary_launch_gate(&turn, &worktree, false);
    let canonical = fs::canonicalize(&worktree).unwrap();
    let scratch = canonical.join(".tmp").join("probe");
    fs::create_dir_all(&scratch).expect("the scratch directory should be created");
    fs::write(scratch.join("run.log"), "scratch\n").unwrap();
    let change = |path: PathBuf| WorkspaceFileChangeEvent {
        path: path.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Modified,
        root_path: None,
        session_id: None,
        mtime_ms: None,
        size_bytes: None,
    };
    turn.state.note_engram_workspace_file_changes(&[
        change(scratch.join("run.log")),
        change(canonical.join(".tmp")),
        // A scratch directory already gone when its event is handled.
        change(canonical.join(".tmp").join("gone").join("x.log")),
    ]);
    assert!(
        turn.record(|record| record.engram.carried_checks[0].fence.is_none()),
        "{:?}",
        turn.record(|record| record.engram.carried_checks[0].fence.clone())
    );

    // A batch mixing scratch and source fences, naming the source path.
    turn.state.note_engram_workspace_file_changes(&[
        change(scratch.join("other.log")),
        change(canonical.join("README.md")),
    ]);
    let fence = turn
        .record(|record| record.engram.carried_checks[0].fence.clone())
        .expect("the source write fences the gate");
    assert!(fence.contains("README.md"), "{fence}");
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(&turn, "a file in its worktree changed");
}

/// The shape of a gate that lost its credit: a Rust test fixture's store
/// (`termal.sqlite` and its journal) in a `termal-test-state-*` directory
/// under the gate worktree's own `.tmp/`, removed when the fixture drops,
/// reported by the watcher in its own spelling (`normalize_user_facing_path`,
/// not the verbatim form). Changes there, while the directory exists
/// and after it is gone, alone or in a batch with the directory's own
/// deletion, leave the carried launch and its artifact custody unfenced.
#[test]
fn a_fixture_store_under_a_removed_scratch_directory_leaves_a_carried_gate_alone() {
    let (turn, worktree) = ordinary_carried_turn("carried-scratch-fixture-store");
    ordinary_launch_gate(&turn, &worktree, false);
    let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
    let store = watched.join(".tmp").join("termal-test-state-0000");
    fs::create_dir_all(&store).expect("the fixture store directory should be created");
    let journal = store.join("termal.sqlite-journal");
    fs::write(&journal, "journal\n").unwrap();
    let change = |path: &FsPath, kind: WorkspaceFileChangeKind| WorkspaceFileChangeEvent {
        path: path.to_string_lossy().into_owned(),
        kind,
        root_path: None,
        session_id: None,
        mtime_ms: None,
        size_bytes: None,
    };
    let fence = || turn.record(|record| record.engram.carried_checks[0].fence.clone());

    turn.state
        .note_engram_workspace_file_changes(&[change(&journal, WorkspaceFileChangeKind::Modified)]);
    assert_eq!(fence(), None, "while the fixture store exists");

    fs::remove_dir_all(&store).expect("the fixture store directory should be removed");
    for batch in [
        vec![change(&journal, WorkspaceFileChangeKind::Deleted)],
        vec![change(&journal, WorkspaceFileChangeKind::Modified)],
        vec![
            change(&journal, WorkspaceFileChangeKind::Deleted),
            change(&store, WorkspaceFileChangeKind::Deleted),
        ],
        vec![change(&store, WorkspaceFileChangeKind::Deleted)],
    ] {
        turn.state.note_engram_workspace_file_changes(&batch);
        assert_eq!(
            fence(),
            None,
            "after the fixture store is gone: {:?}",
            batch.iter().map(|change| change.path.clone()).collect::<Vec<_>>()
        );
    }

    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    assert_carried_artifact_custody(&turn, &run, true);
}

/// A fixture that runs `git init` in its scratch directory under the gate
/// worktree's `.tmp/` after the watcher already routed a change there: the
/// routing key the watcher remembers for that directory (up to 30 s) still
/// names the gate's worktree, while a fresh resolution finds a repository of
/// the fixture's own. A change in that git-ignored directory still leaves the
/// carried launch and its artifact custody unfenced.
#[test]
fn a_fixture_store_under_a_scratch_directory_that_becomes_a_repository_leaves_a_carried_gate_alone()
{
    let (turn, worktree) = ordinary_carried_turn("carried-scratch-fixture-repository");
    // As this repository's .gitignore does, which the exemption assumes:
    // what stays under .tmp/ while the launch is observed is no source.
    fs::write(worktree.join(".gitignore"), ".tmp\n").expect("the worktree ignores .tmp");
    ordinary_launch_gate(&turn, &worktree, false);
    let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
    let repository = watched.join(".tmp").join("termal-test-state-0001");
    fs::create_dir_all(&repository).expect("the fixture directory should be created");
    let journal = repository.join("termal.sqlite-journal");
    fs::write(&journal, "journal\n").unwrap();
    let change = |path: &FsPath| WorkspaceFileChangeEvent {
        path: path.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Modified,
        root_path: None,
        session_id: None,
        mtime_ms: None,
        size_bytes: None,
    };
    let fence = || turn.record(|record| record.engram.carried_checks[0].fence.clone());

    turn.state.note_engram_workspace_file_changes(&[change(&journal)]);
    assert_eq!(fence(), None, "before the fixture's git init");
    fs::create_dir_all(repository.join(".git")).expect("the fixture's .git should be created");
    turn.state.note_engram_workspace_file_changes(&[change(&journal)]);
    assert_eq!(
        fence(),
        None,
        "after the fixture's git init, with the routing key still remembered"
    );

    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    assert_carried_artifact_custody(&turn, &run, true);
}

/// A path that names `.tmp` but is not under the scratch directory at its
/// worktree's root (here `src/.tmp/`) still fences a carried gate, and the
/// fence's cause states what the scratch test judged it by: the worktree key
/// it was routed to, the exact keys of its worktree root and its directory,
/// and whether that directory existed when they were resolved.
#[test]
fn a_tmp_naming_path_that_fences_states_the_scratch_tests_inputs_in_the_cause() {
    let change = |path: &FsPath| WorkspaceFileChangeEvent {
        path: path.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Modified,
        root_path: None,
        session_id: None,
        mtime_ms: None,
        size_bytes: None,
    };
    for (name, present) in [
        ("carried-scratch-detail-present", true),
        ("carried-scratch-detail-gone", false),
    ] {
        let (turn, worktree) = ordinary_carried_turn(name);
        ordinary_launch_gate(&turn, &worktree, false);
        let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
        let directory = watched.join("src").join(".tmp");
        if present {
            fs::create_dir_all(&directory).expect("the nested .tmp should be created");
            fs::write(directory.join("x.log"), "x\n").unwrap();
        }
        let path = directory.join("x.log");
        turn.state.note_engram_workspace_file_changes(&[change(&path)]);
        let fence = turn
            .record(|record| record.engram.carried_checks[0].fence.clone())
            .expect("a change under a nested .tmp fences the gate");

        let canonical = engram_canonical_path(&directory);
        let root = engram_worktree_root_path(&canonical);
        let existed = if present { "existed" } else { "was gone" };
        assert_eq!(
            fence,
            format!(
                "a file in its worktree changed: {} (not left out as scratch: routed to {}; \
                 worktree root {}; directory {}, which {existed} when resolved); \
                 modification time unknown",
                path.to_string_lossy(),
                engram_path_key(&root),
                engram_exact_path_key(&root),
                engram_exact_path_key(&canonical),
            )
        );
        assert!(
            fence.contains("not left out as scratch: routed to "),
            "{fence}"
        );
        assert!(
            fence.contains(if present {
                "which existed when resolved"
            } else {
                "which was gone when resolved"
            }),
            "{fence}"
        );
        finish_run(&worktree, "passed", &"f".repeat(64));
        assert_carried_fence_custody(&turn, "not left out as scratch");
    }
}

#[test]
fn a_launch_while_another_writable_session_is_in_a_turn_there_is_not_carried() {
    // Once a gate is carried, another session's presence in a turn fences
    // nothing. This rule at the launch is what covers a command that session
    // started before it, whose end would say nothing: such a launch is not
    // carried at all.
    let (turn, worktree) = ordinary_carried_turn("carried-launch-beside-session");
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let other = create_test_project_session(&turn.state, Agent::Codex, &project_id, &worktree);
    turn.state.note_engram_session_worktree_off_lock(&other);
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("other session");
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    launch_command(&turn, &worktree, GATE, EngramCommandExit::NotFinished);
    assert_dropped_at_launch(&turn, "another command or another writable session");
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());

    // In a turn in another worktree, it leaves the launch alone.
    let (turn, worktree) = ordinary_carried_turn("carried-launch-beside-session-elsewhere");
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let elsewhere = sibling_worktree(&turn, "carried-launch-beside-session-sibling");
    let other = create_test_project_session(&turn.state, Agent::Codex, &project_id, &elsewhere);
    turn.state.note_engram_session_worktree_off_lock(&other);
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("other session");
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    ordinary_launch_gate(&turn, &worktree, false);
    assert_eq!(carried_fence(&turn), None);
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn another_sessions_command_later_described_in_the_worktree_refuses_the_gate() {
    // An ACP runtime may give a command's directory only in a later update:
    // the description places it, and gives no line to read.
    let (turn, worktree) = ordinary_carried_turn("carried-other-described");
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let elsewhere = sibling_worktree(&turn, "carried-other-described-sibling");
    let other = create_test_project_session(&turn.state, Agent::Cursor, &project_id, &elsewhere);
    ordinary_launch_gate(&turn, &worktree, false);
    let mut recorder = SessionRecorder::new(turn.state.clone(), other);
    recorder
        .command_started_in(
            "late",
            "write-things",
            Some("write-things"),
            Some(&elsewhere.to_string_lossy()),
        )
        .expect("the start should record");
    assert_eq!(carried_fence(&turn), None);
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    recorder
        .command_described("late", None, Some(&cwd))
        .expect("the description should record");
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(
        &turn,
        "ran a command there (its line TermAl was not told)",
    );
}

/// The provenance a Claude reader gives the frames of the turn running on
/// `turn`'s session now.
fn claude_turn_provenance(turn: &EOCheckedTurn) -> ClaudeObservationProvenance {
    ClaudeObservationProvenance {
        token: turn.record(|record| record.runtime.runtime_token().expect("runtime")),
        origin: ClaudeWorkOrigin::Attempt {
            turn_generation: turn.record(|record| record.active_turn_generation),
        },
    }
}

/// Claude's background launch of the full gate in `worktree` as call `key`,
/// in the one-call form, as its stdout reader applies it: the call admitted
/// first, as outstanding background work, then started and ended at the
/// sink with the reader's provenance (`claude_outstanding_work.rs`).
fn launch_claude_background_gate(turn: &EOCheckedTurn, worktree: &FsPath, key: &str) {
    let root = engram_source_root_display(
        &fs::canonicalize(worktree)
            .expect("the worktree canonicalizes")
            .to_string_lossy(),
    );
    let line = format!("pushd \"{root}\" && node scripts/test-launcher.mjs full");
    let provenance = claude_turn_provenance(turn);
    let frame = json!({"type": "assistant", "message": {"content": [{"type": "tool_use",
        "id": key, "name": "Bash", "input": {"command": line, "run_in_background": true}}]}});
    turn.state.admit_claude_frame(
        &turn.session_id,
        &provenance,
        false,
        &claude_frame_work(&frame, false),
    );
    let claude = EngramObservationProvenance::Claude(provenance);
    turn.state.engram_host().observe(
        &turn.session_id,
        &claude,
        EngramRecorderObservation::CommandStarted {
            key,
            ran: Some(&line),
            cwd: None,
        },
    );
    turn.wait_for_snapshots();
    turn.state.engram_host().observe(
        &turn.session_id,
        &claude,
        EngramRecorderObservation::CommandFinished {
            key,
            command: &line,
            reported_command: Some(&line),
            output: "Command running in background with ID: b1.",
            exit: Some(EngramCommandExit::NotFinished),
            runtime_output_cut: false,
        },
    );
}

/// Admits a background call `key` of `name` on the session `session_id`,
/// with `provenance`, as its reader would: outstanding Claude work.
fn admit_background_call(
    turn: &EOCheckedTurn,
    session_id: &str,
    provenance: &ClaudeObservationProvenance,
    key: &str,
    name: &str,
) {
    let frame = json!({"type": "assistant", "message": {"content": [{"type": "tool_use",
        "id": key, "name": name, "input": {"command": "sleep 60", "prompt": "work",
            "run_in_background": true}}]}});
    turn.state.admit_claude_frame(
        session_id,
        provenance,
        false,
        &claude_frame_work(&frame, false),
    );
}

#[test]
fn a_claude_background_launch_keeps_its_artifact_but_withholds_ordinary_source_attribution() {
    let (turn, worktree) = ordinary_carried_turn("self-gate-clean");
    launch_claude_background_gate(&turn, &worktree, "gate");
    turn.record(|record| {
        assert!(
            record.claude_outstanding.holds("gate"),
            "its launch is outstanding background work"
        );
        assert_eq!(record.engram.carried_checks.len(), 1, "and it is carried");
        let carried = &record.engram.carried_checks[0];
        assert_eq!(carried.fence, None);
        assert_eq!(carried.check.fenced_by_outstanding, None);
        assert!(record.engram.unadmitted_active_turn.as_ref().unwrap().source_attribution_uncertain,
            "ordinary source attribution has no self-launch exception");
    });
    let start_basis =
        turn.record(|record| record.engram.carried_checks[0].check.start_basis.clone());
    start_basis
        .wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
        .expect("the launch snapshot was taken in time");
    // Once carried, the gate's own call still does not fence its run when
    // it is reconciled again.
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&turn.session_id)
            .expect("the root");
        EngramHost::reconcile_claude_work(&mut inner, index, &["gate".to_owned()]);
        assert_eq!(inner.sessions[index].engram.carried_checks[0].fence, None);
    }
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let observation = assert_carried_artifact_custody(&turn, &run, true);
    // The occurrence always carries `source_change`; withholding is an
    // explicit null. No baseline was installed, so an attributed change
    // would be an `assumed_missing_baseline` object, not null.
    assert!(observation["occurrence"].as_object().unwrap().contains_key("source_change"));
    assert!(observation["occurrence"]["source_change"].is_null(),
        "outstanding own work cannot receive ordinary source attribution: {observation:#}");
}

#[test]
fn any_other_outstanding_work_still_refuses_a_claude_background_gate() {
    // Earlier background work of its own session.
    let (turn, worktree) = ordinary_carried_turn("self-gate-earlier-work");
    let provenance = claude_turn_provenance(&turn);
    admit_background_call(&turn, &turn.session_id, &provenance, "earlier", "Bash");
    launch_claude_background_gate(&turn, &worktree, "gate");
    turn.record(|record| {
        assert!(record.engram.carried_checks.is_empty(), "it is not carried");
        let pending = record
            .engram
            .pending_source_root_line
            .clone()
            .unwrap_or_default();
        assert!(
            pending.contains("this session's own Claude work"),
            "the refusal names the stored cause: {pending}"
        );
        assert!(
            !pending.contains("two minutes"),
            "and promises no relaunch: {pending}"
        );
    });
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());

    // Two launches of the same line cannot exempt each other.
    let (turn, worktree) = ordinary_carried_turn("self-gate-twice");
    launch_claude_background_gate(&turn, &worktree, "gate-1");
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);
    // The second launch of the same line: its check starts fenced by the
    // first launch's outstanding call, which is not its own.
    let root = engram_source_root_display(
        &fs::canonicalize(&worktree)
            .expect("the worktree canonicalizes")
            .to_string_lossy(),
    );
    let line = format!("pushd \"{root}\" && node scripts/test-launcher.mjs full");
    let provenance = claude_turn_provenance(&turn);
    let frame = json!({"type": "assistant", "message": {"content": [{"type": "tool_use",
        "id": "gate-2", "name": "Bash", "input": {"command": line, "run_in_background": true}}]}});
    turn.state.admit_claude_frame(
        &turn.session_id,
        &provenance,
        false,
        &claude_frame_work(&frame, false),
    );
    turn.state.engram_host().observe(
        &turn.session_id,
        &EngramObservationProvenance::Claude(provenance.clone()),
        EngramRecorderObservation::CommandStarted {
            key: "gate-2",
            ran: Some(&line),
            cwd: None,
        },
    );
    turn.record(|record| {
        let second = record
            .engram
            .active_turn_checks
            .iter()
            .find(|check| check.key == "gate-2")
            .expect("the second launch starts a check");
        assert_eq!(
            second.fenced_by_outstanding,
            Some(ClaudeHazardCause::OwnSession),
            "the first launch is not the second's own call"
        );
    });
    turn.wait_for_snapshots();
    turn.state.engram_host().observe(
        &turn.session_id,
        &EngramObservationProvenance::Claude(provenance),
        EngramRecorderObservation::CommandFinished {
            key: "gate-2",
            command: &line,
            reported_command: Some(&line),
            output: "Command running in background with ID: b2.",
            exit: Some(EngramCommandExit::NotFinished),
            runtime_output_cut: false,
        },
    );
    turn.record(|record| {
        assert_eq!(
            record.engram.carried_checks.len(),
            1,
            "the second is refused"
        );
        let first = &record.engram.carried_checks[0];
        assert_eq!(first.check.key, "gate-1");
        assert_eq!(
            first.check.fenced_by_outstanding,
            Some(ClaudeHazardCause::OwnSession),
            "and the second's launch fences the first"
        );
        assert!(first.fence.is_some());
    });
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn another_sessions_background_work_fences_a_carried_gate_already_running_where_it_may_write() {
    // A background Task launch, which reports no command start, fences the
    // gate at once.
    let (turn, worktree) = ordinary_carried_turn("carried-other-task");
    ordinary_launch_gate(&turn, &worktree, false);
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let other = create_test_project_session(&turn.state, Agent::Claude, &project_id, &worktree);
    let stopped = ClaudeObservationProvenance {
        token: RuntimeToken::Claude("carried-other-task-runtime".to_owned()),
        origin: ClaudeWorkOrigin::Unattributed,
    };
    admit_background_call(&turn, &other, &stopped, "task", "Task");
    let fence = carried_fence(&turn).expect("the running gate is fenced");
    assert!(fence.contains(&other), "{fence}");
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0]
            .check
            .fenced_by_outstanding
            .clone()),
        Some(ClaudeHazardCause::OtherSession {
            session_id: other.clone(),
            name: "Test".to_owned(),
        })
    );
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());

    // Work registered elsewhere fences nothing, until a later command of it
    // is placed in the gate's worktree.
    let (turn, worktree) = ordinary_carried_turn("carried-other-extension");
    ordinary_launch_gate(&turn, &worktree, false);
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let elsewhere = sibling_worktree(&turn, "carried-other-extension-sibling");
    let other = create_test_project_session(&turn.state, Agent::Claude, &project_id, &elsewhere);
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("other session");
        let workdir = inner.sessions[index].session.workdir.clone();
        inner.sessions[index].engram.workdir_worktree =
            Some((workdir, engram_worktree_root(&elsewhere)));
    }
    let stopped = ClaudeObservationProvenance {
        token: RuntimeToken::Claude("carried-other-extension-runtime".to_owned()),
        origin: ClaudeWorkOrigin::Unattributed,
    };
    admit_background_call(&turn, &other, &stopped, "writer", "Bash");
    assert_eq!(carried_fence(&turn), None, "elsewhere fences nothing");
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    turn.state.engram_host().observe(
        &other,
        &EngramObservationProvenance::Claude(stopped),
        EngramRecorderObservation::CommandStarted {
            key: "writer",
            ran: Some("touch notes.txt"),
            cwd: Some(&cwd),
        },
    );
    let fence = carried_fence(&turn).expect("the extension fences the gate");
    assert!(fence.contains(&other), "{fence}");
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0]
            .check
            .fenced_by_outstanding
            .clone()),
        Some(ClaudeHazardCause::OtherSession {
            session_id: other.clone(),
            name: "Test".to_owned(),
        }),
        "the extended hazard stores its own cause"
    );
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

/// Runs an ordinary passing test in `worktree` to its end, with both its
/// snapshots taken.
fn run_check_in(turn: &EOCheckedTurn, worktree: &FsPath, key: &str) {
    let cwd = fs::canonicalize(worktree)
        .expect("the worktree canonicalizes")
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in(key, SIZE_TEST, Some(SIZE_TEST), Some(&cwd))
        .expect("the start should record");
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            key,
            SIZE_TEST,
            "running 3 tests\ntest result: ok. 3 passed",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the end should record");
    turn.wait_for_snapshots();
}

/// A Claude session working in a sibling of `worktree`, and a runtime of it
/// that was stopped: work it registers is another session's, outstanding.
fn other_claude_session_elsewhere(
    turn: &EOCheckedTurn,
    label: &str,
) -> (String, ClaudeObservationProvenance) {
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let elsewhere = sibling_worktree(turn, &format!("{label}-sibling"));
    let other = create_test_project_session(&turn.state, Agent::Claude, &project_id, &elsewhere);
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("other session");
        let workdir = inner.sessions[index].session.workdir.clone();
        inner.sessions[index].engram.workdir_worktree =
            Some((workdir, engram_worktree_root(&elsewhere)));
    }
    let stopped = ClaudeObservationProvenance {
        token: RuntimeToken::Claude(format!("{label}-runtime")),
        origin: ClaudeWorkOrigin::Unattributed,
    };
    (other, stopped)
}

/// The other session's outstanding call `key` is found running in
/// `worktree`: its place grows by that worktree.
fn place_other_work_in(
    turn: &EOCheckedTurn,
    other: &str,
    stopped: &ClaudeObservationProvenance,
    key: &str,
    worktree: &FsPath,
) {
    let cwd = fs::canonicalize(worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    turn.state.engram_host().observe(
        other,
        &EngramObservationProvenance::Claude(stopped.clone()),
        EngramRecorderObservation::CommandStarted {
            key,
            ran: Some("touch notes.txt"),
            cwd: Some(&cwd),
        },
    );
}

#[test]
fn a_newly_found_place_of_outstanding_work_reaches_a_finished_check_only_from_its_registration() {
    for (label, registered_first) in [
        ("late-place-before-check-end", true),
        ("late-place-after-check-end", false),
    ] {
        let (turn, worktree) = ordinary_carried_turn(label);
        let (other, stopped) = other_claude_session_elsewhere(&turn, label);
        if registered_first {
            admit_background_call(&turn, &other, &stopped, "writer", "Bash");
        }
        run_check_in(&turn, &worktree, "check");
        if !registered_first {
            admit_background_call(&turn, &other, &stopped, "writer", "Bash");
        }
        turn.record(|record| {
            let check = &record.engram.active_turn_checks[0];
            assert!(check.end.is_some() && !check.open_to_writes(), "{label}");
            assert_eq!(check.fenced_by_outstanding, None, "{label}: elsewhere");
        });
        // Found writing in the check's worktree after the check closed: what
        // ran since its registration may have written under the check.
        place_other_work_in(&turn, &other, &stopped, "writer", &worktree);
        turn.record(|record| {
            let check = &record.engram.active_turn_checks[0];
            assert_eq!(
                check.fenced_by_outstanding,
                registered_first.then(|| ClaudeHazardCause::OtherSession {
                    session_id: other.clone(),
                    name: "Test".to_owned(),
                }),
                "{label}"
            );
            assert_eq!(check.overlapped, registered_first, "{label}");
        });
        let observation = turn.finish();
        assert_ordinary_observation_has_no_native_credit(&turn, &observation);
        assert_eq!(observation["occurrence"]["observed_checks"][0]["observed_result"], "passed",
            "interference withholds source attribution, not a reported process exit");
    }
}

#[test]
fn a_newly_found_place_keeps_the_carried_artifact_and_its_original_interference_cut() {
    // Registration before the terminal read fences the launch; a later
    // registration remains potential interference. Neither can be converted
    // into a native verification by ordinary closing.
    for (label, registered_before_terminal) in [
        ("carried-late-place-before-end", true),
        ("carried-late-place-before-settlement", false),
    ] {
        let (turn, worktree) = ordinary_carried_turn(label);
        ordinary_launch_gate(&turn, &worktree, false);
        let (other, stopped) = other_claude_session_elsewhere(&turn, label);
        if registered_before_terminal {
            admit_background_call(&turn, &other, &stopped, "writer", "Bash");
        }
        let start_basis =
            turn.record(|record| record.engram.carried_checks[0].check.start_basis.clone());
        start_basis
            .wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
            .expect("the launch snapshot was taken in time");
        let run = finish_run(&worktree, "passed", &"f".repeat(64));
        turn.state.poll_engram_carried_runs();
        let digest = engram_terminal_record_digest(&run).unwrap();
        assert!(
            turn.record(|record| record.engram.carried_checks[0].terminal_digest.is_some()),
            "{label}: read as terminal"
        );
        if !registered_before_terminal {
            admit_background_call(&turn, &other, &stopped, "writer", "Bash");
        }
        assert_eq!(carried_fence(&turn), None, "{label}: elsewhere");
        place_other_work_in(&turn, &other, &stopped, "writer", &worktree);
        let (fence, potential) = turn.record(|record| {
            let carried = &record.engram.carried_checks[0];
            (carried.fence.clone(), carried.potential.clone())
        });
        if registered_before_terminal {
            // It precedes the first terminal read: fenced now.
            assert!(fence.is_some(), "{label}");
        } else {
            // Keep the later registration separately from original custody.
            assert_eq!(fence, None, "{label}");
            assert!(
                potential.as_ref().is_some_and(|(_, cause)| *cause
                    == ClaudeHazardCause::OtherSession {
                        session_id: other.clone(),
                        name: "Test".to_owned(),
                    }),
                "{label}: {potential:?}"
            );
        }
        let observation = turn.finish();
        assert_ordinary_observation_has_no_native_credit(&turn, &observation);
        // Withholding is an explicit null `source_change`; with no baseline
        // installed an attributed change would be an object.
        assert!(observation["occurrence"].as_object().unwrap().contains_key("source_change"));
        assert!(observation["occurrence"]["source_change"].is_null(),
            "retained work at closing withholds ordinary source attribution: {observation:#}");
        turn.record(|record| {
            let carried = &record.engram.carried_checks[0];
            assert_eq!(carried.run_directory.as_deref(), Some(run.as_path()));
            assert_eq!(carried.terminal_digest.as_deref(), Some(digest.as_str()));
            assert_eq!(carried.fence, fence);
            assert_eq!(carried.potential, potential);
            assert!(record.engram.carried_consumed_runs.is_empty());
        });
    }
}

/// A finished check of `turn` in its worktree whose command ended now, with
/// the given snapshots.
fn check_with_captures(
    turn: &EOCheckedTurn,
    start_basis: Arc<EngramBasisCapture>,
    end_basis: Arc<EngramBasisCapture>,
) -> EngramTurnCheck {
    EngramTurnCheck {
        start_basis,
        ended_at: Some(EngramHost::interference_tick()),
        ..turn.finished_check(0, end_basis)
    }
}

/// A snapshot already taken, with no basis.
fn closed_capture() -> Arc<EngramBasisCapture> {
    let capture = Arc::new(EngramBasisCapture::default());
    capture.finish(None);
    capture
}

#[test]
fn work_registered_while_a_checks_snapshot_is_taken_reaches_it_when_found_there_later() {
    // The end snapshot is still being taken, then the start snapshot finishes
    // last: in both, the check's evidence interval runs until its last
    // snapshot is taken, so work registered meanwhile, elsewhere, and found
    // in the check's worktree only after that, is placed under it.
    for (label, start_last) in [
        ("end-snapshot-pending", false),
        ("start-snapshot-last", true),
    ] {
        let (turn, _worktree) = ordinary_carried_turn(label);
        let (other, stopped) = other_claude_session_elsewhere(&turn, label);
        let pending = Arc::new(EngramBasisCapture::default());
        let check = if start_last {
            check_with_captures(&turn, pending.clone(), closed_capture())
        } else {
            check_with_captures(&turn, closed_capture(), pending.clone())
        };
        turn.record_mut(|record| record.engram.active_turn_checks = vec![check]);
        admit_background_call(&turn, &other, &stopped, "writer", "Bash");
        pending.finish(None);
        turn.record(|record| {
            let check = &record.engram.active_turn_checks[0];
            assert!(!check.open_to_writes(), "{label}: closed now");
            assert_eq!(check.fenced_by_outstanding, None, "{label}: elsewhere");
        });
        place_other_work_in(&turn, &other, &stopped, "writer", &turn.root);
        turn.record(|record| {
            assert_eq!(
                record.engram.active_turn_checks[0].fenced_by_outstanding,
                Some(ClaudeHazardCause::OtherSession {
                    session_id: other.clone(),
                    name: "Test".to_owned(),
                }),
                "{label}"
            );
        });
        assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    }
}
