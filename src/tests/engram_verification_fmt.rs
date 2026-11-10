//! The maintained formatter stage is explicit Lint, without test credit.
use super::*;

const SIX_STAGES: [&str; 6] = [
    "fmt",
    "cargo-check",
    "typescript",
    "fingerprint-tests",
    "rust-tests",
    "vitest",
];

fn six_stage_request(root: &FsPath, argv: &[&str]) -> Value {
    let mut request = commit_request(root);
    request["stages"].as_array_mut().unwrap().insert(
        0,
        json!({"name":"fmt", "command":argv[0], "args":&argv[1..]}),
    );
    request
}

fn six_stage_results(outcome: &str, argv: &[&str]) -> Value {
    let mut results = commit_results();
    let passes = outcome == "passed";
    for (index, entry) in results["stages"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        if passes {
            entry["state"] = json!("passed");
            entry["code"] = json!(0);
            entry["started"] = json!(format!("2026-10-30T10:0{}:00.000Z", index + 1));
            entry["ended"] = json!(format!("2026-10-30T10:0{}:30.000Z", index + 1));
        } else {
            // A failed or unrun first stage prevents all later execution.
            *entry = json!({"name":entry["name"], "state":"unrun"});
        }
    }
    let formatter = if outcome == "unrun" {
        // No observed argv: classification must use the planned command.
        json!({"name":"fmt", "state":"unrun"})
    } else {
        stage("fmt", outcome, if passes { 0 } else { 1 }, argv, 0)
    };
    results["stages"]
        .as_array_mut()
        .unwrap()
        .insert(0, formatter);
    results["state"] = json!(if passes { "passed" } else { "failed" });
    results["exitCode"] = json!(if passes { 0 } else { 1 });
    results["ended"] = json!("2026-10-30T10:06:00.000Z");
    results
}

#[test]
fn six_stage_full_gate_sends_fmt_as_lint_for_pass_failure_and_not_run() {
    for outcome in ["passed", "failed", "unrun"] {
        let fixture = RunFixture::new(&format!("verification-fmt-{outcome}"));
        let argv = ["cargo", "fmt", "--check"];
        let mut run = record_run(
            &fixture,
            &format!("test-fmt-{outcome}"),
            six_stage_request(&fixture.root, &argv),
            six_stage_results(outcome, &argv),
        );
        run.registration.stage_names = SIX_STAGES.iter().map(|name| (*name).to_owned()).collect();
        let frozen = ready(&run.registration);
        let checks = frozen.record["checks"].as_array().unwrap();
        assert_eq!(checks.len(), 6);
        assert_eq!(checks[0]["check_kind"], "lint");
        assert_eq!(checks[0]["command"], "cargo fmt --check");
        assert_eq!(
            checks[0]["check_fingerprint"],
            fingerprint("cargo fmt --check")
        );
        assert_eq!(
            checks[0]["outcome"],
            if outcome == "unrun" {
                "not_run"
            } else {
                outcome
            }
        );
        assert!(checks[0].get("counts").is_none());
        for (check, kind) in checks[1..]
            .iter()
            .zip(["build", "build", "test", "test", "test"])
        {
            assert_eq!(check["check_kind"], kind);
        }
        let producers =
            engram_verification_record_producers(&run.registration.run_id, &frozen.body);
        assert_eq!(
            producers
                .iter()
                .map(|(stage, _)| stage.as_str())
                .collect::<Vec<_>>(),
            if outcome == "passed" {
                vec!["fingerprint-tests", "rust-tests", "vitest"]
            } else {
                vec![]
            },
            "Lint must never create a Test observation",
        );

        let completed = match outcome {
            "passed" => 6,
            "failed" => 1,
            _ => 0,
        };
        let verifications = SIX_STAGES
            .iter()
            .take(completed)
            .enumerate()
            .map(|(index, stage)| json!({"stage":stage, "record_id":format!("{:032x}", index + 1)}))
            .collect::<Vec<_>>();
        let transport = Arc::new(RecordingTransport::default());
        let adapter = adapter(&transport);
        let own = connection(SESSION, "actor");
        read_work_session(&transport, &adapter, &own, supported());
        transport.script(Ok(json!({
            "operation":PRODUCER, "producer":PRODUCER, "verifications":verifications,
            "run_outcome":if outcome == "passed" { "passed" } else { "failed" },
        })));
        let step = engram_verification_record_send(frozen, link(&adapter, &own));
        assert!(
            matches!(step, EngramVerificationRecordStep::Received { .. }),
            "{step:?}"
        );
        let sent = transport.sent_records();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["checks"][0]["check_kind"], "lint");
        assert_eq!(sent[0]["checks"].as_array().unwrap().len(), 6);
    }
}

#[test]
fn reserved_fmt_stage_rejects_writes_extra_arguments_and_other_commands() {
    for argv in [
        vec!["cargo", "fmt"],
        vec!["cargo", "fmt", "--check", "--all"],
        vec!["cargo", "fmt", "--", "--check"],
        vec!["cargo", "+nightly", "fmt", "--check"],
        vec!["rustfmt", "--check", "src/lib.rs"],
        vec!["node", "scripts/lint.mjs"],
    ] {
        let fixture = RunFixture::new("verification-fmt-unsupported");
        let mut request = six_stage_request(&fixture.root, &argv);
        // A request's label cannot turn unsupported formatting into a Test.
        request["stages"][0]["kind"] = json!("test");
        for (index, entry) in request["stages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
            .skip(1)
        {
            entry["kind"] = json!(if index < 3 { "build" } else { "test" });
        }
        let run = record_run(
            &fixture,
            "test-fmt-unsupported",
            request,
            six_stage_results("passed", &argv),
        );
        assert_eq!(
            pending_of(&run.registration),
            EngramVerificationRecordPending::UnclassifiedStages {
                stages: vec!["fmt".to_owned()],
            }
        );
        assert!(
            !run.run_dir
                .join(ENGRAM_VERIFICATION_RECORD_BODY_FILE)
                .exists()
        );
    }
    assert!(
        EngramHost::recognized_check("cargo fmt --check").is_none(),
        "the registered stage does not widen ordinary command recognition"
    );
}
