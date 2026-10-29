// Owns strict shared-ledger decoding and producer-identity exclusion controls.
// Does not certify activation, migration, RPC settlement or dispatch guards.
use super::*;

pub(super) fn entry(root: &FsPath) -> EngramSessionObligation {
    let connection = EngramConnectionConfig {
        binary_path: real_engram_control_fixture_path(),
        project_file: root.join(".engram-project"),
        home: root.to_path_buf(),
        project_root: root.to_path_buf(),
        actor_id: "fixture-actor".to_owned(),
        actor_context: None,
        session_id: "producer-session".to_owned(),
    };
    let request = EngramControlRequest::TurnCheckpoint {
        routing_token: "old-token".to_owned(),
        grant_id: "old-grant".to_owned(),
        next_intent: EngramNextIntent::Exit,
        report: EngramTurnReport::default(),
        idempotency_key: "frozen-checkpoint-key".to_owned(),
    };
    EngramSessionObligation {
        id: "obligation-one".to_owned(),
        project_id: "fixture-project".to_owned(),
        authority: materialize_fixture_engram_store(root, root),
        authority_confirmed: true,
        connection,
        external_ref: "termal:session:old-local-session".to_owned(),
        local_session_id: "old-local-session".to_owned(),
        owner_generation: 1,
        prompt_id: None,
        routing_token: "old-token".to_owned(),
        grant_id: "old-grant".to_owned(),
        kind: EngramSessionObligationKind::CheckpointContainment,
        phase: EngramSessionObligationPhase::Sent,
        basis: EngramSessionObligationBasis::CheckpointUnsettled,
        reason_code: "GrantScopeMismatch".to_owned(),
        frozen_request: serde_json::to_value(request).unwrap(),
        settlement_evidence: Value::Null,
    }
}

#[test]
fn project_containment_ledger_requires_complete_supported_metadata() {
    let (_state, _project, _session, root, _receiver) = fixture();
    let ledger = EngramSessionObligationLedger::empty()
        .with_entry(entry(&root))
        .unwrap();
    let value = serde_json::to_value(&ledger).unwrap();
    let decoded: EngramSessionObligationLedger = serde_json::from_value(value.clone()).unwrap();
    let original = &ledger.entries[0];
    assert!(decoded.excludes(
        &original.authority,
        &original.connection,
        &original.external_ref,
        None
    ));
    let mut replacement = original.connection.clone();
    replacement.session_id = "new-local-generation".to_owned();
    assert!(decoded.excludes(
        &original.authority,
        &replacement,
        &original.external_ref,
        None
    ));
    assert!(decoded.excludes(
        &original.authority,
        &replacement,
        "another-alias",
        Some("old-token")
    ));
    assert!(!decoded.excludes(&original.authority, &replacement, "healthy-reference", None));
    assert!(serde_json::from_value::<EngramSessionObligationLedger>(json!({})).is_err());
    assert!(
        serde_json::from_value::<EngramSessionObligationLedger>(json!({"schemaVersion":1}))
            .is_err()
    );
    for field in ["kind", "phase", "basis"] {
        let mut unsupported = value.clone();
        unsupported["entries"][0][field] = json!("future_unsupported_value");
        assert!(serde_json::from_value::<EngramSessionObligationLedger>(unsupported).is_err());
    }
    for field in [
        "id",
        "projectId",
        "connection",
        "ownerGeneration",
        "frozenRequest",
        "settlementEvidence",
    ] {
        let mut missing = value.clone();
        missing["entries"][0].as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<EngramSessionObligationLedger>(missing).is_err(),
            "missing {field}"
        );
    }
    let mut future_connection = value.clone();
    future_connection["entries"][0]["connection"]["unknown_authority_component"] =
        json!("unsupported");
    assert!(serde_json::from_value::<EngramSessionObligationLedger>(future_connection).is_err());
    let mut unproved = value.clone();
    unproved["entries"][0]["phase"] = json!("confirmed");
    assert!(serde_json::from_value::<EngramSessionObligationLedger>(unproved).is_err());
    let mut unsupported = value;
    unsupported["schemaVersion"] = json!(2);
    assert!(serde_json::from_value::<EngramSessionObligationLedger>(unsupported).is_err());
    assert!(
        serde_json::from_value::<EngramSessionObligationLedger>(
            json!({"schemaVersion":1,"entries":[],"checkpointOperations":[]})
        )
        .unwrap()
        .entries
        .is_empty()
    );
}

#[test]
fn project_containment_ledger_rejects_conflicting_identity_and_changed_replay() {
    let (_state, _project, _session, root, _receiver) = fixture();
    let first = entry(&root);
    let ledger = EngramSessionObligationLedger::empty()
        .with_entry(first.clone())
        .unwrap();
    assert_eq!(ledger.with_entry(first.clone()).unwrap().entries.len(), 1);
    let mut changed = first.clone();
    changed.frozen_request["idempotency_key"] = json!("replacement-key");
    assert!(ledger.with_entry(changed).is_err());
    let mut conflicting = first;
    conflicting.id = "new-id".to_owned();
    conflicting.local_session_id = "recreated-local-session".to_owned();
    conflicting.owner_generation = 2;
    assert!(ledger.with_entry(conflicting).is_err());
    let mut value = serde_json::to_value(&ledger).unwrap();
    let duplicate = value["entries"][0].clone();
    value["entries"].as_array_mut().unwrap().push(duplicate);
    assert!(serde_json::from_value::<EngramSessionObligationLedger>(value).is_err());
}

#[test]
fn project_containment_ledger_keeps_distinct_kinds_and_exact_request_shape() {
    let (_state, _project, _session, root, _receiver) = fixture();
    let containment = entry(&root);
    let mut retirement = containment.clone();
    retirement.id = "retirement-two".to_owned();
    retirement.kind = EngramSessionObligationKind::IssuedGrantRetirement;
    retirement.prompt_id = Some("original-owner-head".to_owned());
    retirement.basis = EngramSessionObligationBasis::ProvenNeverSentBegin;
    retirement.phase = EngramSessionObligationPhase::Intended;
    retirement.frozen_request = serde_json::to_value(EngramControlRequest::SessionBind {
        external_ref: "termal:session:old-local-session".to_owned(),
        title: "Fixture".to_owned(),
        assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
        mediated_effects: Vec::new(),
        capability_map_revision: 1,
        work_binding: None,
        idempotency_key: "distinct-retirement-key".to_owned(),
    })
    .unwrap();
    let ledger = EngramSessionObligationLedger::empty()
        .with_entry(containment.clone())
        .unwrap()
        .with_entry(retirement)
        .unwrap();
    assert_eq!(ledger.entries.len(), 2);
    assert!(ledger.excludes(
        &containment.authority,
        &containment.connection,
        &containment.external_ref,
        None
    ));
    let mut unsupported = containment.clone();
    unsupported.frozen_request["future_private_field"] = json!(true);
    assert!(
        EngramSessionObligationLedger::empty()
            .with_entry(unsupported)
            .is_err()
    );
    let mut mismatched = containment;
    mismatched.frozen_request["grant_id"] = json!("successor-grant");
    assert!(
        EngramSessionObligationLedger::empty()
            .with_entry(mismatched)
            .is_err()
    );
}
