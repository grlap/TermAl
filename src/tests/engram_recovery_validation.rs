//! Pure dormant contract controls; this does not activate a recovery producer.
use super::*;

struct ProofFixture {
    _root: TestTempRoot,
    session: Session,
    original: QueuedPromptRecord,
    queue: Vec<QueuedPromptRecord>,
    envelope: EngramRecoveryEnvelope<EngramBeginReplay>,
    capture: Value,
    audit: Value,
}

fn proof_head(id: &str, text: &str) -> QueuedPromptRecord {
    serde_json::from_value(json!({
        "source":"user", "attachments":[],
        "promotion_disposition_known":true,
        "pending_prompt":{"id":id,"timestamp":"2026-10-01T01:02:03Z","text":text}
    }))
    .unwrap()
}

impl ProofFixture {
    fn prepared() -> Self {
        let root = TestTempRoot::create("dormant-proof-contract");
        let mut inner = StateInner::new();
        let session = inner
            .create_session(
                Agent::Codex,
                None,
                root.path().to_string_lossy().into_owned(),
                None,
                None,
            )
            .session;
        let original = proof_head("original-head", "whole first line\nżółw 🐢\nlast line");
        let authority = EngramRecoveryAuthority {
            project_id: "saved-project".to_owned(),
            connection: EngramConnectionConfig {
                binary_path: PathBuf::from("C:/github/Personal/TermAl/.tmp/saved-engram.exe"),
                project_file: PathBuf::from("C:/github/Personal/TermAl/.engram-project"),
                home: PathBuf::from("C:/github/Personal/TermAl/.tmp/saved-home"),
                project_root: PathBuf::from("C:/github/Personal/TermAl"),
                actor_id: "saved-actor".to_owned(),
                actor_context: Some("saved-context".to_owned()),
                session_id: session.id.clone(),
            },
            settings: EngramProjectSettings {
                enabled: true,
                turn_gated_control: true,
                acceptance_evaluation: Some(AcceptanceEvaluatorDefaults {
                    default_mode: Some(AcceptanceEvaluationMode::IndependentSession),
                    evaluator_agent: Some(Agent::Claude),
                    evaluator_model: Some("saved-evaluator".to_owned()),
                }),
                binary_path: Some("saved-binary-override".to_owned()),
                home: Some("saved-home-override".to_owned()),
                work_authority_grant: None,
                authority_store_key: Some(EngramAuthorityStoreKey {
                    project_id: "saved-store-project".to_owned(),
                    database_path: PathBuf::from("C:/github/Personal/TermAl/.tmp/saved.sqlite"),
                }),
                deadline_ms: Some(1234),
            },
            work_binding: Some(EngramControlWorkBinding {
                root_execution_id: "saved-root-execution".to_owned(),
                work_id: "saved-work".to_owned(),
                run_id: "saved-run".to_owned(),
                work_revision: 19,
                claim_id: "saved-claim".to_owned(),
                claim_fence: 27,
            }),
            origin: EngramRecoveryOrigin::ModernBegin,
        };
        let envelope = EngramRecoveryEnvelope {
            schema_version: 1,
            envelope_id: "saved-operation".to_owned(),
            revision: 41,
            authority,
            payload: EngramBeginReplay {
                routing_token: "saved-routing".to_owned(),
                original_head: serde_json::to_value(&original).unwrap(),
                prompt_id: original.pending_prompt.id.clone(),
                fingerprint: engram_turn_intent_fingerprint(
                    &original.pending_prompt.text,
                    original.pending_prompt.expanded_text.as_deref(),
                    &original.attachments,
                    original.pending_prompt.source.as_ref(),
                    original.source,
                ),
                dispatch_generation: 97,
                grant_id: "saved-grant".to_owned(),
                delivery_tokens: vec!["saved-token-one".to_owned(), "saved-token-two".to_owned()],
                idempotency_key: "saved-begin-key".to_owned(),
                grant_basis: json!({"sourceRevision":"saved-source","effects":["observe"]}),
            },
            phase: EngramRecoveryPhase::Prepared,
            settlement: None,
            attempts: 0,
            held_since: "2026-10-01T01:02:03Z".to_owned(),
            due_at: "2026-10-01T01:02:04Z".to_owned(),
        };
        let capture = json!({
            "operationId":envelope.envelope_id, "revision":envelope.revision,
            "sessionId":session.id, "dispatchGeneration":envelope.payload.dispatch_generation,
            "originalHead":original, "grantId":envelope.payload.grant_id,
            "beginKey":envelope.payload.idempotency_key, "authority":envelope.authority
        });
        Self {
            _root: root,
            session,
            queue: vec![original.clone()],
            original,
            envelope,
            capture,
            audit: Value::Null,
        }
    }

    fn cancel(&mut self) {
        self.audit = json!({
            "localCapture":self.capture, "operationId":self.capture["operationId"],
            "revision":41, "cancellationRevision":42,
            "grantId":self.capture["grantId"], "beginKey":self.capture["beginKey"],
            "decision":"operator_cancelled", "promptId":self.original.pending_prompt.id,
            "messageId":"saved-cancel-message", "originalText":self.original.pending_prompt.text
        });
        self.envelope.phase = EngramRecoveryPhase::OperatorCancelled;
        self.envelope.revision = 42;
        self.envelope.settlement = Some(self.audit.clone());
        self.queue = vec![proof_head("successor-head", "successor is preserved")];
        self.session.messages.push(Message::Text {
            attachments: vec![],
            id: "saved-cancel-message".to_owned(),
            timestamp: "2026-10-01T01:02:05Z".to_owned(),
            author: Author::System,
            text: format!(
                "Begin cancelled; resend this message when ready:\n\n{}",
                self.original.pending_prompt.text
            ),
            expanded_text: None,
            source: None,
        });
    }

