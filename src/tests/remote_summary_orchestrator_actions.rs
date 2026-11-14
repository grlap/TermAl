// Exercises nested snapshot compatibility after a remote orchestrator POST.
use super::*;

fn orchestrator_action(fields: Value, should_apply: bool) {
    let (state, remote, mut snapshot) = fixture();
    let project = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .projects
            .iter()
            .find(|project| project.remote_id == remote.id)
            .cloned()
            .expect("remote project should exist")
    };
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(project.id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;
    let mut remote_orchestrator = snapshot.orchestrators[0].clone();
    remote_orchestrator.id = "created-old-peer-orchestrator".to_owned();
    remote_orchestrator.template_id = template.id.clone();
    remote_orchestrator.template_snapshot = OrchestratorTemplate {
        project_id: Some("remote-project-1".to_owned()),
        ..template.clone()
    };
    snapshot.orchestrators[0] = remote_orchestrator.clone();
    let response = json!({
        "orchestrator": remote_orchestrator,
        "state": payload(&snapshot, "state", &fields)
    });
    let (port, requests, server) = action_server(response, "POST /api/orchestrators ");
    insert_test_remote_connection(&state, &remote, port, TestRemoteBridgeOwnership::Claimed);

    let result = state.create_remote_orchestrator_proxy(&template, &project);
    join_test_server(server);
    let captured = requests.lock().expect("requests mutex poisoned");
    assert_eq!(captured.len(), 2, "one health probe and exactly one create");
    assert_eq!(captured[1].0, "POST /api/orchestrators HTTP/1.1");
    let sent: Value = serde_json::from_str(&captured[1].1).expect("create body should decode");
    assert_eq!(sent["projectId"], "remote-project-1");
    assert_eq!(sent["templateId"], template.id);
    drop(captured);

    if should_apply {
        let response = result.expect("successful remote orchestrator creation must decode");
        assert_eq!(
            response.orchestrator.remote_id.as_deref(),
            Some(remote.id.as_str())
        );
        assert_eq!(
            response.orchestrator.remote_orchestrator_id.as_deref(),
            Some("created-old-peer-orchestrator")
        );
        assert_eq!(response.orchestrator.project_id, project.id);
        assert!(
            response
                .state
                .orchestrators
                .iter()
                .any(|instance| instance.id == response.orchestrator.id)
        );
        assert_applied(&state, &remote, snapshot.sessions.len());
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(
            inner
                .find_remote_orchestrator_index(&remote.id, "created-old-peer-orchestrator")
                .is_some()
        );
    } else {
        let error = match result {
            Ok(_) => panic!("unknown summary field beside retired fields must refuse"),
            Err(error) => error,
        };
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert!(
            error.message.contains("futureSummaryField"),
            "expected unknown live summary field refusal, got: {}",
            error.message
        );
        assert_unapplied(&state, &remote);
    }
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_orchestrator_creation_accepts_retired_nested_state_without_retry() {
    orchestrator_action(retired_fields().pop().unwrap(), true);
}

#[test]
fn remote_orchestrator_creation_refuses_genuinely_unknown_summary_field() {
    orchestrator_action(json!({"futureSummaryField": true}), false);
}

#[test]
fn remote_orchestrator_creation_refuses_unknown_field_beside_retired_fields() {
    let mut fields = retired_fields().pop().unwrap();
    fields["futureSummaryField"] = json!(true);
    orchestrator_action(fields, false);
}
