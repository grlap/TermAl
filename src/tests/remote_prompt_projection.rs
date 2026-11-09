// Owns strict status decoding and correlation controls; no transport or runtime.
use super::*;

fn status() -> Value {
    json!({"protocolVersion":1,"operationId":"12bc727c-9af0-45fd-8c79-8dfe7cd12021",
        "intentDigest":"ab".repeat(32),"duplicate":false,"localIntentDurable":true,
        "receiver":{"received":true,"queueEntryId":"prompt-1","queuePosition":null,
            "rootPaused":false,"rootStopping":false,"cancelledUnstarted":false,
            "delivery":{"disposition":"accepted","attempt":2,
                "writeAhead":"abcddce8-3795-4ac7-9ba7-cae69fb890cd","turnGeneration":5,
                "accepted":null,"completed":null}}})
}

fn observation(kind: &str) -> Value {
    json!({"kind":kind,"promptId":"prompt-1","attempt":2,
        "writeAhead":"abcddce8-3795-4ac7-9ba7-cae69fb890cd",
        "runtimeId":"runtime-1","turnGeneration":5,"correlation":"response-1"})
}

fn decode(value: &Value) -> Result<RemotePromptStatus, ApiError> {
    RemotePromptStatus::decode_bounded(&serde_json::to_vec(value).unwrap())
}

#[test]
fn remote_prompt_projection_accepted_disposition_does_not_invent_observations() {
    let parsed = decode(&status()).unwrap();
    let delivery = parsed.receiver.as_ref().unwrap().delivery.as_ref().unwrap();
    assert!(delivery.accepted.is_none() && delivery.completed.is_none());
    assert_eq!(
        serde_json::from_slice::<Value>(&parsed.encode_bounded().unwrap()).unwrap(),
        status()
    );
}

#[test]
fn remote_prompt_projection_retains_exact_correlated_acceptance_and_completion() {
    let mut value = status();
    value["receiver"]["delivery"]["accepted"] = observation("codexTurnStartResponse");
    value["receiver"]["delivery"]["completed"] = observation("nativeTurnCompleted");
    let parsed = decode(&value).unwrap();
    assert!(
        parsed
            .receiver
            .unwrap()
            .delivery
            .unwrap()
            .completed
            .is_some()
    );
    let mut acp = status();
    acp["receiver"]["delivery"]["accepted"] = observation("acpPromptResponse");
    acp["receiver"]["delivery"]["completed"] = observation("acpPromptResponse");
    decode(&acp).unwrap();
}

#[test]
fn remote_prompt_projection_rejects_mismatched_observation_identity() {
    for (field, bad) in [
        ("promptId", json!("other")),
        ("attempt", json!(3)),
        ("writeAhead", json!("e8eaf50e-6670-46c4-8187-428a48c44c02")),
        ("turnGeneration", json!(6)),
        ("runtimeId", json!("")),
        ("correlation", json!("\n")),
    ] {
        let mut value = status();
        let mut accepted = observation("claudeOwnedCommand");
        accepted[field] = bad;
        value["receiver"]["delivery"]["accepted"] = accepted;
        assert!(decode(&value).is_err(), "{field}");
    }
    let mut value = status();
    value["receiver"]["delivery"]["accepted"] = observation("codexTurnStartResponse");
    value["receiver"]["delivery"]["completed"] = observation("nativeTurnCompleted");
    value["receiver"]["delivery"]["completed"]["runtimeId"] = json!("other");
    assert!(decode(&value).is_err());
}

#[test]
fn remote_prompt_projection_rejects_positive_facts_without_delivery_or_durable_intent() {
    let mut terminal_as_acceptance = status();
    terminal_as_acceptance["receiver"]["delivery"]["accepted"] = observation("nativeTurnCompleted");
    assert!(decode(&terminal_as_acceptance).is_err());
    for disposition in ["pending", "handingOff", "ambiguous", "rejected"] {
        let mut value = status();
        value["receiver"]["delivery"]["accepted"] = observation("codexTurnStartResponse");
        value["receiver"]["delivery"]["disposition"] = json!(disposition);
        assert!(decode(&value).is_err(), "{disposition}");
    }
    let mut value = status();
    value["localIntentDurable"] = json!(false);
    assert!(decode(&value).is_err());
    value["receiver"] = Value::Null;
    decode(&value).unwrap();
    let mut value = status();
    value["receiver"]["cancelledUnstarted"] = json!(true);
    assert!(decode(&value).is_err());
}

#[test]
fn remote_prompt_projection_strict_at_every_object_depth() {
    for pointer in [
        "",
        "/receiver",
        "/receiver/delivery",
        "/receiver/delivery/accepted",
    ] {
        let mut value = status();
        value["receiver"]["delivery"]["accepted"] = observation("codexTurnStartResponse");
        value
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), json!(1));
        assert!(decode(&value).is_err(), "unknown at {pointer}");
    }
    let bytes = serde_json::to_string(&status()).unwrap();
    for (original, duplicate) in [
        (
            "\"protocolVersion\":1",
            "\"protocolVersion\":1,\"protocolVersion\":1",
        ),
        ("\"received\":true", "\"received\":true,\"received\":true"),
        ("\"attempt\":2", "\"attempt\":2,\"attempt\":2"),
    ] {
        assert!(
            RemotePromptStatus::decode_bounded(bytes.replace(original, duplicate).as_bytes())
                .is_err()
        );
    }
    for pointer in [
        "/receiver/queuePosition",
        "/receiver/delivery/turnGeneration",
        "/receiver/delivery/completed",
    ] {
        let mut value = status();
        let (object, field) = pointer.rsplit_once('/').unwrap();
        value
            .pointer_mut(object)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(decode(&value).is_err(), "missing {pointer}");
    }
}

#[test]
fn remote_prompt_projection_bounded_canonical_and_exact_owner_lookup() {
    let value = status();
    let parsed = decode(&value).unwrap();
    parsed
        .validate_for_operation(parsed.operation_id, &parsed.intent_digest, "prompt-1")
        .unwrap();
    assert!(
        parsed
            .validate_for_operation(Uuid::new_v4(), &parsed.intent_digest, "prompt-1")
            .is_err()
    );
    assert!(
        parsed
            .validate_for_operation(parsed.operation_id, &parsed.intent_digest, "other")
            .is_err()
    );
    for bad in [
        "00000000-0000-0000-0000-000000000000",
        "12BC727C-9AF0-45FD-8C79-8DFE7CD12021",
    ] {
        let mut value = value.clone();
        value["operationId"] = json!(bad);
        assert!(decode(&value).is_err());
    }
    assert!(RemotePromptStatus::decode_bounded(&vec![b' '; 16_385]).is_err());
    assert!(RemotePromptStatus::decode_bounded(&[0xff]).is_err());
    assert!(RemotePromptStatus::decode_bounded(b"null").is_err());
    assert!(
        RemotePromptStatus::decode_optional_bounded(b"null")
            .unwrap()
            .is_none()
    );
    assert!(
        RemotePromptStatus::decode_optional_bounded(&serde_json::to_vec(&value).unwrap())
            .unwrap()
            .is_some()
    );
    assert!(RemotePromptStatus::decode_optional_bounded(&vec![b' '; 16_385]).is_err());
}