    fn row(&self) -> EngramRecoveryRawRow<'_> {
        EngramRecoveryRawRow {
            session: &self.session,
            queued_prompts: self.queue.clone(),
            engram_begin_recovery: Some(serde_json::to_value(&self.envelope).unwrap()),
            engram_begin_recovery_audit: json!([]),
            engram_begin_recovery_format: json!(true),
            engram_begin_operation_id: Some(json!(self.envelope.envelope_id)),
            engram_begin_cancel_capture: Some(self.capture.clone()),
            engram_begin_cancel_audit: (!self.audit.is_null()).then(|| self.audit.clone()),
            uncertain_grant: None,
        }
    }
}

#[test]
fn engram_recovery_proof_derived_hold_flags_preserve_authority() {
    let mut fixture = ProofFixture::prepared();
    assert!(verify_engram_local_cancel_capture(
        &fixture.row(),
        &fixture.capture
    ));
    fixture.queue[0].pending_prompt.engram_interrupted = true;
    fixture.queue[0].pending_prompt.is_engram_retained = true;
    fixture.queue[0].engram_interrupted = true;
    fixture.queue[0].engram_waiting = true;
    assert!(
        verify_engram_local_cancel_capture(&fixture.row(), &fixture.capture),
        "derived hold projections must preserve the original cancellation authority"
    );
    let (envelope, _, stored) = decode_engram_begin_storage(&fixture.row());
    assert!(envelope.is_some() && stored.invalid.is_none());
}

#[test]
fn engram_recovery_proof_prepared_envelope_cannot_claim_terminal() {
    let mut fixture = ProofFixture::prepared();
    fixture.cancel();
    assert!(engram_recovery_cancel_terminal(&fixture.row()));
    fixture.envelope.phase = EngramRecoveryPhase::Prepared;
    fixture.envelope.revision = 41;
    fixture.envelope.settlement = None;
    assert!(verify_engram_local_cancel_capture(
        &fixture.row(),
        &fixture.capture
    ));
    assert!(
        !engram_recovery_cancel_terminal(&fixture.row()),
        "an intact Prepared envelope cannot establish an OperatorCancelled transition"
    );
}

#[test]
fn engram_recovery_proof_independent_modern_capture_requires_dispatch() {
    let mut fixture = ProofFixture::prepared();
    fixture.cancel();
    for dispatch in [0, 97] {
        let mut row = fixture.row();
        row.engram_begin_recovery = Some(json!({"damagedTransport":true}));
        let mut capture = row.engram_begin_cancel_capture.clone().unwrap();
        capture["dispatchGeneration"] = json!(dispatch);
        row.engram_begin_cancel_audit.as_mut().unwrap()["localCapture"] = capture.clone();
        row.engram_begin_cancel_capture = Some(capture.clone());
        assert!(verify_engram_local_cancel_capture(&row, &capture));
        assert!(engram_recovery_cancel_terminal(&row));
    }
    let mut wrongly_accepted = Vec::new();
    for (label, value) in [
        ("missing", None),
        ("null", Some(Value::Null)),
        ("string", Some(json!("97"))),
        ("negative", Some(json!(-1))),
        ("fractional", Some(json!(1.5))),
    ] {
        let mut row = fixture.row();
        row.engram_begin_recovery = Some(json!({"damagedTransport":true}));
        let mut capture = row.engram_begin_cancel_capture.clone().unwrap();
        match value {
            Some(value) => capture["dispatchGeneration"] = value,
            None => {
                assert!(
                    capture
                        .as_object_mut()
                        .unwrap()
                        .remove("dispatchGeneration")
                        .is_some()
                );
            }
        }
        row.engram_begin_cancel_audit.as_mut().unwrap()["localCapture"] = capture.clone();
        row.engram_begin_cancel_capture = Some(capture.clone());
        if verify_engram_local_cancel_capture(&row, &capture) {
            wrongly_accepted.push(format!("{label}:direct"));
        }
        if engram_recovery_cancel_terminal(&row) {
            wrongly_accepted.push(format!("{label}:terminal"));
        }
    }
    assert!(
        wrongly_accepted.is_empty(),
        "invalid dispatch accepted: {wrongly_accepted:?}"
    );
}

#[test]
fn engram_recovery_proof_empty_replay_identities_rejected_without_capture() {
    let fixture = ProofFixture::prepared();
    let mut valid = fixture.row();
    valid.engram_begin_cancel_capture = None;
    valid.engram_begin_cancel_audit = None;
    assert!(valid.uncertain_grant.is_none());
    let (accepted, _, stored) = decode_engram_begin_storage(&valid);
    assert!(accepted.is_some() && stored.invalid.is_none());
    let mut wrongly_accepted = Vec::new();
    for field in ["grantId", "idempotencyKey"] {
        let mut row = fixture.row();
        row.engram_begin_cancel_capture = None;
        row.engram_begin_cancel_audit = None;
        row.engram_begin_recovery.as_mut().unwrap()["payload"][field] = json!("");
        let raw = row.engram_begin_recovery.clone();
        let (accepted, _, stored) = decode_engram_begin_storage(&row);
        assert_eq!(
            stored.raw_envelope, raw,
            "raw rejected identity must survive"
        );
        if accepted.is_some()
            || !matches!(stored.invalid, Some(EngramRecoveryDataInvalid::Envelope(_)))
        {
            wrongly_accepted.push(field);
        }
    }
    assert!(
        wrongly_accepted.is_empty(),
        "empty identity accepted: {wrongly_accepted:?}"
    );
}

