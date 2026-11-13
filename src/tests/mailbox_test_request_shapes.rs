// Wire and durable-record compatibility, using disposable in-memory fixtures.
// Legacy byte fixtures describe the old public/persisted contract; no source,
// instruction files, run artifacts or live store are read by these tests.
use super::*;

fn legacy_origin_bytes() -> String {
    concat!(
        "{\"host\":{\"hostId\":\"11111111-1111-4111-8111-111111111111\",\"hostGeneration\":1},",
        "\"nativeSessionId\":\"session-1\",",
        "\"nativeCreationId\":\"22222222-2222-4222-8222-222222222222\",",
        "\"projectId\":\"project-1\",\"projectScope\":\"example/repository\"}"
    )
    .into()
}

fn legacy_registration_bytes() -> String {
    format!(
        concat!(
            "{{\"runId\":\"test-legacy\",\"nativeOrigin\":{},",
            "\"projectId\":\"project-1\",\"runDirectory\":\"repository/review-runs/test-legacy\",",
            "\"locatorIdentity\":null,\"worktree\":\"repository\",",
            "\"requestIdentity\":\"legacy-request\",\"initiatingSessionId\":\"session-1\",",
            "\"owningSessionId\":\"session-1\",\"creatorPid\":null,",
            "\"registeredAt\":\"2026-11-12T00:00:00Z\",\"startedAt\":null,",
            "\"preset\":\"focused\",\"detached\":false,\"stageNames\":[\"focused\"],",
            "\"workBinding\":null,\"authorityStore\":null,\"validatedUnbound\":true,",
            "\"monitoringInterruptedAt\":null}}"
        ),
        legacy_origin_bytes(),
    )
}

fn legacy_mailbox_bytes() -> &'static str {
    concat!(
        "{\"id\":\"mailbox-message-33333333-3333-4333-8333-333333333333\",",
        "\"mailboxId\":\"mailbox-44444444-4444-4444-8444-444444444444\",\"sequence\":1,",
        "\"senderSessionId\":\"session-1\",\"senderName\":\"Sender\",",
        "\"targetSessionId\":\"session-2\",\"targetName\":\"Runner\",",
        "\"createdAt\":\"2026-11-12T00:00:00Z\",\"class\":\"routine\",",
        "\"body\":\"ordinary message\",",
        "\"notificationState\":\"durableButNotWoken\"}"
    )
}

fn legacy_summary_bytes() -> &'static str {
    concat!(
        "{\"registrationState\":null,\"trackingDurability\":null,\"initiatingSessionId\":null,",
        "\"boundWorkIds\":[],\"creatorPid\":null,\"runnerPid\":null,\"monitoringInterruptedAt\":null,",
        "\"restartWouldVoidEvidence\":null,\"restartEvidenceReason\":\"\",\"observedAt\":null,",
        "\"verificationRecordIds\":[],\"sourceFingerprints\":[],\"artifactLocations\":[],",
        "\"trackingProvenances\":[],\"supersededRunIds\":[],\"startTimeOrigin\":null,\"plannedStageNames\":[],",
        "\"runId\":\"test-legacy\",\"projectId\":\"project-1\",\"worktree\":\"repository\",",
        "\"runDir\":\"repository/review-runs/test-legacy\",\"preset\":\"focused\",",
        "\"command\":null,\"commandTruncated\":false,\"detached\":false,\"state\":\"passed\",",
        "\"interrupted\":false,\"currentStage\":null,\"stages\":[],\"ownerSessionId\":\"session-1\",",
        "\"notifyTo\":null,\"notifySessionId\":null,\"startedAt\":null,\"endedAt\":null,",
        "\"exitCode\":0,\"error\":null,\"detailVersion\":null}"
    )
}

fn legacy_compacted_bytes() -> String {
    format!(
        concat!(
            "{{\"compacted\":true,\"requestIdentity\":\"legacy-request\",\"summary\":{},",
            "\"registration\":{},\"observations\":[],\"requestStamp\":null,\"resultsStamp\":null,",
            "\"reason\":\"resolved\",\"artifactConflict\":false,\"artifactUnavailable\":false}}"
        ),
        legacy_summary_bytes(),
        legacy_registration_bytes(),
    )
}

