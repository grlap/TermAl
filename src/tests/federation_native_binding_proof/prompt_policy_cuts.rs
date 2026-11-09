// Owns cross-peer revocation and proof-before-RPC refusal cuts. These fixtures
// use real channel ownership but never send a text prompt or start a runtime.
use super::*;

#[tokio::test]
async fn federation_prompt_policy_revocation_cannot_borrow_another_enrolled_peers_authenticated_link(
) {
    let mut fixture = policy_fixture(true).await;
    let scope = scope(&fixture, true);
    let consent = consent(&fixture, &scope);
    let other = federation_registry::peer(48921);
    fixture
        .central
        .enroll_federation_peer(other.clone())
        .unwrap();
    let mut foreign = consent;
    foreign.issuer = other.identity.clone();
    foreign.scope.target_host = other.identity.clone();
    foreign.scope.target.host_id = other.identity.host_id;
    foreign.scope.target.creation_id = Uuid::new_v4().to_string();
    foreign.scope.placement_generation = None;
    foreign.identity.id = Uuid::new_v4().to_string();
    foreign.identity.generation = 2;
    foreign.revoked = true;
    foreign.creation_key = "foreign-consent".into();
    foreign.validate().unwrap();
    let before = version(&fixture.central);
    // The captured channel is genuinely peer A. Merely enrolling B and naming
    // B as issuer in a valid record must not authorize A to revoke B's policy.
    let result = fixture.central.observe_federation_prompt_revocation(
        &fixture.channel.current_lease().unwrap(),
        foreign.clone(),
    );
    assert!(result.is_err());
    assert_eq!(version(&fixture.central), before);
    assert!(fixture
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .prompt_permissions
        .as_ref()
        .is_none_or(|policy| policy.find(&foreign.identity.id).is_none()));
    fixture.server.finish().await;
}

#[tokio::test]
async fn federation_prompt_policy_expired_or_foreign_proof_refuses_before_rpc_counter_or_policy_mutation(
) {
    let mut fixture = policy_fixture(true).await;
    let scope = scope(&fixture, true);
    let consent = consent(&fixture, &scope);
    let source_lease = fixture.channel.current_lease().unwrap();
    let grant = fixture
        .central
        .issue_federation_prompt_grant_on_link(
            version(&fixture.central),
            "grant".into(),
            scope.clone(),
            consent.identity,
            &source_lease,
            &fixture.rpc,
        )
        .await
        .unwrap();
    let request = remote_prompt_protocol::OwnPrompt {
        reference: Uuid::parse_str(&grant.identity.id).unwrap(),
        idempotency_key: "never-enter".into(),
        text: "exact body".into(),
    };
    let intent = federation_prompt_intent_value(&grant, &request).unwrap();
    let receiver_lease = fixture.satellite_channel.current_lease().unwrap();
    let check = fixture.satellite_channel.clone();
    let unavailable_rpc = FederationRpcClient::new(
        9,
        fixture.satellite_channel.clone(),
        Arc::new(move || check.current_authority().map(|_| ())),
    )
    .unwrap();
    let security = fixture.satellite_channel.security_context().unwrap();
    let before_counter = security.sender.lock().unwrap().next;
    let before_version = version(&fixture.satellite);
    let stale = remote_prompt_protocol::UnverifiedProof::new(
        Uuid::parse_str(&receiver_lease.incarnation).unwrap(),
        Uuid::new_v4(),
        1,
    )
    .unwrap();
    let foreign = remote_prompt_protocol::UnverifiedProof::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        federation_prompt_now().unwrap() + 30,
    )
    .unwrap();
    for proof in [stale, foreign] {
        assert!(fixture
            .satellite
            .observe_remote_prompt_grant(&receiver_lease, &unavailable_rpc, &intent, &proof)
            .await
            .is_err());
        assert_eq!(
            security.sender.lock().unwrap().next,
            before_counter,
            "local proof refusal precedes transport counter allocation"
        );
        assert_eq!(version(&fixture.satellite), before_version);
    }
    assert!(fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .federation
        .prompt_permissions
        .as_ref()
        .unwrap()
        .find(&grant.identity.id)
        .is_none());
    fixture.server.finish().await;
}