fn proof_raw_image(row: &EngramRecoveryRawRow<'_>) -> Value {
    json!({"session":row.session,"queue":row.queued_prompts,
        "envelope":row.engram_begin_recovery,"audit":row.engram_begin_recovery_audit,
        "format":row.engram_begin_recovery_format,"operation":row.engram_begin_operation_id,
        "capture":row.engram_begin_cancel_capture,"cancelAudit":row.engram_begin_cancel_audit,
        "uncertain":row.uncertain_grant})
}

fn proof_assert_raw_preserved(row: &EngramRecoveryRawRow<'_>) {
    let before = proof_raw_image(row);
    let (_, _, stored) = decode_engram_begin_storage(row);
    assert_eq!(stored.raw_envelope, row.engram_begin_recovery);
    assert_eq!(stored.raw_audit, row.engram_begin_recovery_audit);
    assert_eq!(stored.raw_cancel_audit, row.engram_begin_cancel_audit);
    assert_eq!(stored.raw_format, row.engram_begin_recovery_format);
    assert_eq!(stored.operation_id, row.engram_begin_operation_id);
    assert_eq!(stored.cancel_capture, row.engram_begin_cancel_capture);
    assert_eq!(stored.raw_uncertain_grant, row.uncertain_grant);
    assert_eq!(
        proof_raw_image(row),
        before,
        "pure decoding must not mutate input"
    );
}

#[test]
fn engram_recovery_proof_raw_evidence_and_first_diagnostic_survive() {
    let fixture = ProofFixture::prepared();
    let (accepted, _, stored) = decode_engram_begin_storage(&fixture.row());
    assert!(accepted.is_some() && stored.invalid.is_none());
    for field in [
        "envelope",
        "audit",
        "format",
        "operation",
        "capture",
        "cancelAudit",
        "uncertain",
    ] {
        let mut row = fixture.row();
        let malformed = json!({"originalBytes":"żółw\nretained","wrongType":true});
        match field {
            "envelope" => row.engram_begin_recovery = Some(malformed),
            "audit" => row.engram_begin_recovery_audit = malformed,
            "format" => row.engram_begin_recovery_format = malformed,
            "operation" => row.engram_begin_operation_id = Some(malformed),
            "capture" => row.engram_begin_cancel_capture = Some(malformed),
            "cancelAudit" => row.engram_begin_cancel_audit = Some(malformed),
            "uncertain" => row.uncertain_grant = Some(malformed),
            _ => unreachable!(),
        }
        proof_assert_raw_preserved(&row);
        if field != "cancelAudit" {
            assert!(
                decode_engram_begin_storage(&row).2.invalid.is_some(),
                "{field}"
            );
        } else {
            assert!(!engram_recovery_cancel_terminal(&row));
        }
    }
    let mut row = fixture.row();
    row.engram_begin_recovery = Some(json!({"damagedEnvelope":true}));
    row.engram_begin_recovery_audit = json!({"damagedAudit":true});
    row.engram_begin_recovery_format = json!("damagedFormat");
    row.engram_begin_operation_id = Some(json!(17));
    row.engram_begin_cancel_capture = Some(json!({"damagedCapture":true}));
    let (_, audit, stored) = decode_engram_begin_storage(&row);
    assert!(audit.is_empty());
    match stored.invalid {
        Some(EngramRecoveryDataInvalid::Envelope(reason)) => assert!(!reason.is_empty()),
        other => panic!("earliest envelope diagnostic lost: {other:?}"),
    }
    proof_assert_raw_preserved(&row);
    let mut audit_first = fixture.row();
    audit_first.engram_begin_recovery_audit = json!({"damagedAudit":true});
    audit_first.engram_begin_recovery_format = json!("damagedFormat");
    assert!(matches!(
        decode_engram_begin_storage(&audit_first).2.invalid,
        Some(EngramRecoveryDataInvalid::Audit)
    ));
}

