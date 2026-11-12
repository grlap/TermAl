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
fn every_preparatory_serde_ingress_refuses_supplied_custody() {
    for supplied in [
        json!({}),
        serde_json::to_value(payload()).unwrap(),
        json!("malformed"),
    ] {
        let mut mailbox: Value = serde_json::from_str(legacy_mailbox_bytes()).unwrap();
        mailbox["testRequest"] = supplied.clone();
        assert!(
            serde_json::from_value::<MailboxMessage>(mailbox)
                .unwrap_err()
                .to_string()
                .contains(TEST_REQUEST_CUSTODY_UNAVAILABLE)
        );

        let mut relay: Value = serde_json::from_str(&legacy_relay_bytes()).unwrap();
        relay["testRequest"] = supplied.clone();
        assert!(
            serde_json::from_value::<MailboxRelayAppend>(relay)
                .unwrap_err()
                .to_string()
                .contains(TEST_REQUEST_CUSTODY_UNAVAILABLE)
        );

        let mut origin: Value = serde_json::from_str(&legacy_origin_bytes()).unwrap();
        origin["causalRequest"] = supplied.clone();
        assert!(
            serde_json::from_value::<TestRunNativeOrigin>(origin)
                .unwrap_err()
                .to_string()
                .contains(TEST_REQUEST_CUSTODY_UNAVAILABLE)
        );

        let mut registration: Value = serde_json::from_str(&legacy_registration_bytes()).unwrap();
        registration["nativeOrigin"]["causalRequest"] = supplied.clone();
        assert!(
            serde_json::from_value::<TestRunRegistration>(registration)
                .unwrap_err()
                .to_string()
                .contains(TEST_REQUEST_CUSTODY_UNAVAILABLE)
        );

        let mut compacted: Value = serde_json::from_str(&legacy_compacted_bytes()).unwrap();
        compacted["registration"]["nativeOrigin"]["causalRequest"] = supplied;
        assert!(
            serde_json::from_value::<TestRunCompacted>(compacted)
                .unwrap_err()
                .to_string()
                .contains(TEST_REQUEST_CUSTODY_UNAVAILABLE)
        );
    }
}

#[test]
fn programmatic_relay_payload_cannot_bypass_inactive_custody_guard() {
    let mut relay: MailboxRelayAppend = serde_json::from_str(&legacy_relay_bytes()).unwrap();
    relay.test_request = Some(payload());
    assert_eq!(
        relay.input().unwrap_err().message,
        TEST_REQUEST_CUSTODY_UNAVAILABLE
    );
    assert_eq!(
        relay.digest().unwrap_err().message,
        TEST_REQUEST_CUSTODY_UNAVAILABLE
    );
}

#[test]
fn nested_mailbox_pages_and_relay_rpc_cannot_discard_supplied_custody() {
    let mut message: Value = serde_json::from_str(legacy_mailbox_bytes()).unwrap();
    message["testRequest"] = serde_json::to_value(payload()).unwrap();
    let command = CoordinationCliCommand::MailboxReadMessage {
        as_session: "session-2".into(),
        message_id: message["id"].as_str().unwrap().into(),
    };
    assert!(
        validate_coordination_cli_output(&command, &message)
            .unwrap_err()
            .to_string()
            .contains(TEST_REQUEST_CUSTODY_UNAVAILABLE)
    );
    let page = json!({"messages":[message],"afterSequence":0,"processedThrough":0});
    assert!(
        serde_json::from_value::<MailboxReadResponse>(page)
            .unwrap_err()
            .to_string()
            .contains(TEST_REQUEST_CUSTODY_UNAVAILABLE)
    );

    let mut append: Value = serde_json::from_str(&legacy_relay_bytes()).unwrap();
    append["testRequest"] = serde_json::to_value(payload()).unwrap();
    let rpc = json!({
        "version":MAILBOX_RELAY_VERSION, "requestId":"77777777-7777-4777-8777-777777777777",
        "operation":{"operation":"send","append":append}
    });
    let error = serde_json::from_value::<MailboxRelayRequest>(rpc)
        .err()
        .expect("the HTTP relay request must refuse inactive custody");
    assert!(error.to_string().contains(TEST_REQUEST_CUSTODY_UNAVAILABLE));
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