fn principal(host: char, store: char, creation: char, session: &str) -> MailboxRelayPrincipal {
    fn id(ch: char) -> String {
        format!(
            "{0}{0}{0}{0}{0}{0}{0}{0}-{0}{0}{0}{0}-4{0}{0}{0}-8{0}{0}{0}-{0}{0}{0}{0}{0}{0}{0}{0}{0}{0}{0}{0}",
            ch
        )
    }
    MailboxRelayPrincipal {
        host: FederationHostIdentity {
            host_id: id(host),
            host_generation: 1,
        },
        store_id: id(store),
        native_session_id: session.into(),
        native_creation_id: id(creation),
    }
}

fn legacy_relay_bytes() -> String {
    let sender = principal('1', '2', '3', "session-1");
    let target = principal('4', '5', '6', "session-2");
    let conversation = MailboxRelayConversation::new(
        MailboxRelayHome {
            host: sender.host.clone(),
            store_id: sender.store_id.clone(),
        },
        sender.clone(),
        target.clone(),
    )
    .unwrap();
    format!(
        concat!(
            "{{\"requestedTarget\":\"host:runner::session-2\",\"conversation\":{},",
            "\"sender\":{},\"target\":{},\"senderName\":\"Sender\",\"targetName\":\"Runner\",",
            "\"message\":\"ordinary message\",\"idempotencyKey\":\"legacy-send\",\"topic\":null,\"stateStamp\":null}}"
        ),
        serde_json::to_string(&conversation).unwrap(),
        serde_json::to_string(&sender).unwrap(),
        serde_json::to_string(&target).unwrap(),
    )
}

fn payload() -> MailboxTestPayload {
    MailboxTestPayload {
        schema_version: 1,
        request_send_key: "legacy-send".into(),
        recipient: Some(principal('4', '5', '6', "session-2")),
        requester: principal('1', '2', '3', "session-1"),
        mode: MailboxTestMode::Focused,
        targets: vec![MailboxTestCapturedTarget {
            target: MailboxTestTarget {
                work_ref: "work-example".into(),
                r#use: MailboxTestInputUse::CommittedInput {
                    expected_commit: "a".repeat(40),
                    purpose: MailboxTestPurpose::Verification,
                },
            },
            association: None,
            uncredited_reason: Some("producer_unsupported".into()),
        }],
    }
}

#[test]
fn ordinary_mailbox_and_relay_keep_legacy_bytes_and_digest() {
    let encoded = legacy_mailbox_bytes();
    let message: MailboxMessage = serde_json::from_str(encoded).unwrap();
    assert!(message.test_request.is_none());
    assert_eq!(serde_json::to_vec(&message).unwrap(), encoded.as_bytes());

    let encoded = legacy_relay_bytes();
    let relay: MailboxRelayAppend = serde_json::from_str(&encoded).unwrap();
    assert!(relay.test_request.is_none());
    assert!(relay.input().is_ok());
    assert_eq!(serde_json::to_vec(&relay).unwrap(), encoded.as_bytes());
    // The frozen relay protocol digest of this ordinary, constructed fixture.
    assert_eq!(
        relay.digest().unwrap(),
        "3a102c016e0967210fc578136c8c51e784581d8e310b5910de736ffe8f457ab3"
    );
}

#[test]
fn native_origin_and_compacted_registration_keep_legacy_bytes() {
    let encoded = legacy_registration_bytes();
    let registration: TestRunRegistration = serde_json::from_str(&encoded).unwrap();
    assert!(
        registration
            .native_origin
            .as_ref()
            .unwrap()
            .causal_request
            .is_none()
    );
    assert_eq!(
        serde_json::to_vec(&registration).unwrap(),
        encoded.as_bytes()
    );
    let encoded = legacy_compacted_bytes();
    let compacted: TestRunCompacted = serde_json::from_str(&encoded).unwrap();
    assert!(
        compacted
            .registration
            .as_ref()
            .unwrap()
            .native_origin
            .as_ref()
            .unwrap()
            .causal_request
            .is_none()
    );
    assert_eq!(serde_json::to_vec(&compacted).unwrap(), encoded.as_bytes());
}