#[test]
fn engram_recovery_proof_single_field_envelope_and_capture_identity_mismatches() {
    let fixture = ProofFixture::prepared();
    assert!(decode_engram_begin_storage(&fixture.row()).0.is_some());
    for (pointer, wrong) in [
        ("/schemaVersion", json!(2)),
        ("/envelopeId", json!("other-operation")),
        ("/revision", json!(40)),
        ("/authority/connection/session_id", json!("other-session")),
        ("/authority/projectId", json!("other-project")),
        ("/payload/grantId", json!("other-grant")),
        ("/payload/idempotencyKey", json!("other-key")),
        ("/payload/promptId", json!("other-head")),
        (
            "/payload/fingerprint",
            json!("wrong-production-fingerprint"),
        ),
        ("/payload/dispatchGeneration", json!(98)),
    ] {
        let mut row = fixture.row();
        *row.engram_begin_recovery
            .as_mut()
            .unwrap()
            .pointer_mut(pointer)
            .unwrap() = wrong;
        let (accepted, _, stored) = decode_engram_begin_storage(&row);
        assert!(accepted.is_none() && stored.invalid.is_some(), "{pointer}");
        assert!(
            !verify_engram_local_cancel_capture(&row, &fixture.capture),
            "{pointer}"
        );
        proof_assert_raw_preserved(&row);
    }
    for (pointer, wrong) in [
        ("/operationId", json!("other-operation")),
        ("/revision", json!(40)),
        ("/sessionId", json!("other-session")),
        ("/grantId", json!("other-grant")),
        ("/beginKey", json!("other-key")),
        ("/dispatchGeneration", json!(98)),
        ("/authority/projectId", json!("other-project")),
        ("/authority/connection/session_id", json!("other-session")),
        (
            "/authority/settings/authorityStoreKey/projectId",
            json!("other-store"),
        ),
    ] {
        let mut row = fixture.row();
        let mut capture = fixture.capture.clone();
        *capture.pointer_mut(pointer).unwrap() = wrong;
        row.engram_begin_cancel_capture = Some(capture.clone());
        assert!(
            !verify_engram_local_cancel_capture(&row, &capture),
            "{pointer}"
        );
        assert!(
            matches!(
                decode_engram_begin_storage(&row).2.invalid,
                Some(EngramRecoveryDataInvalid::CancellationTuple)
            ),
            "{pointer}"
        );
        proof_assert_raw_preserved(&row);
    }
    let mut marker = fixture.row();
    marker.engram_begin_operation_id = Some(json!("another-operation"));
    assert!(decode_engram_begin_storage(&marker).0.is_none());
    assert!(!verify_engram_local_cancel_capture(
        &marker,
        &fixture.capture
    ));
}

#[test]
fn engram_recovery_proof_each_immutable_head_field_is_bound_independently() {
    let fixture = ProofFixture::prepared();
    let metadata = json!({"byteSize":3,"fileName":"different.png","mediaType":"image/png"});
    let changes = [
        ("/pending_prompt/id", json!("different-head")),
        ("/pending_prompt/timestamp", json!("2026-10-02T01:02:03Z")),
        ("/pending_prompt/text", json!("same id different text")),
        (
            "/pending_prompt/expandedText",
            json!("different expanded text"),
        ),
        ("/pending_prompt/attachments", json!([metadata.clone()])),
        (
            "/pending_prompt/source",
            json!({"sessionId":"peer","name":"different-peer"}),
        ),
        (
            "/attachments",
            json!([{"data":"changed-data","metadata":metadata}]),
        ),
        ("/source", json!("orchestrator")),
    ];
    for (pointer, wrong) in changes {
        for target in ["queue", "saved-envelope", "capture"] {
            // An absent old id may leave a successor queue after cancellation;
            // it is not a same-id substitution of immutable authorization input.
            if target == "queue" && pointer == "/pending_prompt/id" {
                continue;
            }
            let mut row = fixture.row();
            let mut head = serde_json::to_value(&fixture.original).unwrap();
            let (parent, field) = pointer.rsplit_once('/').unwrap();
            head.pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), wrong.clone());
            match target {
                "queue" => row.queued_prompts[0] = serde_json::from_value(head).unwrap(),
                "saved-envelope" => {
                    row.engram_begin_recovery.as_mut().unwrap()["payload"]["originalHead"] = head
                }
                "capture" => {
                    row.engram_begin_cancel_capture.as_mut().unwrap()["originalHead"] = head
                }
                _ => unreachable!(),
            }
            assert!(
                !verify_engram_local_cancel_capture(
                    &row,
                    row.engram_begin_cancel_capture.as_ref().unwrap()
                ),
                "{target} {pointer}"
            );
            assert!(
                decode_engram_begin_storage(&row).0.is_none(),
                "{target} {pointer}"
            );
            proof_assert_raw_preserved(&row);
        }
    }
}

#[test]
fn engram_recovery_proof_promotion_and_hold_projections_are_not_authorization_inputs() {
    let fixture = ProofFixture::prepared();
    let mut row = fixture.row();
    row.queued_prompts[0].promotion_disposition_known = false;
    row.queued_prompts[0].promoted_message_index = Some(123);
    row.queued_prompts[0].engram_waiting = true;
    row.queued_prompts[0].engram_interrupted = true;
    row.queued_prompts[0].pending_prompt.engram_interrupted = true;
    row.queued_prompts[0].pending_prompt.is_engram_retained = true;
    assert!(verify_engram_local_cancel_capture(&row, &fixture.capture));
    assert!(decode_engram_begin_storage(&row).0.is_some());
    proof_assert_raw_preserved(&row);
}

