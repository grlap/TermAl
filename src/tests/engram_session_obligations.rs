// Strict historical ledger decoding; no live producer exclusion or replay owner.
// Does not certify activation, migration, RPC settlement or dispatch guards.
use super::*;

pub(super) fn entry(root: &FsPath) -> EngramSessionObligation {
    let connection = EngramConnectionConfig {
        binary_path: PathBuf::from("engram"),
        project_file: root.join(".engram-project"),
        home: root.to_path_buf(),
        project_root: root.to_path_buf(),
        actor_id: "fixture-actor".to_owned(),
        actor_context: None,
        session_id: "producer-session".to_owned(),
    };
    let request = json!({"operation":"turn_checkpoint", "routing_token":"old-token",
        "grant_id":"old-grant", "next_intent":"exit", "idempotency_key":"frozen-checkpoint-key"});
    EngramSessionObligation {
        id: "obligation-one".to_owned(),
        project_id: "fixture-project".to_owned(),
        authority: EngramAuthorityStoreKey { database_path: root.join("historical.sqlite"), project_id: "historical-project".into() },
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
        frozen_request: request,
        settlement_evidence: Value::Null,
    }
}

#[test]
fn project_containment_ledger_requires_complete_supported_metadata() {
    let root = TestTempRoot::create("historical-ledger");
    let ledger = ledger(vec![entry(root.path())]).unwrap();
    let value = serde_json::to_value(&ledger).unwrap();
    let decoded: EngramSessionObligationLedger = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), value,
        "historical identity remains exact without a live excludes API");
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
fn project_containment_ledger_rejects_conflicting_identity_and_changed_frozen_request() {
    let root = TestTempRoot::create("historical-ledger");
    let first = entry(root.path());
    let ledger = ledger(vec![first.clone()]).unwrap();
    let mut changed = first.clone();
    changed.frozen_request["idempotency_key"] = json!("replacement-key");
    assert!(self::ledger(vec![first.clone(), changed]).is_err(), "same original cannot have two frozen requests");
    let mut conflicting = first.clone();
    conflicting.id = "new-id".into();
    conflicting.local_session_id = "recreated-local-session".into();
    conflicting.owner_generation = 2;
    assert!(self::ledger(vec![first, conflicting]).is_err());
    let mut value = serde_json::to_value(&ledger).unwrap();
    let duplicate = value["entries"][0].clone();
    value["entries"].as_array_mut().unwrap().push(duplicate);
    assert!(serde_json::from_value::<EngramSessionObligationLedger>(value).is_err());
}

#[test]
fn project_containment_ledger_keeps_distinct_kinds_and_exact_request_shape() {
    let root = TestTempRoot::create("historical-ledger");
    let containment = entry(root.path());
    let mut retirement = containment.clone();
    retirement.id = "retirement-two".to_owned();
    retirement.kind = EngramSessionObligationKind::IssuedGrantRetirement;
    retirement.prompt_id = Some("original-owner-head".to_owned());
    retirement.basis = EngramSessionObligationBasis::ProvenNeverSentBegin;
    retirement.phase = EngramSessionObligationPhase::Intended;
    retirement.frozen_request = json!({"operation":"session_bind",
        "external_ref":"termal:session:old-local-session", "title":"Fixture",
        "assurance":"turn_gated", "mediated_effects":[], "capability_map_revision":1,
        "idempotency_key":"distinct-retirement-key"});
    let ledger = ledger(vec![containment.clone(), retirement]).unwrap();
    assert_eq!(ledger.entries.len(), 2);
    let mut unsupported = containment.clone();
    unsupported.frozen_request["future_private_field"] = json!(true);
    assert!(
        self::ledger(vec![unsupported]).is_err()
    );
    let mut mismatched = containment;
    mismatched.frozen_request["grant_id"] = json!("successor-grant");
    assert!(
        self::ledger(vec![mismatched]).is_err()
    );
}

fn ledger(entries: Vec<EngramSessionObligation>) -> Result<EngramSessionObligationLedger> {
    let ledger = EngramSessionObligationLedger { schema_version: 1, entries, checkpoint_operations: Vec::new() };
    ledger.validate()?;
    Ok(ledger)
}
