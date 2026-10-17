// Explicit, one-time release measurement on a disposable metadata replica.
// Counts and catalog byte shapes come from the aggregate-only live read;
// strings and retained history are synthetic, never a live-store export.
use super::*;

const REPLICA_SESSIONS: usize = 5_696;
const REPLICA_DELEGATIONS: usize = 9_614;
const REPLICA_RUNS: usize = 3_577;

fn replica_options(count: usize, encoded_bytes: usize) -> Vec<SessionModelOption> {
    let mut options: Vec<_> = (0..count)
        .map(|index| SessionModelOption::plain("", index.to_string()))
        .collect();
    let current = serde_json::to_vec(&options).unwrap().len();
    assert!(current <= encoded_bytes);
    options[0].label = "x".repeat(encoded_bytes - current);
    assert_eq!(serde_json::to_vec(&options).unwrap().len(), encoded_bytes);
    options
}

fn populate_replica(fixture: &mut ObservationPublicationFixture) {
    // Produce a real observation as the typed history template, then retain
    // synthetic Unknown history instead of pretending to reproduce live work.
    let template_id = fixture.start("replica-template");
    fixture.flush();
    fixture.write_pending();
    fixture.flush();
    let mut inner = fixture.state.inner.lock().unwrap();
    let template = inner.test_run_observations.remove(&template_id).unwrap();
    inner.test_runs.observations_durable.remove(&template_id);
    let workdir = fixture.writer.root.path().join("project");
    let project = inner.sessions[0].session.project_id.clone();
    for (agent, count) in [
        (Agent::Codex, 3_068), // The fixture already owns one Codex session.
        (Agent::Claude, 2_495),
        (Agent::Cursor, 4),
        (Agent::Kimi, 128),
    ] {
        for index in 0..count {
            let created = inner.create_session(
                agent,
                Some(format!("Replica {index}")),
                workdir.to_string_lossy().into_owned(),
                project.clone(),
                None,
            );
            let position = inner.find_session_index(&created.session.id).unwrap();
            let session = &mut inner.sessions[position].session;
            session.model_options.clear();
            session.kimi_effort_options.clear();
            session.opencode_effort_options.clear();
            session.opencode_mode_options.clear();
            session.model_options = match agent {
                Agent::Claude if index < 3 => replica_options(13, 2_451),
                Agent::Claude if index == 3 => replica_options(13, 2_466),
                Agent::Cursor => replica_options(38, 2_810),
                Agent::Kimi => replica_options(4, 226),
                _ => Vec::new(),
            };
            if agent == Agent::Kimi {
                session.kimi_effort_options = replica_options(3, 120);
            }
        }
    }
    assert_eq!(inner.sessions.len(), REPLICA_SESSIONS);
    // Match aggregate preview bytes, including JSON quotes, without retaining
    // any operator text or claiming to reproduce its real length distribution.
    let preview_bytes = 3_145_530;
    for (index, record) in inner.sessions.iter_mut().enumerate() {
        let bytes = preview_bytes / REPLICA_SESSIONS
            + usize::from(index < preview_bytes % REPLICA_SESSIONS);
        record.session.preview = "p".repeat(bytes - 2);
    }
    for index in 0..REPLICA_RUNS {
        let mut row = template.clone();
        row.execution_id = format!("replica-observation-{index:05}");
        row.tool_identity = sha256_hex(row.execution_id.as_bytes());
        row.summary.run_id = row.execution_id.clone();
        row.summary.state = TestRunState::Unknown;
        row.summary.error = Some("Synthetic unresolved history".to_owned());
        inner
            .test_run_observations
            .insert(row.execution_id.clone(), row);
    }
    for index in 0..REPLICA_DELEGATIONS {
        let delegation: DelegationRecord = serde_json::from_value(json!({
            "id": format!("replica-delegation-{index:05}"),
            "parentSessionId": fixture.session,
            "childSessionId": inner.sessions[index % REPLICA_SESSIONS].session.id,
            "mode": "reviewer", "status": "completed", "title": "Synthetic history",
            "prompt": "", "cwd": test_run_display_path(&workdir), "agent": "Codex",
            "writePolicy": {"kind": "readOnly"}, "createdAt": "2026-10-17T03:15:41Z"
        }))
        .unwrap();
        inner.delegations.push(delegation);
        let position = inner.delegations.len() - 1;
        inner.mark_delegation_mutated(position);
    }
    drop(inner);
    let baseline = collect_persist_delta_from_shared_state(&fixture.state.inner, 0);
    fixture.writer.write(&baseline).unwrap();
    fixture.watermark = baseline.watermark;
}