#[test]
fn engram_recovery_proof_mailbox_same_identity_text_cannot_replace_original() {
    let mut fixture = ProofFixture::prepared();
    fixture.original.source = QueuedPromptSource::Mailbox;
    fixture.original.pending_prompt.source = Some(MessageSource::mailbox(
        "sender-session".to_owned(),
        "sender".to_owned(),
        MailboxMessageSource {
            mailbox_id: "original-mailbox".to_owned(),
            message_id: "original-message".to_owned(),
            sequence: 17,
            unread_count: 1,
        },
    ));
    fixture.queue = vec![fixture.original.clone()];
    fixture.envelope.payload.original_head = json!(fixture.original);
    fixture.envelope.payload.fingerprint = engram_turn_intent_fingerprint(
        &fixture.original.pending_prompt.text,
        None,
        &[],
        fixture.original.pending_prompt.source.as_ref(),
        QueuedPromptSource::Mailbox,
    );
    fixture.capture["originalHead"] = json!(fixture.original);
    assert!(verify_engram_local_cancel_capture(
        &fixture.row(),
        &fixture.capture
    ));
    assert!(decode_engram_begin_storage(&fixture.row()).0.is_some());
    fixture.queue[0].pending_prompt.text = "changed same-id mailbox text".to_owned();
    assert_eq!(
        engram_turn_intent_fingerprint(
            &fixture.queue[0].pending_prompt.text,
            None,
            &[],
            fixture.queue[0].pending_prompt.source.as_ref(),
            QueuedPromptSource::Mailbox
        ),
        fixture.envelope.payload.fingerprint,
        "real mailbox fingerprint alone cannot see changed text"
    );
    assert!(!verify_engram_local_cancel_capture(
        &fixture.row(),
        &fixture.capture
    ));
    assert!(decode_engram_begin_storage(&fixture.row()).0.is_none());
}

#[test]
fn engram_recovery_proof_historical_and_legacy_evidence_cannot_bypass_modern_data() {
    let mut fixture = ProofFixture::prepared();
    let mut historical = fixture.row();
    historical.engram_begin_recovery = None;
    historical.engram_begin_recovery_audit = Value::Null;
    historical.engram_begin_recovery_format = Value::Null;
    historical.engram_begin_operation_id = None;
    historical.engram_begin_cancel_capture = None;
    historical.engram_begin_cancel_audit = None;
    historical.uncertain_grant = Some(json!("historical-grant"));
    let (envelope, _, stored) = decode_engram_begin_storage(&historical);
    assert!(envelope.is_none() && stored.invalid.is_none() && !stored.has_modern_evidence());
    assert_eq!(stored.raw_uncertain_grant, historical.uncertain_grant);
    fixture.cancel();
    let mut legacy_capture = fixture.capture.clone();
    legacy_capture["origin"] = json!("legacy_local_cancel");
    for field in ["grantId", "beginKey", "authority", "dispatchGeneration"] {
        assert!(
            legacy_capture
                .as_object_mut()
                .unwrap()
                .remove(field)
                .is_some()
        );
    }
    let mut legacy = fixture.row();
    legacy.engram_begin_recovery = None;
    legacy.engram_begin_recovery_format = Value::Null;
    legacy.engram_begin_recovery_audit = Value::Null;
    legacy.engram_begin_cancel_capture = Some(legacy_capture.clone());
    legacy.engram_begin_cancel_audit.as_mut().unwrap()["localCapture"] = legacy_capture.clone();
    for field in ["grantId", "beginKey"] {
        assert!(
            legacy
                .engram_begin_cancel_audit
                .as_mut()
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(field)
                .is_some()
        );
    }
    assert!(verify_engram_local_cancel_capture(&legacy, &legacy_capture));
    assert!(engram_recovery_cancel_terminal(&legacy));
    assert!(decode_engram_begin_storage(&legacy).2.invalid.is_none());
    assert!(
        decode_engram_begin_storage(&legacy).0.is_none(),
        "legacy must not invent transport"
    );
    for specimen in [
        "intact",
        "malformed",
        "present-null",
        "format-true",
        "format-false",
        "format-malformed",
        "audit-modern",
        "audit-malformed",
    ] {
        let mut row = EngramRecoveryRawRow {
            session: legacy.session,
            queued_prompts: legacy.queued_prompts.clone(),
            engram_begin_recovery: None,
            engram_begin_recovery_audit: Value::Null,
            engram_begin_recovery_format: Value::Null,
            engram_begin_operation_id: legacy.engram_begin_operation_id.clone(),
            engram_begin_cancel_capture: Some(legacy_capture.clone()),
            engram_begin_cancel_audit: legacy.engram_begin_cancel_audit.clone(),
            uncertain_grant: None,
        };
        match specimen {
            "intact" => row.engram_begin_recovery = Some(json!(fixture.envelope)),
            "malformed" => row.engram_begin_recovery = Some(json!({"transportDamage":true})),
            "present-null" => row.engram_begin_recovery = Some(Value::Null),
            "format-true" => row.engram_begin_recovery_format = json!(true),
            "format-false" => row.engram_begin_recovery_format = json!(false),
            "format-malformed" => row.engram_begin_recovery_format = json!("modern"),
            "audit-modern" => row.engram_begin_recovery_audit = json!([fixture.envelope]),
            "audit-malformed" => row.engram_begin_recovery_audit = json!({"modern":true}),
            _ => unreachable!(),
        }
        assert!(
            !verify_engram_local_cancel_capture(&row, &legacy_capture),
            "{specimen}"
        );
        assert!(!engram_recovery_cancel_terminal(&row), "{specimen}");
        assert!(
            decode_engram_begin_storage(&row).2.invalid.is_some(),
            "{specimen}"
        );
        proof_assert_raw_preserved(&row);
    }
}

