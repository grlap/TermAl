// A broad remote snapshot that changes nothing must not mark the mirrored
// proxy sessions as changed. A stamp sends a session to the next persist
// delta, and a session without a transcript baseline is then rewritten in
// full, so stamping every proxy on each resync turns one unchanged snapshot
// into a rewrite of every mirrored transcript. A real change must still be
// stamped and persisted.
use super::*;

const REMOTE_PROJECT: &str = "remote-project-1";
const REMOTE_ROOT: &str = "/remote/repo";

fn mirrored_remote(revision: u64) -> (AppState, RemoteConfig, StateResponse) {
    mirrored_remote_from(revision, |_| {})
}

/// Mirrors the sample remote after `baseline` adjusts its snapshot, and
/// returns the snapshot as applied.
fn mirrored_remote_from(
    revision: u64,
    baseline: impl FnOnce(&mut StateResponse),
) -> (AppState, RemoteConfig, StateResponse) {
    let state = test_app_state();
    let remote = super::remote_delta_replay::local_replay_test_remote();
    create_test_remote_project(
        &state,
        &remote,
        REMOTE_ROOT,
        "Remote Project",
        REMOTE_PROJECT,
    );
    let mut snapshot = sample_remote_orchestrator_state(
        REMOTE_PROJECT,
        REMOTE_ROOT,
        revision,
        OrchestratorInstanceStatus::Running,
    )
    .into_state_response();
    baseline(&mut snapshot);
    apply_snapshot(
        &state,
        &remote,
        &snapshot,
        RemoteSnapshotApplyMode::GateBySnapshotRevision,
    );
    (state, remote, snapshot)
}

fn apply_snapshot(
    state: &AppState,
    remote: &RemoteConfig,
    snapshot: &StateResponse,
    mode: RemoteSnapshotApplyMode,
) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        apply_remote_state_if_newer_locked(
            &mut inner,
            &remote.id,
            snapshot,
            None,
            mode,
            &EngramRemoteWriterWorkdirs::default(),
        ),
        "the snapshot is applied"
    );
    note_remote_applied_state_snapshot_revision(&mut inner, &remote.id, snapshot);
}

/// Each mirrored proxy's local id and mutation stamp, and the watermark a
/// persist tick would start from now.
fn proxy_stamps(state: &AppState, remote: &RemoteConfig) -> (Vec<(String, u64)>, u64) {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let stamps = inner
        .sessions
        .iter()
        .filter(|record| record.remote_id.as_deref() == Some(remote.id.as_str()))
        .map(|record| (record.session.id.clone(), record.mutation_stamp))
        .collect::<Vec<_>>();
    (stamps, inner.last_mutation_stamp)
}

/// The sessions the next persist tick would write after `watermark`.
fn persist_selected(state: &AppState, watermark: u64) -> Vec<String> {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    inner
        .collect_persist_delta_plan(watermark)
        .changed_sessions
        .into_iter()
        .map(|candidate| candidate.session_id)
        .collect()
}

fn local_proxy_id(state: &AppState, remote: &RemoteConfig, remote_session_id: &str) -> String {
    let inner = state.inner.lock().expect("state mutex poisoned");
    inner
        .sessions
        .iter()
        .find(|record| {
            record.remote_id.as_deref() == Some(remote.id.as_str())
                && record.remote_session_id.as_deref() == Some(remote_session_id)
        })
        .map(|record| record.session.id.clone())
        .expect("the remote session is mirrored")
}