#[test]
#[ignore = "one-time large replica measurement; run explicitly in a release build window"]
fn observation_replica_measurement() {
    assert!(
        !cfg!(debug_assertions),
        "replica measurement requires --release"
    );
    let mut fixture = ObservationPublicationFixture::new();
    populate_replica(&mut fixture);
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let report = diagnostics.clone();
    let saved = {
        let mut inner = fixture.state.inner.lock().unwrap();
        std::mem::replace(&mut *inner, StateInner::new())
    };
    fixture.state.inner = Arc::new(StateMutex::new_with_diagnostic_reporter(
        saved,
        Duration::ZERO,
        Arc::new(move |diagnostic| report.lock().unwrap().push(diagnostic)),
    ));
    fixture.writer.inner = fixture.state.inner.clone();

    let snapshot = fixture.state.summary_snapshot();
    assert_eq!(snapshot.sessions.len(), REPLICA_SESSIONS);
    assert_eq!(snapshot.delegations.len(), REPLICA_DELEGATIONS);
    assert_eq!(snapshot.test_runs.len(), REPLICA_RUNS);
    let serialization = std::time::Instant::now();
    let snapshot_bytes = serde_json::to_vec(&snapshot).unwrap().len();
    let serialization_ns = serialization.elapsed().as_nanos();
    drop(snapshot);
    let snapshot_timings = std::mem::take(&mut *diagnostics.lock().unwrap());

    let mut state_count = 0;
    let mut state_bytes = 0;
    let mut delta_count = 0;
    for number in 0..3 {
        let key = format!("replica-measured-{number}");
        fixture.start(&key);
        let mut events = fixture.flush();
        fixture.write_pending();
        events.extend(fixture.flush());
        fixture.end(&key);
        events.extend(fixture.flush());
        fixture.write_pending();
        events.extend(fixture.flush());
        for event in events {
            match event {
                SseStreamEvent::State(payload) => {
                    state_count += 1;
                    state_bytes += payload.len();
                }
                SseStreamEvent::Delta(_) => delta_count += 1,
                SseStreamEvent::Lagged => panic!("replica observation lost ordered events"),
            }
        }
    }
    let observation_timings = std::mem::take(&mut *diagnostics.lock().unwrap());
    let timings = |records: Vec<StateMutexDiagnostic>| -> Vec<Value> {
        records
            .into_iter()
            .filter_map(|record| match record {
                StateMutexDiagnostic::Held {
                    held, waited, file, line, ..
                } => Some(json!({
                    "file": file, "line": line,
                    "heldNs": held.as_nanos(), "waitedNs": waited.as_nanos()
                })),
                _ => None,
            })
            .collect()
    };
    println!("{}", json!({
        "measurement": "DISPOSABLE METADATA REPLICA, NOT LIVE HOST LOCK TIMING",
        "provenance": "Aggregate-only live read 2026-10-17T03:15:41.946Z, host8cfe02d2 revision21569291",
        "sessions": REPLICA_SESSIONS, "delegations": REPLICA_DELEGATIONS,
        "testRunsBefore": REPLICA_RUNS, "commands": 3, "observationCommits": 6,
        "snapshotBytes": snapshot_bytes, "serializationOffLockNs": serialization_ns,
        "observationStateEvents": state_count, "observationStateBytes": state_bytes,
        "observationDeltaEvents": delta_count,
        "snapshotLockSamples": timings(snapshot_timings),
        "observationAndWriterLockSamples": timings(observation_timings),
        "limits": "Synthetic catalog values match option counts and encoded bytes, not real labels/capabilities. Preview aggregate matches, distribution differs. Historical run/delegation counts match but synthetic content and statuses differ. No transcripts, live store, subprocess or machine-load claim. Zero-threshold reporter adds instrumentation overhead. Writer steps driven explicitly. Compare actual baseline and repaired product builds in granted slots."
    }));
}
