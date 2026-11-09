// Local operator HTTP and caller-bound reference-view controls. Uses the same
// real isolated federation writer/MAC fixture, not a privileged live host.
use super::*;

#[tokio::test]
async fn federation_prompt_operator_routes_issue_content_fenced_records_and_derive_references() {
    let mut fixture = policy_fixture(true).await;
    let scope = scope(&fixture, true);
    let mut satellite_api = DomainServer::start(federation_prompt_operator_routes().with_state(fixture.satellite.clone())).await;
    let mut central_api = DomainServer::start(federation_prompt_operator_routes().with_state(fixture.central.clone())).await;
    fixture
        .central
        .federation_peer_handles
        .lock()
        .unwrap()
        .insert(
            scope.target_host.host_id.clone(),
            Arc::new(FederationPeerHandle {
                channel: fixture.channel.clone(),
                rpc: fixture.rpc.clone(),
            }),
        );
    let response=fixture.client.post(format!("http://127.0.0.1:{}/api/federation/prompt-consents",satellite_api.port))
        .json(&json!({"expectedVersion":version(&fixture.satellite),"creationKey":"consent-api","scope":scope}))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let consent: FederationPromptPermission = response.json().await.unwrap();
    let issue_url = format!(
        "http://127.0.0.1:{}/api/federation/prompt-grants",
        central_api.port
    );
    let request = json!({"expectedVersion":version(&fixture.central),"creationKey":"grant-api","scope":scope,"consent":consent.identity});
    let stale=fixture.client.post(&issue_url).json(&json!({"expectedVersion":0,"creationKey":"stale","scope":scope,"consent":consent.identity})).send().await.unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let response = fixture
        .client
        .post(&issue_url)
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response: Value = response.json().await.unwrap();
    let grant: FederationPromptPermission =
        serde_json::from_value(response["permission"].clone()).unwrap();
    let locator = response["targetReference"].as_str().unwrap();
    let own = remote_prompt_protocol::decode_own(
        &serde_json::to_vec(
            &json!({"targetReference":locator,"idempotencyKey":"one","text":"body"}),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        own.reference.to_string(),
        grant.identity.id,
        "the locator is the immutable grant UUID, with no provision table"
    );
    let before = version(&fixture.central);
    let references = fixture
        .central
        .remote_prompt_references_for_caller(&scope.source.session_id, None)
        .unwrap();
    assert_eq!(references["references"][0]["targetReference"], locator);
    assert_eq!(
        references["references"][0]["target"],
        serde_json::to_value(&scope.target).unwrap()
    );
    assert_eq!(
        version(&fixture.central),
        before,
        "reference reads perform no mutation/proof refresh"
    );
    let other = test_session_id(&fixture.central, Agent::Codex);
    assert_eq!(
        fixture
            .central
            .remote_prompt_references_for_caller(&other, None)
            .unwrap()["references"],
        json!([])
    );
    assert!(fixture
        .central
        .remote_prompt_references_for_caller(&other, Some(&grant.identity.id))
        .is_err());
    let old_provision = fixture
        .client
        .post(format!(
            "http://127.0.0.1:{}/api/sessions/{}/remote-prompt-references",
            central_api.port, scope.source.session_id
        ))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(old_provision.status(), StatusCode::METHOD_NOT_ALLOWED);
    let response = fixture
        .client
        .post(format!(
            "http://127.0.0.1:{}/api/federation/prompt-permissions/{}/revoke",
            central_api.port, grant.identity.id
        ))
        .json(&json!({"expectedVersion":version(&fixture.central)}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let revoked: FederationPromptPermission = response.json().await.unwrap();
    assert!(revoked.revoked && revoked.receiver_observed_generation.is_none());
    assert_eq!(
        fixture
            .central
            .remote_prompt_references_for_caller(&scope.source.session_id, None)
            .unwrap()["references"],
        json!([])
    );
    let repeat = fixture
        .central
        .issue_federation_prompt_grant_on_link(
            version(&fixture.central),
            "grant-api".into(),
            scope.clone(),
            consent.identity.clone(),
            &fixture.channel.current_lease().unwrap(),
            &fixture.rpc,
        )
        .await
        .unwrap();
    assert_eq!(
        repeat, revoked,
        "same issuance key never revives a revoked identity"
    );
    let regrant = fixture
        .central
        .issue_federation_prompt_grant_on_link(
            version(&fixture.central),
            "new-operator-grant".into(),
            scope,
            consent.identity,
            &fixture.channel.current_lease().unwrap(),
            &fixture.rpc,
        )
        .await
        .unwrap();
    assert_ne!(regrant.identity.id, grant.identity.id);
    assert_eq!(regrant.identity.generation, 1);
    central_api.finish().await;
    satellite_api.finish().await;
    fixture.server.finish().await;
}

#[tokio::test]
async fn federation_prompt_operator_strict_metadata_refuses_unknown_duplicate_and_oversized_requests(
) {
    let mut fixture = policy_fixture(false).await;
    let scope = scope(&fixture, false);
    let mut api = DomainServer::start(federation_prompt_operator_routes().with_state(fixture.satellite.clone())).await;
    let url = format!(
        "http://127.0.0.1:{}/api/federation/prompt-consents",
        api.port
    );
    let current = version(&fixture.satellite);
    let request = json!({"expectedVersion":current,"creationKey":"strict","scope":scope});
    let mut extra = request.clone();
    extra["callerSession"] = json!("impersonated");
    let duplicate = serde_json::to_string(&request).unwrap().replacen(
        "{",
        &format!("{{\"expectedVersion\":{current},"),
        1,
    );
    for body in [
        serde_json::to_vec(&extra).unwrap(),
        duplicate.into_bytes(),
        vec![b' '; 32_769],
        vec![0xff],
    ] {
        let response = fixture
            .client
            .post(&url)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert!(matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::PAYLOAD_TOO_LARGE
        ));
    }
    assert_eq!(version(&fixture.satellite), current);
    assert!(fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .federation
        .prompt_permissions
        .is_none());
    api.finish().await;
    fixture.server.finish().await;
}
