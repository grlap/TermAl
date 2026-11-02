// Canonical/mirror and owner consistency at real passive storage boundaries.
// The former in-flight replay/ACK races have no executable owner to exercise.
use super::*;

fn storage_image(connection: &rusqlite::Connection) -> Value {
    json!({"header":required_state_meta_value(connection, "schema_version").unwrap(),
        "generation":metadata_unit_generation(connection).unwrap(),
        "units":format!("{:?}", read_metadata_units_unchecked(connection).unwrap()),
        "rows":raw_session_rows(connection)})
}

#[test]
fn historical_canonical_checkpoint_rejects_duplicate_identity_key_and_duty() {
    let root = TestTempRoot::create("checkpoint-canonical-conflicts");
    let operation = passive_checkpoint(root.path(), "historical-session");
    for changed in ["identity", "key", "duty"] {
        let first = serde_json::to_value(&operation).unwrap();
        let mut second = first.clone();
        if changed != "identity" {
            second["original"]["id"] = json!("other-checkpoint");
            second["protection"]["id"] = json!("other-checkpoint");
            second["attempts"][0]["id"] = json!("other-checkpoint");
            if changed == "duty" {
                second["original"]["request"]["idempotency_key"] = json!("other-key");
            }
            second["protection"]["frozenRequest"] = second["original"].clone();
        }
        let raw = json!({"schemaVersion":1,"entries":[],"checkpointOperations":[first,second]});
        assert!(serde_json::from_value::<EngramSessionObligationLedger>(raw).is_err(), "{changed}");
    }
}

#[test]
fn historical_checkpoint_mirror_refuses_orphan_changed_original_and_wrong_session_without_writes() {
    for changed in ["orphan", "original", "session"] {
        let (_root, path, _inner, session, _operation) = passive_state(&format!("checkpoint-mirror-{changed}"));
        let connection = rusqlite::Connection::open(&path).unwrap();
        let raw: String = connection.query_row("SELECT value_json FROM sessions WHERE id=?1", [&session], |row| row.get(0)).unwrap();
        let mut row: Value = serde_json::from_str(&raw).unwrap();
        if changed == "orphan" {
            // Use the actual metadata unit codec, whose object representation
            // may split the field into more than one unit.
            let mut metadata = metadata_units_to_value(&read_metadata_units_unchecked(&connection).unwrap()).unwrap();
            metadata["engramSessionObligations"]["checkpointOperations"] = json!([]);
            connection.execute("DELETE FROM state_metadata_units", []).unwrap();
            for ((field, key), (kind, encoded)) in metadata_value_to_units(metadata).unwrap() {
                connection.execute("INSERT INTO state_metadata_units(field_name,item_key,kind,value_json) VALUES(?1,?2,?3,?4)",
                    rusqlite::params![field,key,kind,encoded]).unwrap();
            }
        } else {
            let operation = &mut row["engramCheckpointOperation"];
            if changed == "session" {
                operation["original"]["connection"]["session_id"] = json!("different-session");
                operation["protection"]["connection"]["session_id"] = json!("different-session");
                operation["protection"]["localSessionId"] = json!("different-session");
            } else {
                operation["original"]["request"]["idempotency_key"] = json!("changed-mirror-key");
            }
            operation["protection"]["frozenRequest"] = operation["original"].clone();
            connection.execute("UPDATE sessions SET value_json=?1 WHERE id=?2", rusqlite::params![row.to_string(),session]).unwrap();
        }
        let before = storage_image(&connection);
        drop(connection);
        assert!(load_state(&path).is_err(), "{changed}");
        let connection = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(storage_image(&connection), before, "{changed}: no normalization or write on rejection");
    }
}

