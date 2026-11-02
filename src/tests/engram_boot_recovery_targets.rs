//! Ordinary boot retains delegation history without reconstructing a control
//! recovery executor. Real SQLite reloads preserve terminal child statuses and
//! every historical token/grant/unknown field; a fresh parent prompt still runs.
use super::super::delegation_support::{
    boot_on_delegation_root, install_delegation_codex_runtime, temp_delegation_state_paths,
};
use super::*;

fn boot_history_fixture(
    root: &Arc<TestTempRoot>,
    paths: &(PathBuf, PathBuf, PathBuf),
    transport: Arc<ScriptedEngramControlTransport>,
) -> AppState {
    boot_on_delegation_root(root, ||
        AppState::new_with_paths_engram_transport_and_clock_for_test(
            paths.0.to_string_lossy().into_owned(), paths.1.clone(), paths.2.clone(),
            transport, EngramBudgetClock::scripted(),
        ).expect("ordinary history fixture should boot")
    )
}

fn assert_boot_history(
    state: &AppState,
    histories: &BTreeMap<String, BTreeMap<String, Value>>,
) {
    let connection = open_sqlite_state_read_connection(&state.persistence_path).unwrap();
    let inner = state.inner.lock().unwrap();
    for (session, expected) in histories {
        let record = &inner.sessions[inner.find_session_index(session).unwrap()];
        assert_eq!(&record.engram.historical_control, expected);
        let raw: String = connection.query_row(
            "SELECT value_json FROM sessions WHERE id = ?1", rusqlite::params![session],
            |row| row.get(0),
        ).unwrap();
        let saved: Value = serde_json::from_str(&raw).unwrap();
        for (key, value) in expected {
            assert_eq!(saved.get(key), Some(value), "{session}: boot preserves {key}");
        }
    }
}

#[test]
fn ordinary_boot_keeps_finished_child_and_uncertain_history_without_replaying_it() {
    let (temp_root, project_root, persistence_path, templates_path) = temp_delegation_state_paths();
    let cleaned = temp_root.observe_cleanup();
    let paths = (project_root, persistence_path, templates_path);
    let transport = ScriptedEngramControlTransport::new([]);
    let state = boot_history_fixture(&temp_root, &paths, transport.clone());
    install_delegation_codex_runtime(&state, "ordinary-boot-history-children");
    let project = create_test_project(&state, &paths.0, "Ordinary boot history");
    fs::write(paths.0.join(".engram-project"), format!("{project}\n")).unwrap();
    let parents: Vec<_> = (0..7_usize.div_ceil(MAX_RUNNING_DELEGATIONS_PER_PARENT))
        .map(|_| create_test_project_session(&state, Agent::Codex, &project, &paths.0))
        .collect();
    let statuses = [
        DelegationStatus::Completed, DelegationStatus::Completed, DelegationStatus::Failed,
        DelegationStatus::Canceled, DelegationStatus::Canceled, DelegationStatus::Canceled,
        DelegationStatus::Running,
    ];
    let children: Vec<_> = (0..statuses.len()).map(|index| {
        state.create_read_only_delegation(&parents[index / MAX_RUNNING_DELEGATIONS_PER_PARENT],
            CreateDelegationRequest {
                prompt: format!("Keep child history {index}."), title: None, cwd: None,
                agent: Some(Agent::Codex), model: None, mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            }).unwrap().delegation.child_session_id
    }).collect();
    let mut histories = BTreeMap::new();
    {
        let mut inner = state.inner.lock().unwrap();
        inner.projects.iter_mut().find(|entry| entry.id == project).unwrap().engram =
            Some(EngramProjectSettings {
                enabled: true, execution_observability: false,
                home: Some(paths.0.to_string_lossy().into_owned()),
                binary_path: Some(paths.0.join("engram-fixture").to_string_lossy().into_owned()),
                ..EngramProjectSettings::default()
            });
        for (child, status) in children.iter().zip(statuses) {
            let index = inner.delegations.iter().position(|row| &row.child_session_id == child).unwrap();
            inner.delegations[index].status = status;
            inner.mark_delegation_mutated(index).unwrap();
        }
        for (index, session) in parents.iter().chain(children.iter()).enumerate() {
            let history = BTreeMap::from([
                ("engramRoutingToken".to_owned(), json!(format!("historical-{session}"))),
                ("engramRebindRequired".to_owned(), json!(true)),
                ("engramBeginsRecorded".to_owned(), json!(false)),
                ("engramOpenGrantId".to_owned(), if index % 2 == 0 { json!(format!("old-grant-{index}")) } else { Value::Null }),
                ("engramUncertainGrantId".to_owned(), if index % 3 == 0 { json!("possibly-sent-before-boot") } else { Value::Null }),
                ("engramUnknownBootPayload".to_owned(), json!({"original":[null,"żółw 🐢",{"n":7}]})),
            ]);
            inner.session_mut(session).unwrap().engram.historical_control = history.clone();
            histories.insert(session.clone(), history);
        }
        state.commit_locked(&mut inner).unwrap();
    }
    state.shutdown_persist_blocking();
    assert_boot_history(&state, &histories);
    drop(state);

    // Two real boots must keep even the uncertain/unrecorded originals; no
    // clean transport answer is permission to settle or replace that history.
    for _ in 0..2 {
        let loaded = boot_history_fixture(&temp_root, &paths, transport.clone());
        assert_boot_history(&loaded, &histories);
        {
            let inner = loaded.inner.lock().unwrap();
            for (child, status) in children.iter().zip(statuses).filter(|(_, status)|
                *status != DelegationStatus::Running)
            {
                assert_eq!(inner.delegations.iter().find(|row| &row.child_session_id == child).unwrap().status, status);
            }
        }
        loaded.shutdown_persist_blocking();
        drop(loaded);
    }

    let loaded = boot_history_fixture(&temp_root, &paths, transport.clone());
    let (runtime, prompts, _process) = test_shared_codex_runtime("ordinary-boot-history-followup");
    install_single_wire_codex_fixture(&loaded, Some(runtime));
    let session = &parents[0];
    let dispatch = match loaded.dispatch_turn(session, SendMessageRequest {
        text: "Distinct ordinary input after history reload.".to_owned(), expanded_text: None,
        attachments: Vec::new(), source_session_id: None, source_mailbox: None,
    }).unwrap() {
        DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("passive history cannot hold a fresh ordinary prompt"),
    };
    let token = dispatch.runtime_token().clone();
    let generation = dispatch.active_turn_generation();
    assert!(matches!(deliver_turn_dispatch(&loaded, dispatch), TurnDispatchDeliveryOutcome::Delivered));
    loop {
        if let CodexRuntimeCommand::Prompt { session_id, command } =
            phase_sync::receive(&prompts, "ordinary prompt after real boot")
        {
            assert_eq!(&session_id, session);
            assert!(command.prompt.contains("Distinct ordinary input after history reload."));
            break;
        }
    }
    loaded.finish_turn_ok_if_runtime_and_generation_match(session, &token, generation).unwrap();
    loaded.shutdown_persist_blocking();
    assert_boot_history(&loaded, &histories);
    for request in transport.requests() {
        assert!(!matches!(request.request["operation"].as_str(),
            Some("session_bind" | "session_status" | "turn_evaluate" | "turn_begin"
                | "turn_checkpoint" | "turn_grant_read" | "verification_record" | "verification_bind")),
            "passive boot history cannot create a control or verification request");
    }
    drop(loaded);
    drop(temp_root);
    phase_sync::receive(&cleaned, "last ordinary boot owner removes the fixture root").unwrap();
}
