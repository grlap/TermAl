// Owns requester-monotonic versus issuer-wall-clock freshness controls.
// A real authenticated reply supplies policy; scripted epochs model separate
// clocks in the pure post-authentication validation, not live clock changes.
use super::*;

#[tokio::test]
async fn federation_prompt_policy_reply_accepts_both_clock_skew_directions_without_extending_lifetime()
 {
    let mut fixture = policy_fixture(false).await;
    let scope = scope(&fixture, false);
    let consent = consent(&fixture, &scope);
    let lease = fixture.channel.current_lease().unwrap();
    let reply = fixture
        .central
        .read_federation_prompt_policy(
            &fixture.rpc,
            &lease,
            FederationPromptPolicyOperation::DestinationProof {
                scope,
                consent: consent.identity,
            },
        )
        .await
        .unwrap();
    let sent = std::time::Instant::now();
    let deadline = sent + FEDERATION_PROMPT_OBSERVATION_LIFETIME;
    // Both roles issue replies. Neither requester may compare the peer epoch
    // with its own, including a peer much farther away than the proof lifetime.
    let requester_now = federation_prompt_now().unwrap();
    for issuer_now in [
        requester_now - 86_400,
        requester_now - 1,
        requester_now + 1,
        requester_now + 86_400,
    ] {
        let mut scripted = reply.clone();
        scripted.expires_at = issuer_now + 30;
        let bytes = serde_json::to_vec(&scripted).unwrap();
        let decoded: FederationPromptPolicyReply = serde_json::from_slice(&bytes).unwrap();
        assert!(
            decoded.observed_until.is_none(),
            "wire carries no local deadline"
        );
        let observed = decoded
            .observe(
                &reply.request_id,
                &lease.incarnation,
                deadline,
                sent + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(observed.expires_at, issuer_now + 30);
        observed
            .ensure_fresh_at(sent + Duration::from_secs(29))
            .unwrap();
        assert!(observed.ensure_fresh_at(deadline).is_err());
        assert!(
            observed
                .ensure_fresh_at(deadline + Duration::from_secs(1))
                .is_err()
        );
    }
    fixture.server.finish().await;
}

#[tokio::test]
async fn federation_prompt_policy_late_or_uncorrelated_reply_cannot_gain_a_local_deadline() {
    let mut fixture = policy_fixture(false).await;
    let scope = scope(&fixture, false);
    let consent = consent(&fixture, &scope);
    let lease = fixture.channel.current_lease().unwrap();
    let reply = fixture
        .central
        .read_federation_prompt_policy(
            &fixture.rpc,
            &lease,
            FederationPromptPolicyOperation::DestinationProof {
                scope,
                consent: consent.identity,
            },
        )
        .await
        .unwrap();
    let sent = std::time::Instant::now();
    let deadline = sent + FEDERATION_PROMPT_OBSERVATION_LIFETIME;
    let mut far_future = reply.clone();
    far_future.expires_at = u64::MAX;
    assert!(
        far_future
            .observe(&reply.request_id, &lease.incarnation, deadline, deadline)
            .is_err(),
        "a remote future timestamp cannot renew a timed-out request"
    );
    assert!(
        reply
            .clone()
            .observe(
                &Uuid::new_v4().to_string(),
                &lease.incarnation,
                deadline,
                sent
            )
            .is_err()
    );
    assert!(
        reply
            .clone()
            .observe(
                &reply.request_id,
                &Uuid::new_v4().to_string(),
                deadline,
                sent
            )
            .is_err()
    );
    let mut zero = reply.clone();
    zero.expires_at = 0;
    assert!(
        zero.observe(&reply.request_id, &lease.incarnation, deadline, sent)
            .is_err()
    );
    let mut wire = serde_json::to_value(&reply).unwrap();
    assert!(wire.get("observedUntil").is_none());
    wire["observedUntil"] = json!(999);
    assert!(
        serde_json::from_value::<FederationPromptPolicyReply>(wire).is_err(),
        "the peer cannot supply requester-local timing"
    );
    fixture.server.finish().await;
}

#[tokio::test]
async fn federation_prompt_policy_envelope_expiry_uses_only_its_issuing_hosts_clock() {
    let mut fixture = policy_fixture(false).await;
    let lease = fixture.satellite_channel.current_lease().unwrap();
    // This envelope returns to the satellite that issued it. A different
    // central epoch has no role in this check; consent/grant are still re-read.
    for issuer_now in [1_000_000u64, 1_086_400u64] {
        let proof = remote_prompt_protocol::UnverifiedProof::new(
            Uuid::parse_str(&lease.incarnation).unwrap(),
            Uuid::new_v4(),
            issuer_now + 30,
        )
        .unwrap();
        federation_prompt_issuer_proof_current_at(&lease, &proof, issuer_now).unwrap();
        federation_prompt_issuer_proof_current_at(&lease, &proof, issuer_now + 29).unwrap();
        assert!(
            federation_prompt_issuer_proof_current_at(&lease, &proof, issuer_now + 30).is_err()
        );
        assert!(
            federation_prompt_issuer_proof_current_at(&lease, &proof, issuer_now - 1).is_err(),
            "an issuer clock rollback outside its own bound remains fail-closed"
        );
        let foreign = remote_prompt_protocol::UnverifiedProof::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            issuer_now + 30,
        )
        .unwrap();
        assert!(federation_prompt_issuer_proof_current_at(&lease, &foreign, issuer_now).is_err());
    }
    fixture.server.finish().await;
}