#[test]
fn every_custody_ingress_refuses_malformed_or_incomplete_values() {
    let mut wrong_version = serde_json::to_value(payload()).unwrap();
    wrong_version["schemaVersion"] = json!(2);
    for supplied in [json!({}), wrong_version, json!("malformed")] {
        let mut mailbox: Value = serde_json::from_str(legacy_mailbox_bytes()).unwrap();
        mailbox["testRequest"] = supplied.clone();
        assert!(serde_json::from_value::<MailboxMessage>(mailbox).is_err());
        let mut relay: Value = serde_json::from_str(&legacy_relay_bytes()).unwrap();
        relay["testRequest"] = supplied.clone();
        assert!(serde_json::from_value::<MailboxRelayAppend>(relay).is_err());
        let mut origin: Value = serde_json::from_str(&legacy_origin_bytes()).unwrap();
        origin["causalRequest"] = supplied.clone();
        assert!(serde_json::from_value::<TestRunNativeOrigin>(origin).is_err());
        let mut registration: Value = serde_json::from_str(&legacy_registration_bytes()).unwrap();
        registration["nativeOrigin"]["causalRequest"] = supplied.clone();
        assert!(serde_json::from_value::<TestRunRegistration>(registration).is_err());
        let mut compacted: Value = serde_json::from_str(&legacy_compacted_bytes()).unwrap();
        compacted["registration"]["nativeOrigin"]["causalRequest"] = supplied;
        assert!(serde_json::from_value::<TestRunCompacted>(compacted).is_err());
    }
}

#[test]
fn programmatic_relay_custody_is_validated_and_changes_only_selected_digests() {
    let mut relay: MailboxRelayAppend = serde_json::from_str(&legacy_relay_bytes()).unwrap();
    let ordinary = relay.digest().unwrap();
    relay.test_request = Some(payload());
    relay.input().unwrap();
    assert_ne!(relay.digest().unwrap(), ordinary);
    let mut wrong = payload();
    wrong.recipient = Some(wrong.requester.clone());
    relay.test_request = Some(wrong);
    assert!(relay.input().is_err());
    assert!(relay.digest().is_err());
    relay.test_request = Some(payload());
    relay.idempotency_key = "a-different-send".into();
    assert!(relay.input().is_err());
    assert!(relay.digest().is_err());
}

#[test]
fn nested_mailbox_pages_and_relay_rpc_retain_validated_custody() {
    let expected = payload();
    let mut message: Value = serde_json::from_str(legacy_mailbox_bytes()).unwrap();
    message["testRequest"] = serde_json::to_value(&expected).unwrap();
    let command = CoordinationCliCommand::MailboxReadMessage {
        as_session: "session-2".into(),
        message_id: message["id"].as_str().unwrap().into(),
    };
    validate_coordination_cli_output(&command, &message).unwrap();
    let decoded: MailboxMessage = serde_json::from_value(message.clone()).unwrap();
    assert_eq!(decoded.test_request.as_ref(), Some(&expected));
    let page: MailboxReadResponse = serde_json::from_value(json!({
        "messages":[message],"afterSequence":0,"processedThrough":0
    }))
    .unwrap();
    assert_eq!(page.messages[0].test_request.as_ref(), Some(&expected));
    let mut append: Value = serde_json::from_str(&legacy_relay_bytes()).unwrap();
    append["testRequest"] = serde_json::to_value(&expected).unwrap();
    let rpc: MailboxRelayRequest = serde_json::from_value(json!({
        "version":MAILBOX_RELAY_VERSION, "requestId":"77777777-7777-4777-8777-777777777777",
        "operation":{"operation":"send","append":append}
    }))
    .unwrap();
    let MailboxRelayOperation::Send { append } = rpc.operation else {
        panic!("wrong operation")
    };
    assert_eq!(append.test_request.as_ref(), Some(&expected));
    append.input().unwrap();
}

