// Owns agent-facing mailbox protocol negotiation and central outage witnesses.
// Does not provision links or implement sender intents/wake recovery. New M2
// integration coverage mounted from tests/mod.rs, using disposable host state.
use super::*;

async fn protocol_response(state: AppState, session: &str) -> Response {
    app_router(state)
        .oneshot(
            Request::builder()
                .uri(format!("/api/sessions/{session}/mailboxes/protocol"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn central_negotiation_reports_authority_schema_and_unchanged_mailbox_limits() {
    let state = federation_registry::central();
    let session = test_session_id(&state, Agent::Codex);
    let central = state
        .inner
        .lock()
        .unwrap()
        .federation
        .local
        .clone()
        .unwrap();
    let response = protocol_response(state, &session).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 16384).await.unwrap()).unwrap();
    assert_eq!(body["protocolVersion"], 1);
    assert_eq!(body["schemaVersion"], "1");
    assert_eq!(body["central"], serde_json::to_value(central).unwrap());
    assert_eq!(body["limits"]["bodyBytes"], 262144);
    assert_eq!(body["limits"]["metadataBytes"], 4096);
    assert_eq!(body["limits"]["displayNameChars"], 160);
    assert_eq!(body["receiptOnlyAcknowledgement"], true);
    assert_eq!(body["senderIntentRegistry"], false);
    assert_eq!(body["remoteWakeDelivery"], false);
}

#[tokio::test]
async fn satellite_negotiation_fails_retryably_instead_of_claiming_local_authority() {
    let state = federation_registry::domain_state();
    let session = test_session_id(&state, Agent::Codex);
    let central = federation_registry::peer(48001).identity;
    state
        .configure_federation(FederationSettings {
            enabled: true,
            role: FederationRole::Satellite,
            central: Some(central),
        })
        .unwrap();
    let before = federation_persist_image(&PersistedState::metadata_from_inner(
        &state.inner.lock().unwrap(),
    ));
    let response = protocol_response(state.clone(), &session).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        federation_persist_image(&PersistedState::metadata_from_inner(
            &state.inner.lock().unwrap()
        )),
        before
    );
}

#[tokio::test]
async fn disabled_host_does_not_advertise_domain_mailbox_authority() {
    let state = federation_registry::domain_state();
    let session = test_session_id(&state, Agent::Codex);
    assert_eq!(
        protocol_response(state, &session).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn central_negotiation_refuses_a_delegation_child() {
    let state = federation_registry::central();
    let session = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].session.parent_delegation_id = Some("delegation-child".to_owned());
    }
    assert_eq!(
        protocol_response(state, &session).await.status(),
        StatusCode::BAD_REQUEST
    );
}