#[test]
fn engram_recovery_proof_independent_capture_binds_uncertain_grant_and_session() {
    let mut fixture = ProofFixture::prepared();
    fixture.cancel();
    for grant in [None, Some(json!("saved-grant"))] {
        let mut row = fixture.row();
        row.engram_begin_recovery = Some(json!({"transportDamage":true}));
        row.uncertain_grant = grant;
        assert!(verify_engram_local_cancel_capture(&row, &fixture.capture));
        assert!(engram_recovery_cancel_terminal(&row));
        assert!(
            decode_engram_begin_storage(&row).0.is_none(),
            "local proof grants no replay authority"
        );
    }
    for grant in [
        json!("other-grant"),
        Value::Null,
        json!(17),
        json!({"grant":"saved-grant"}),
    ] {
        let mut row = fixture.row();
        row.engram_begin_recovery = Some(json!({"transportDamage":true}));
        row.uncertain_grant = Some(grant);
        assert!(!verify_engram_local_cancel_capture(&row, &fixture.capture));
        assert!(!engram_recovery_cancel_terminal(&row));
        proof_assert_raw_preserved(&row);
    }
    let mut row = fixture.row();
    row.engram_begin_recovery = Some(json!({"transportDamage":true}));
    let mut capture = fixture.capture.clone();
    capture["authority"]["connection"]["session_id"] = json!("other-session");
    row.engram_begin_cancel_capture = Some(capture.clone());
    row.engram_begin_cancel_audit.as_mut().unwrap()["localCapture"] = capture.clone();
    assert!(!verify_engram_local_cancel_capture(&row, &capture));
    assert!(!engram_recovery_cancel_terminal(&row));
}

#[test]
fn engram_recovery_proof_cancel_transition_rejects_stale_and_cross_operation_evidence() {
    let mut fixture = ProofFixture::prepared();
    let stale = EngramRecoveryAttempt {
        envelope_id: fixture.envelope.envelope_id.clone(),
        revision: fixture.envelope.revision,
        attempt_id: "old-attempt".to_owned(),
        authority: fixture.envelope.authority.clone(),
    };
    assert!(!engram_recovery_cancel_terminal(&fixture.row()));
    fixture.cancel();
    assert_eq!(stale.envelope_id, fixture.envelope.envelope_id);
    assert_eq!(
        stale.revision.checked_add(1),
        Some(fixture.envelope.revision)
    );
    assert_ne!(stale.revision, fixture.envelope.revision);
    assert!(engram_recovery_cancel_terminal(&fixture.row()));
    for (pointer, wrong) in [
        ("/operationId", json!("cross-operation")),
        ("/revision", json!(40)),
        ("/cancellationRevision", json!(43)),
        ("/grantId", json!("cross-grant")),
        ("/beginKey", json!("cross-key")),
        ("/promptId", json!("successor-head")),
        ("/messageId", json!("successor-message")),
        ("/originalText", json!("partial text")),
        ("/decision", json!("cancel-requested")),
        ("/localCapture/revision", json!(40)),
    ] {
        let mut row = fixture.row();
        *row.engram_begin_cancel_audit
            .as_mut()
            .unwrap()
            .pointer_mut(pointer)
            .unwrap() = wrong;
        assert!(!engram_recovery_cancel_terminal(&row), "{pointer}");
        proof_assert_raw_preserved(&row);
    }
    for change in [
        "stale-revision",
        "wrong-phase",
        "wrong-settlement",
        "wrong-operation",
        "old-head-present",
    ] {
        let mut row = fixture.row();
        match change {
            "stale-revision" => row.engram_begin_recovery.as_mut().unwrap()["revision"] = json!(41),
            "wrong-phase" => {
                row.engram_begin_recovery.as_mut().unwrap()["phase"] = json!("Delivered")
            }
            "wrong-settlement" => {
                row.engram_begin_recovery.as_mut().unwrap()["settlement"] = Value::Null
            }
            "wrong-operation" => row.engram_begin_operation_id = Some(json!("other-operation")),
            "old-head-present" => row.queued_prompts.push(fixture.original.clone()),
            _ => unreachable!(),
        }
        assert!(!engram_recovery_cancel_terminal(&row), "{change}");
    }
    let mut overflow = fixture.row();
    overflow.engram_begin_recovery = Some(json!({"transportDamage":true}));
    overflow.engram_begin_cancel_capture.as_mut().unwrap()["revision"] = json!(u64::MAX);
    let capture = overflow.engram_begin_cancel_capture.clone().unwrap();
    overflow.engram_begin_cancel_audit.as_mut().unwrap()["localCapture"] = capture;
    overflow.engram_begin_cancel_audit.as_mut().unwrap()["revision"] = json!(u64::MAX);
    assert!(!engram_recovery_cancel_terminal(&overflow));
    assert_eq!(fixture.queue[0].pending_prompt.id, "successor-head");
}

#[test]
fn engram_recovery_proof_terminal_requires_exact_identified_message_and_preserves_successor() {
    for change in ["absent", "wrong-id", "partial-text", "successor-text"] {
        let mut fixture = ProofFixture::prepared();
        fixture.cancel();
        assert!(engram_recovery_cancel_terminal(&fixture.row()));
        match change {
            "absent" => fixture.session.messages.clear(),
            "wrong-id" => match &mut fixture.session.messages[0] {
                Message::Text { id, .. } => *id = "other-message".to_owned(),
                _ => unreachable!(),
            },
            "partial-text" | "successor-text" => match &mut fixture.session.messages[0] {
                Message::Text { text, .. } => {
                    *text = if change == "partial-text" {
                        "Begin cancelled; resend this message when ready:\n\nwhole first line"
                            .to_owned()
                    } else {
                        "Begin cancelled; resend this message when ready:\n\nsuccessor is preserved"
                            .to_owned()
                    }
                }
                _ => unreachable!(),
            },
            _ => unreachable!(),
        }
        let before = proof_raw_image(&fixture.row());
        assert!(!engram_recovery_cancel_terminal(&fixture.row()), "{change}");
        assert_eq!(proof_raw_image(&fixture.row()), before);
        assert_eq!(fixture.queue.len(), 1);
        assert_eq!(
            fixture.queue[0].pending_prompt.text,
            "successor is preserved"
        );
    }
}