#[test]
fn adopted_historical_checkpoint_requires_exact_owner_generation_authority_and_original() {
    let root = TestTempRoot::create("checkpoint-adopted-data");
    let operation = passive_checkpoint(root.path(), "historical-session");
    let mut entry = operation.protection.clone();
    entry.kind = EngramSessionObligationKind::CheckpointContainment;
    entry.basis = EngramSessionObligationBasis::ControlStateUnsettled;
    entry.phase = EngramSessionObligationPhase::Intended;
    entry.owner_generation = 1;
    entry.frozen_request = json!({"operation":"unsettled_control_image","routingToken":entry.routing_token,
        "activeGrantId":entry.grant_id,"uncertainGrantId":null,"beginsRecorded":true,"bindInProgress":false,
        "checkpointInProgress":true,"existingCheckpoint":operation,"runtimeRunning":false,
        "queuedPrompts":[],"beginRequested":null,"dispatchGeneration":0});
    entry.validate().unwrap();
    for changed in ["generation", "authority", "token", "original"] {
        let mut invalid = serde_json::to_value(&entry).unwrap();
        match changed {
            "generation" => invalid["ownerGeneration"] = json!(2),
            "authority" => invalid["authority"]["projectId"] = json!("other-authority"),
            "token" => {
                invalid["routingToken"] = json!("other-token");
                invalid["frozenRequest"]["routingToken"] = json!("other-token");
            }
            "original" => invalid["frozenRequest"]["existingCheckpoint"]["original"]["id"] = json!("other-original"),
            _ => unreachable!(),
        }
        let invalid: EngramSessionObligation = serde_json::from_value(invalid).unwrap();
        assert!(invalid.validate().is_err(), "{changed}");
    }
}

#[test]
fn accepted_historical_component_does_not_rewrite_an_independent_enclosing_record() {
    let (_root, path, mut inner, session, mut operation) = passive_state("checkpoint-independent-history");
    let mut independent = operation.protection.clone();
    independent.id = "independent-record".into();
    independent.kind = EngramSessionObligationKind::IssuedGrantRetirement;
    independent.basis = EngramSessionObligationBasis::ProvenNeverSentBegin;
    independent.phase = EngramSessionObligationPhase::Intended;
    independent.owner_generation = 1;
    independent.prompt_id = Some("independent-head".into());
    independent.frozen_request = json!({"operation":"session_bind","external_ref":independent.external_ref,
        "title":"Independent original","assurance":"turn_gated","mediated_effects":[],
        "capability_map_revision":1,"idempotency_key":"independent-key"});
    let original_independent = serde_json::to_value(&independent).unwrap();
    inner.engram_session_obligations.entries.push(independent);
    accepted_history(&mut operation);
    install_historical_checkpoint(&mut inner, &session, &operation);
    persist_state(&path, &inner).unwrap();
    let loaded = load_state(&path).unwrap().unwrap();
    assert_historical_operation(&loaded, &operation);
    assert_eq!(serde_json::to_value(&loaded.engram_session_obligations.entries[0]).unwrap(), original_independent);
    assert_eq!(loaded.engram_session_obligations.entries[0].phase, EngramSessionObligationPhase::Intended);
}

#[test]
fn accepted_historical_receipt_and_original_roundtrip_without_a_live_ack_latch() {
    let (_root, path, mut inner, session, mut operation) = passive_state("checkpoint-accepted-data");
    let original = serde_json::to_value(&operation.original).unwrap();
    let first = serde_json::to_value(&operation.attempts[0]).unwrap();
    accepted_history(&mut operation);
    install_historical_checkpoint(&mut inner, &session, &operation);
    persist_state(&path, &inner).unwrap();
    let loaded = load_state(&path).unwrap().unwrap();
    assert_historical_operation(&loaded, &operation);
    assert_eq!(serde_json::to_value(&operation.original).unwrap(), original);
    assert_eq!(serde_json::to_value(&operation.attempts[0]).unwrap(), first);
    let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    assert!(!record.engram.historical_control.contains_key("engramOwedBinds"));
    assert!(matches!(record.runtime, SessionRuntime::None));
}