#[test]
fn an_unchanged_broad_snapshot_does_not_restamp_mirrored_proxies() {
    // Re-applied after a lagged marker (same revision) and on a resync at a
    // newer revision whose sessions are all unchanged.
    for (label, mode, next_revision) in [
        (
            "re-applied after lagged",
            RemoteSnapshotApplyMode::ForceAfterLaggedEvent,
            2,
        ),
        (
            "resync at a newer revision",
            RemoteSnapshotApplyMode::GateBySnapshotRevision,
            3,
        ),
    ] {
        let (state, remote, snapshot) = mirrored_remote(2);
        let (before, watermark) = proxy_stamps(&state, &remote);
        assert!(!before.is_empty(), "the remote has mirrored proxies");

        let mut again = snapshot.clone();
        again.revision = next_revision;
        apply_snapshot(&state, &remote, &again, mode);

        let (after, _) = proxy_stamps(&state, &remote);
        assert_eq!(
            after, before,
            "{label}: no proxy is re-stamped by an unchanged snapshot"
        );
        assert_eq!(
            persist_selected(&state, watermark),
            Vec::<String>::new(),
            "{label}: the next persist tick writes no mirrored session"
        );
        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

#[test]
fn a_changed_proxy_in_a_broad_snapshot_is_stamped_and_persisted() {
    type Adjust = fn(&mut StateSessionSummary);
    fn unchanged(_: &mut StateSessionSummary) {}
    // Live activity is part of the broadcast summary only while the session
    // is active, and persistence drops it, so this case starts active.
    fn active(summary: &mut StateSessionSummary) {
        summary.status = SessionStatus::Active;
        summary.live_activity = None;
    }
    // A body sequence and its epoch only exist as a pair (a lone half is
    // normalized away), so the transcript-identity cases start sequenced.
    fn sequenced(summary: &mut StateSessionSummary) {
        summary.body_seq = Some(1);
        summary.body_seq_epoch = Some("epoch-a".to_owned());
    }
    // The broadcast summary bounds live activity (whitespace collapsed,
    // length cut), so this pair looks identical there; only the full value
    // differs.
    fn spaced_activity(summary: &mut StateSessionSummary) {
        active(summary);
        summary.live_activity = Some(SessionLiveActivity {
            prompt: "run   the   tests".to_owned(),
            command: None,
            command_status: None,
        });
    }
    let changes: [(&str, Adjust, Adjust); 7] = [
        ("metadata only", unchanged, |summary| {
            summary.preview = format!("{} (changed)", summary.preview);
        }),
        ("transcript append", sequenced, |summary| {
            summary.message_count += 1;
            summary.body_seq = Some(2);
        }),
        ("same-length transcript change", sequenced, |summary| {
            summary.body_seq = Some(2);
        }),
        ("epoch change", sequenced, |summary| {
            summary.body_seq_epoch = Some("epoch-b".to_owned());
        }),
        ("remote stamp only", unchanged, |summary| {
            summary.session_mutation_stamp = Some(summary.session_mutation_stamp.unwrap_or(0) + 7);
        }),
        ("live activity only", active, |summary| {
            summary.live_activity = Some(SessionLiveActivity {
                prompt: "Running the focused tests".to_owned(),
                command: None,
                command_status: None,
            });
        }),
        (
            "live activity hidden by the bounded summary",
            spaced_activity,
            |summary| {
                summary.live_activity = Some(SessionLiveActivity {
                    prompt: "run the tests".to_owned(),
                    command: None,
                    command_status: None,
                });
            },
        ),
    ];
    // Every case runs, so one failing case cannot hide the others.
    let mut missed = Vec::new();
    for (label, baseline, change) in changes {
        let (state, remote, snapshot) =
            mirrored_remote_from(2, |snapshot| baseline(&mut snapshot.sessions[0]));
        let (before, watermark) = proxy_stamps(&state, &remote);

        let mut next = snapshot.clone();
        next.revision = 3;
        let changed_remote_id = next.sessions[0].id.clone();
        change(&mut next.sessions[0]);
        apply_snapshot(
            &state,
            &remote,
            &next,
            RemoteSnapshotApplyMode::GateBySnapshotRevision,
        );

        let changed = local_proxy_id(&state, &remote, &changed_remote_id);
        let (after, _) = proxy_stamps(&state, &remote);
        let stamp_of = |stamps: &[(String, u64)]| {
            stamps
                .iter()
                .find(|(id, _)| *id == changed)
                .map(|(_, stamp)| *stamp)
        };
        if stamp_of(&after) <= stamp_of(&before) {
            missed.push(format!("{label}: the changed proxy is not stamped"));
        }
        if !persist_selected(&state, watermark).contains(&changed) {
            missed.push(format!("{label}: the next persist tick does not write it"));
        }
        let _ = fs::remove_file(state.persistence_path.as_path());
    }
    assert!(missed.is_empty(), "{missed:#?}");
}

#[test]
fn an_unchanged_snapshot_keeps_a_loaded_transcript_and_stamps_nothing() {
    let state = test_app_state();
    let remote = super::remote_delta_replay::local_replay_test_remote();
    create_test_remote_project(
        &state,
        &remote,
        REMOTE_ROOT,
        "Remote Project",
        REMOTE_PROJECT,
    );
    let mut sample = sample_remote_orchestrator_state(
        REMOTE_PROJECT,
        REMOTE_ROOT,
        2,
        OrchestratorInstanceStatus::Running,
    );
    sample.sessions[0].messages = (vec![super::remote::remote_text_message(
        "message-1",
        "First reply",
    )])
    .into();
    sample.sessions[0].message_count = 1;
    let snapshot = sample.as_state_response();
    apply_snapshot(
        &state,
        &remote,
        &snapshot,
        RemoteSnapshotApplyMode::GateBySnapshotRevision,
    );
    super::remote::materialize_remote_proxy_session_transcript_for_test(
        &state,
        &remote,
        &sample.sessions[0],
    );
    let loaded = local_proxy_id(&state, &remote, &sample.sessions[0].id);
    let retained = |state: &AppState| {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .sessions
            .iter()
            .find(|record| record.session.id == loaded)
            .map(|record| record.session.messages.len())
            .expect("the loaded proxy exists")
    };
    assert_eq!(retained(&state), 1, "the transcript is loaded");
    let (before, watermark) = proxy_stamps(&state, &remote);

    let mut again = snapshot.clone();
    again.revision = 3;
    apply_snapshot(
        &state,
        &remote,
        &again,
        RemoteSnapshotApplyMode::GateBySnapshotRevision,
    );

    let (after, _) = proxy_stamps(&state, &remote);
    assert_eq!(after, before, "an unchanged resync stamps no proxy");
    assert_eq!(
        retained(&state),
        1,
        "the loaded transcript is kept across the unchanged resync"
    );
    assert_eq!(
        persist_selected(&state, watermark),
        Vec::<String>::new(),
        "the next persist tick writes no mirrored session"
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}