#[test]
fn engram_recovery_proof_cancel_readback_uses_real_serializer_and_complete_persisted_input() {
    let mut fixture = ProofFixture::prepared();
    fixture.cancel();
    for index in 1..130 {
        fixture.session.messages.push(Message::Text {
            attachments: Vec::new(),
            id: format!("later-{index}"),
            timestamp: "2026-10-01T01:02:06Z".to_owned(),
            author: Author::Assistant,
            text: format!("later message {index}"),
            expanded_text: None,
            source: None,
        });
    }
    fixture.session.message_count = 130;
    fixture.session.messages_loaded = true;
    let mut inner = StateInner::new();
    let id = inner
        .create_session(
            Agent::Codex,
            None,
            fixture._root.path().to_string_lossy().into_owned(),
            None,
            None,
        )
        .session
        .id;
    {
        let index = inner.find_session_index(&id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session = fixture.session.clone();
        record.queued_prompts = fixture.queue.clone().into();
        record.message_start_index = 0;
        record.message_positions = build_message_positions(&record.session.messages);
    }
    let path = fixture._root.path().join("proof.sqlite");
    persist_persisted_state_to_sqlite(&path, &PersistedState::from_inner(&inner)).unwrap();
    let connection = open_sqlite_state_connection(&path).unwrap();
    let encoded: String = connection
        .query_row(
            "SELECT value_json FROM sessions WHERE id=?1",
            [&fixture.session.id],
            |row| row.get(0),
        )
        .unwrap();
    let mut metadata: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(metadata["session"]["messages"], json!([]));
    assert_eq!(metadata["queuedPrompts"], json!(fixture.queue));
    // Only future dormant specimens are added to the real serialized row.
    metadata["engramBeginRecovery"] = json!(fixture.envelope);
    metadata["engramBeginRecoveryAudit"] = json!([]);
    metadata["engramBeginRecoveryFormat"] = json!(true);
    metadata["engramBeginOperationId"] = json!(fixture.envelope.envelope_id);
    metadata["engramBeginCancelCapture"] = fixture.capture.clone();
    metadata["engramBeginCancelAudit"] = fixture.audit.clone();
    connection
        .execute(
            "UPDATE sessions SET value_json=?1 WHERE id=?2",
            rusqlite::params![
                serde_json::to_string(&metadata).unwrap(),
                &fixture.session.id
            ],
        )
        .unwrap();
    let read_metadata = || {
        let encoded: String = connection
            .query_row(
                "SELECT value_json FROM sessions WHERE id=?1",
                [&fixture.session.id],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str::<Value>(&encoded).unwrap()
    };
    let read_messages = || {
        let mut statement = connection
            .prepare("SELECT value_json FROM messages WHERE session_id=?1 ORDER BY position")
            .unwrap();
        statement
            .query_map([&fixture.session.id], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|row| serde_json::from_str::<Message>(&row.unwrap()).unwrap())
            .collect::<Vec<_>>()
    };
    let raw = read_metadata();
    let complete = read_messages();
    assert_eq!(complete.len(), 130);
    assert_eq!(json!(complete), json!(fixture.session.messages));
    let mut resident: PersistedSessionRecord = serde_json::from_value(raw.clone()).unwrap();
    load_persisted_session_tail(&connection, &path, &mut resident).unwrap();
    assert_eq!(resident.session.messages.len(), 64);
    assert!(
        !resident
            .session
            .messages
            .iter()
            .any(|message| message.id() == "saved-cancel-message")
    );
    let mut row = fixture.row();
    row.queued_prompts = serde_json::from_value(raw["queuedPrompts"].clone()).unwrap();
    row.engram_begin_recovery = Some(raw["engramBeginRecovery"].clone());
    row.engram_begin_cancel_capture = Some(raw["engramBeginCancelCapture"].clone());
    row.engram_begin_cancel_audit = Some(raw["engramBeginCancelAudit"].clone());
    row.session = &resident.session;
    assert!(
        !engram_recovery_cancel_terminal(&row),
        "a partial tail cannot establish terminal durable truth"
    );
    let mut hydrated = resident.session.clone();
    hydrated.messages = complete;
    hydrated.messages_loaded = true;
    row.session = &hydrated;
    assert!(engram_recovery_cancel_terminal(&row));
    let (decoded, _, stored) = decode_engram_begin_storage(&row);
    assert!(stored.invalid.is_none());
    assert_eq!(decoded.unwrap().revision, 42);
    let content = engram_recovery_image_from_row(&raw, json!(hydrated.messages));
    let target = PersistFenceTarget::EngramRecovery {
        session_id: fixture.session.id.clone(),
        content,
    };
    assert!(target.is_already_durable(&connection).unwrap());
    connection
        .execute(
            "DELETE FROM messages WHERE session_id=?1 AND message_id=?2",
            rusqlite::params![&fixture.session.id, "saved-cancel-message"],
        )
        .unwrap();
    assert!(!target.is_already_durable(&connection).unwrap());
    let mut after_deletion = hydrated.clone();
    after_deletion.messages = read_messages();
    let mut deleted_row = fixture.row();
    deleted_row.session = &after_deletion;
    assert!(!engram_recovery_cancel_terminal(&deleted_row));
    assert_eq!(row.queued_prompts.len(), 1);
    assert_eq!(row.queued_prompts[0].pending_prompt.id, "successor-head");
    assert_eq!(read_metadata()["queuedPrompts"], json!(fixture.queue));
}

#[test]
fn engram_recovery_proof_decode_cancel_audit_coherence() {
    let prepared = ProofFixture::prepared();
    let (envelope, _, stored) = decode_engram_begin_storage(&prepared.row());
    assert!(envelope.is_some() && stored.invalid.is_none());
    let mut without_capture = prepared.row();
    without_capture.engram_begin_cancel_capture = None;
    let (envelope, _, stored) = decode_engram_begin_storage(&without_capture);
    assert!(envelope.is_some() && stored.invalid.is_none());

    let mut cancelled = ProofFixture::prepared();
    cancelled.cancel();
    let (envelope, _, stored) = decode_engram_begin_storage(&cancelled.row());
    assert!(envelope.is_some() && stored.invalid.is_none());
    assert!(engram_recovery_cancel_terminal(&cancelled.row()));
    let mut damaged_transport = cancelled.row();
    damaged_transport.engram_begin_recovery = Some(json!({"damagedTransport": true}));
    let (envelope, _, stored) = decode_engram_begin_storage(&damaged_transport);
    assert!(envelope.is_none());
    assert!(matches!(
        stored.invalid,
        Some(EngramRecoveryDataInvalid::Envelope(_))
    ));
    assert!(verify_engram_local_cancel_capture(
        &damaged_transport,
        &cancelled.capture
    ));
    assert!(engram_recovery_cancel_terminal(&damaged_transport));
    proof_assert_raw_preserved(&damaged_transport);

    let mut wrongly_accepted = Vec::new();
    for label in [
        "prepared-with-audit",
        "prepared-with-malformed-audit",
        "cancelled-without-capture",
        "cancelled-without-audit",
        "cancelled-without-both",
        "missing-settlement",
        "wrong-settlement",
        "localCapture",
        "operationId",
        "revision",
        "cancellationRevision",
        "grantId",
        "beginKey",
        "originalText",
        "promptId",
        "messageId",
        "decision",
    ] {
        let mut row = cancelled.row();
        match label {
            "prepared-with-audit" | "prepared-with-malformed-audit" => {
                let envelope = row.engram_begin_recovery.as_mut().unwrap();
                envelope["phase"] = json!("Prepared");
                envelope["revision"] = json!(41);
                envelope["settlement"] = Value::Null;
                if label == "prepared-with-malformed-audit" {
                    row.engram_begin_cancel_audit = Some(json!("malformed audit"));
                }
            }
            "cancelled-without-capture" => row.engram_begin_cancel_capture = None,
            "cancelled-without-audit" => row.engram_begin_cancel_audit = None,
            "cancelled-without-both" => {
                row.engram_begin_cancel_capture = None;
                row.engram_begin_cancel_audit = None;
            }
            "missing-settlement" => {
                row.engram_begin_recovery.as_mut().unwrap()["settlement"] = Value::Null;
            }
            "wrong-settlement" => {
                row.engram_begin_recovery.as_mut().unwrap()["settlement"] = json!({"other": true});
            }
            field => {
                let audit = row.engram_begin_cancel_audit.as_mut().unwrap();
                audit[field] = match field {
                    "localCapture" => json!({"other": true}),
                    "revision" | "cancellationRevision" => json!(99),
                    "messageId" => json!(""),
                    _ => json!("other"),
                };
                // Keep settlement equal to the changed audit, isolating its capture binding.
                row.engram_begin_recovery.as_mut().unwrap()["settlement"] = audit.clone();
            }
        }
        proof_assert_raw_preserved(&row);
        let (envelope, _, stored) = decode_engram_begin_storage(&row);
        if envelope.is_some()
            || !matches!(
                stored.invalid,
                Some(EngramRecoveryDataInvalid::CancellationTuple)
            )
        {
            wrongly_accepted.push(label);
        }
    }
    assert!(
        wrongly_accepted.is_empty(),
        "cancel audit contradictions accepted: {wrongly_accepted:?}"
    );
}

#[test]
fn engram_recovery_proof_cancel_audit_keeps_earlier_diagnostics() {
    let mut fixture = ProofFixture::prepared();
    fixture.cancel();
    fixture.envelope.phase = EngramRecoveryPhase::Prepared;
    fixture.envelope.revision = 41;
    fixture.envelope.settlement = None;

    let mut invalid_audit = fixture.row();
    invalid_audit.engram_begin_recovery_audit = json!("malformed recovery audit");
    let (envelope, _, stored) = decode_engram_begin_storage(&invalid_audit);
    assert!(envelope.is_none());
    assert!(matches!(
        stored.invalid,
        Some(EngramRecoveryDataInvalid::Audit)
    ));
    proof_assert_raw_preserved(&invalid_audit);

    let mut invalid_envelope = fixture.row();
    invalid_envelope.engram_begin_recovery = Some(json!({"damagedTransport": true}));
    let (envelope, _, stored) = decode_engram_begin_storage(&invalid_envelope);
    assert!(envelope.is_none());
    assert!(matches!(
        stored.invalid,
        Some(EngramRecoveryDataInvalid::Envelope(_))
    ));
    proof_assert_raw_preserved(&invalid_envelope);
}
