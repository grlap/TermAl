// Sender-to-artifact-to-launcher-summary witnesses; all producer calls are
// scripted and use disposable fixture files, never a live store or a replay.
use super::*;

fn summary(run_dir: &FsPath) -> String {
    let output = crate::host_command::Command::new("node")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/test-launcher.mjs"))
        .arg("summary")
        .arg(run_dir)
        .output()
        .expect("Node can read the fixture summary");
    assert!(
        output.status.success(),
        "{}",
        output.stderr.len()
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn typed_producer_refusal_reaches_the_retained_record_and_actual_summary() {
    let fixture = RunFixture::new("verification-refusal-summary");
    let run = record_run(
        &fixture,
        "test-refusal-summary",
        commit_request(&fixture.root),
        commit_results(),
    );
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");
    read_work_session(&transport, &adapter, &own, supported());
    let wire = json!({"status":"error", "error":{
        "code":"verification_record_refused",
        "message":format!("execution_observe source_id {}", source_projection::source_for_refusal_test()),
        "details":{"field":"root", "reason":"secret-not-for-output", "ignored":"secret-not-for-output"}
    }});
    let EngramControlResponse::Error { error } = serde_json::from_value(wire).unwrap() else {
        panic!("error envelope")
    };
    transport.script(Err(EngramTransportError::remote(error.into_body())));
    let step = engram_verification_record_send(ready(&run.registration), link(&adapter, &own));
    let EngramVerificationRecordStep::Rejected(detail) = &step else {
        panic!("{step:?}")
    };
    assert_eq!(detail, "Engram refused it: verification_record_refused");
    assert!(!detail.contains("source_id"));
    assert!(!detail.contains("execution_observe"));
    assert!(!detail.contains("secret-not-for-output"));
    assert_eq!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Rejected {
            detail: detail.clone()
        })
    );
    let rendered = summary(&run.run_dir);
    assert!(rendered.contains("verification: rejected:"), "{rendered}");
    assert!(
        rendered.contains("verification_record_refused"),
        "{rendered}"
    );
    assert!(!rendered.contains("secret-not-for-output"));
    assert!(!rendered.contains("private-child.txt"));
    assert_eq!(
        transport.sent_operations(),
        ["work_session_read", "verification_record"]
    );
    // A terminal refusal must remain terminal without another producer send.
    register(&fixture, &[&run]);
    fixture.state.note_engram_verification_record_step(&run.registration, &step);
    let stored = current(&fixture, &run);
    assert!(matches!(
        &stored.verification_record,
        Some(TestRunVerificationAck::Rejected { detail: saved }) if saved == detail
    ));
    assert!(matches!(
        engram_verification_record_prepare(&stored),
        EngramVerificationRecordPrepared::Finished
    ));
}

#[test]
fn command_limit_names_both_fields_and_counts_utf8_without_sending_or_rewriting() {
    for field in ["run", "check"] {
        let fixture = RunFixture::new(&format!("verification-command-limit-{field}"));
        let run = record_run(
            &fixture,
            "test-command-limit",
            commit_request(&fixture.root),
            commit_results(),
        );
        let transport = Arc::new(RecordingTransport::default());
        let adapter = adapter(&transport);
        let own = connection(SESSION, "actor");
        read_work_session(&transport, &adapter, &own, supported());
        let mut frozen = ready(&run.registration);
        // A long selector: byte length, not Unicode scalar count, is the contract.
        let command = format!("cargo test {}", "é".repeat(4575));
        let command_bytes = command.len();
        if field == "run" {
            frozen.body.run.command = command.clone();
        } else {
            frozen.body.checks[0].command = command.clone();
        }
        frozen.record = serde_json::to_value(&frozen.body).unwrap();
        let body_path = run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
        let body_before = fs::read(&body_path).unwrap();
        let step = engram_verification_record_send(frozen, link(&adapter, &own));
        let EngramVerificationRecordStep::Rejected(detail) = step else {
            panic!("{step:?}")
        };
        assert!(detail.contains("verification_command_too_long"));
        assert!(detail.contains(&format!("{command_bytes} UTF-8 bytes")));
        assert!(detail.contains("Engram limit is 8192 bytes"));
        assert!(detail.contains(if field == "run" {
            "run.command"
        } else {
            "checks[0].command"
        }));
        assert_eq!(transport.sent_operations(), ["work_session_read"]);
        assert_eq!(
            fs::read(&body_path).unwrap(),
            body_before,
            "do not rewrite the frozen record"
        );
        assert!(summary(&run.run_dir).contains(&detail));
    }
}

#[test]
fn command_at_8192_bytes_still_reaches_the_producer_unchanged() {
    let fixture = RunFixture::new("verification-command-boundary");
    let run = record_run(
        &fixture,
        "test-command-boundary",
        commit_request(&fixture.root),
        commit_results(),
    );
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");
    read_work_session(&transport, &adapter, &own, supported());
    let mut frozen = ready(&run.registration);
    let command = "x".repeat(8192);
    frozen.body.run.command = command.clone();
    frozen.body.checks[0].command = command.clone();
    frozen.record = serde_json::to_value(&frozen.body).unwrap();
    transport.script(Err(EngramTransportError::remote(EngramControlErrorBody {
        code: "verification_record_refused".into(),
        message: "scripted boundary refusal".into(),
    })));
    let _ = engram_verification_record_send(frozen, link(&adapter, &own));
    assert_eq!(
        transport.sent_operations(),
        ["work_session_read", "verification_record"]
    );
    let sent = &transport.sent_records()[0];
    assert_eq!(sent["run"]["command"], command);
    assert_eq!(sent["checks"][0]["command"], command);
}