#[test]
fn captured_associations_are_closed_and_cannot_hide_an_unexplained_target() {
    let mut value = serde_json::to_value(payload()).unwrap();
    value["targets"][0]["target"]["workRef"] = json!("w-aaaaaaaaaaaa");
    value["targets"][0]
        .as_object_mut()
        .unwrap()
        .remove("uncreditedReason");
    value["targets"][0]["association"] = json!({
        "binding":{
            "root_execution_id":"11111111-1111-4111-8111-111111111111",
            "work_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "run_id":"22222222-2222-4222-8222-222222222222",
            "work_revision":1,"claim_id":"33333333-3333-4333-8333-333333333333","claim_fence":1
        },
        "root":{"workspace_id":"fixture-worktree","generation":1,"state":"named"}
    });
    serde_json::from_value::<MailboxTestPayload>(value.clone()).unwrap();
    for level in ["binding", "root"] {
        let mut invalid = value.clone();
        invalid["targets"][0]["association"][level]["injected"] = json!(true);
        assert!(serde_json::from_value::<MailboxTestPayload>(invalid).is_err());
    }
    let mut both = value.clone();
    both["targets"][0]["uncreditedReason"] = json!("association_stale");
    assert!(serde_json::from_value::<MailboxTestPayload>(both).is_err());
    value["targets"][0]
        .as_object_mut()
        .unwrap()
        .remove("association");
    assert!(serde_json::from_value::<MailboxTestPayload>(value).is_err());
    let mut draft = serde_json::to_value(payload()).unwrap();
    draft.as_object_mut().unwrap().remove("recipient");
    assert!(serde_json::from_value::<MailboxTestPayload>(draft).is_err());
    assert!(
        !format!("{:?}", payload()).contains("legacy-send"),
        "Debug must redact the captured send key"
    );
}

#[test]
fn explicit_null_stays_absent_on_wire_and_in_persisted_native_origin() {
    let mut mailbox: Value = serde_json::from_str(legacy_mailbox_bytes()).unwrap();
    mailbox["testRequest"] = Value::Null;
    let message: MailboxMessage = serde_json::from_value(mailbox).unwrap();
    assert_eq!(
        serde_json::to_vec(&message).unwrap(),
        legacy_mailbox_bytes().as_bytes()
    );

    let mut relay: Value = serde_json::from_str(&legacy_relay_bytes()).unwrap();
    relay["testRequest"] = Value::Null;
    let relay: MailboxRelayAppend = serde_json::from_value(relay).unwrap();
    assert_eq!(
        serde_json::to_vec(&relay).unwrap(),
        legacy_relay_bytes().as_bytes()
    );

    let mut registration: Value = serde_json::from_str(&legacy_registration_bytes()).unwrap();
    registration["nativeOrigin"]["causalRequest"] = Value::Null;
    let registration: TestRunRegistration = serde_json::from_value(registration).unwrap();
    assert_eq!(
        serde_json::to_vec(&registration).unwrap(),
        legacy_registration_bytes().as_bytes()
    );
}

#[test]
fn public_request_shape_cannot_supply_host_association_or_expand_purpose() {
    for mode in ["full", "focused"] {
        let mut request = json!({"mode":mode,"targets":[{
            "workRef":"work-example",
            "use":{"kind":"committed_input","expected_commit":"a".repeat(40),"purpose":"verification"}
        }]});
        let parsed: MailboxTestRequest = serde_json::from_value(request.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), request);
        request["targets"][0]["binding"] = json!({"claim_fence":1});
        assert!(serde_json::from_value::<MailboxTestRequest>(request).is_err());
    }
    assert!(serde_json::from_value::<MailboxTestRequest>(json!({
        "mode":"full", "targets":[{"workRef":"work-example", "use":{
            "kind":"authorized_test_ref", "ref_name":"refs/heads/test/example", "expected_tree":"b".repeat(40),
            "purpose":"verification"
        }}]
    })).is_err());
    assert!(
        serde_json::from_value::<MailboxTestRequest>(json!({"mode":"live","targets":[]})).is_err()
    );
}
