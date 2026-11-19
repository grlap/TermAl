//! Engram's registered FULL stages preserve lint evidence and skipped work.
use super::*;

const CLIPPY: &[&str] = &[
    "cargo",
    "clippy",
    "--all-targets",
    "--all-features",
    "--",
    "-D",
    "warnings",
];
const DOCS: &[&str] = &["/usr/bin/node", "scripts/check-doc-links.mjs"];

fn engram_full_run(
    fixture: &RunFixture,
    clippy: &[&str],
    docs: &[&str],
    failed_stage: Option<&str>,
    adjust: impl FnOnce(&mut Value, &mut Value),
) -> RecordRun {
    let stages: [(&str, &str, &[&str]); 6] = [
        ("fmt", "lint", &["cargo", "fmt", "--check"]),
        ("check", "build", &["cargo", "check"]),
        ("clippy", "lint", clippy),
        (
            "rust",
            "test",
            &["sh", "scripts/test-rust.sh", "--phase", "ordinary"],
        ),
        (
            "freeze",
            "test",
            &["/usr/bin/node", "--test", "scripts/test-launcher.test.mjs"],
        ),
        ("docs", "other", docs),
    ];
    let mut request = commit_request(&fixture.root);
    request["stages"] = json!(stages.map(|(name, kind, argv)| {
        json!({"name":name, "kind":kind, "command":argv[0], "args":&argv[1..]})
    }));
    let mut results = commit_results();
    let mut ran = Vec::new();
    let mut stopped = false;
    for (index, (name, _, argv)) in stages.iter().enumerate() {
        if stopped {
            ran.push(json!({"name":name, "state":"unrun"}));
            continue;
        }
        stopped = failed_stage == Some(*name);
        ran.push(stage(
            name,
            if stopped { "failed" } else { "passed" },
            i64::from(stopped),
            argv,
            index as u32,
        ));
    }
    results["stages"] = json!(ran);
    results["ended"] = json!("2026-10-30T10:06:00.000Z");
    results["state"] = json!(if stopped { "failed" } else { "passed" });
    results["exitCode"] = json!(i64::from(stopped));
    results["input"]["eligible"] = json!(!stopped);
    adjust(&mut request, &mut results);
    let stage_names = request["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|stage| stage["name"].as_str().unwrap().to_owned())
        .collect();
    let mut run = record_run(fixture, "test-engram-full", request, results);
    run.registration.stage_names = stage_names;
    run
}

#[test]
fn engram_full_preserves_completed_checks_with_clippy_and_unrun_docs() {
    for (failed_stage, completed, docs_outcome) in [
        (None, 6, "passed"),
        (Some("freeze"), 5, "not_run"),
        (Some("clippy"), 3, "not_run"),
    ] {
        let fixture = RunFixture::new("verification-engram-full");
        let run = engram_full_run(&fixture, CLIPPY, DOCS, failed_stage, |_, _| {});
        let frozen = ready(&run.registration);
        let checks = frozen.record["checks"].as_array().unwrap();
        assert_eq!(checks.len(), 6);
        assert_eq!(checks[0]["check_kind"], "lint");
        assert_eq!(checks[1]["check_kind"], "build");
        assert_eq!(checks[1]["outcome"], "passed");
        assert_eq!(checks[2]["check_kind"], "lint");
        assert_eq!(
            checks[2]["outcome"],
            if failed_stage == Some("clippy") {
                "failed"
            } else {
                "passed"
            }
        );
        assert_eq!(checks[3]["check_kind"], "test");
        assert_eq!(
            checks[3]["outcome"],
            if failed_stage == Some("clippy") {
                "not_run"
            } else {
                "passed"
            }
        );
        assert_eq!(checks[4]["check_kind"], "test");
        assert_eq!(
            checks[4]["outcome"],
            match failed_stage {
                Some("clippy") => "not_run",
                Some("freeze") => "failed",
                _ => "passed",
            }
        );
        assert_eq!(checks[5]["check_kind"], "lint");
        assert_eq!(checks[5]["outcome"], docs_outcome);
        if docs_outcome == "not_run" {
            assert!(checks[5].get("started_at").is_none());
            assert!(checks[5].get("completed_at").is_none());
        }
        let expected_tests = if completed >= 5 {
            vec!["rust", "freeze"]
        } else {
            vec![]
        };
        let producers =
            engram_verification_record_producers(&run.registration.run_id, &frozen.body);
        assert_eq!(
            producers
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            expected_tests,
        );

        let transport = Arc::new(RecordingTransport::default());
        let adapter = adapter(&transport);
        let own = connection(SESSION, "actor");
        read_work_session(&transport, &adapter, &own, supported());
        let verifications = ["fmt", "check", "clippy", "rust", "freeze", "docs"]
            .into_iter()
            .take(completed)
            .enumerate()
            .map(|(index, name)| json!({"stage":name, "record_id":format!("{:032x}", index + 1)}))
            .collect::<Vec<_>>();
        transport.script(Ok(json!({
            "operation":PRODUCER, "producer":PRODUCER, "verifications":verifications,
            "run_outcome":if failed_stage.is_some() { "failed" } else { "passed" },
        })));
        let step = engram_verification_record_send(frozen, link(&adapter, &own));
        assert!(
            matches!(step, EngramVerificationRecordStep::Received { .. }),
            "{step:?}"
        );
        assert_eq!(transport.sent_records().len(), 1);
    }
}

#[test]
fn engram_full_names_unsupported_commands_under_lint_stage_labels() {
    let fixture = RunFixture::new("verification-engram-full-unclassified");
    let run = engram_full_run(
        &fixture,
        &["cargo", "clippy", "--fix"],
        &["node", "scripts/custom-docs.mjs"],
        None,
        |request, _| {
            request["stages"][2]["kind"] = json!("test");
            request["stages"][5]["kind"] = json!("test");
        },
    );
    assert_eq!(
        pending_of(&run.registration),
        EngramVerificationRecordPending::UnclassifiedStages {
            stages: vec!["clippy".to_owned(), "docs".to_owned()],
        }
    );
    assert!(
        !run.run_dir
            .join(ENGRAM_VERIFICATION_RECORD_BODY_FILE)
            .exists()
    );
}

#[test]
fn engram_full_lint_requires_usable_observed_argv() {
    for observed in [
        None,
        Some(json!([])),
        Some(json!(["cargo", 42])),
        Some(json!([""])),
        Some(json!(["unused-runner", "wrong-command"])),
    ] {
        let fixture = RunFixture::new("verification-engram-full-observed-argv");
        let run = engram_full_run(&fixture, CLIPPY, DOCS, None, |_, results| {
            for index in [2, 5] {
                let fields = results["stages"][index].as_object_mut().unwrap();
                if let Some(command) = &observed {
                    fields.insert("command".to_owned(), command.clone());
                } else {
                    fields.remove("command");
                }
            }
        });
        assert_eq!(
            pending_of(&run.registration),
            EngramVerificationRecordPending::UnclassifiedStages {
                stages: vec!["clippy".to_owned(), "docs".to_owned()],
            }
        );
        assert!(
            !run.run_dir
                .join(ENGRAM_VERIFICATION_RECORD_BODY_FILE)
                .exists()
        );
    }
}

#[test]
fn engram_full_lint_names_preserve_recognized_observed_test_and_build() {
    let fixture = RunFixture::new("verification-engram-full-observed-test-build");
    let run = engram_full_run(&fixture, CLIPPY, DOCS, None, |_, results| {
        results["stages"][2]["command"] = json!(["cargo", "test"]);
        results["stages"][5]["command"] = json!(["cargo", "check"]);
    });
    let frozen = ready(&run.registration);
    assert_eq!(frozen.record["checks"][2]["check_kind"], "test");
    assert_eq!(frozen.record["checks"][2]["command"], "cargo test");
    assert_eq!(frozen.record["checks"][5]["check_kind"], "build");
    assert_eq!(frozen.record["checks"][5]["command"], "cargo check");
}

#[test]
fn engram_full_lint_requires_the_stage_name_and_excludes_cargo_shims() {
    let fixture = RunFixture::new("verification-engram-full-lint-names");
    let run = engram_full_run(&fixture, CLIPPY, DOCS, None, |request, results| {
        for (index, name) in [(2, "linter"), (5, "custom-docs")] {
            request["stages"][index]["name"] = json!(name);
            results["stages"][index]["name"] = json!(name);
        }
    });
    assert_eq!(
        pending_of(&run.registration),
        EngramVerificationRecordPending::UnclassifiedStages {
            stages: vec!["linter".to_owned(), "custom-docs".to_owned()],
        }
    );

    let fixture = RunFixture::new("verification-engram-full-lint-shim");
    let run = engram_full_run(&fixture, CLIPPY, DOCS, None, |_, results| {
        results["stages"][2]["command"][0] = json!("cargo.cmd");
    });
    assert_eq!(
        pending_of(&run.registration),
        EngramVerificationRecordPending::UnclassifiedStages {
            stages: vec!["clippy".to_owned()],
        }
    );
}

#[test]
fn engram_full_lint_rechecks_an_acknowledged_legacy_body_without_rewriting_it() {
    let fixture = RunFixture::new("verification-engram-full-lint-retained");
    let mut run = engram_full_run(&fixture, CLIPPY, DOCS, None, |request, _| {
        request["stages"][2]["kind"] = json!("test");
        request["stages"][5]["kind"] = json!("test");
    });
    let read =
        |name| serde_json::from_slice::<Value>(&fs::read(run.run_dir.join(name)).unwrap()).unwrap();
    let (request, results) = (read("request.json"), read("results.json"));
    let original = engram_verification_record_build_with_stage_policy(
        &run.registration,
        &request,
        &results,
        None,
        false,
    )
    .unwrap();
    assert_eq!(
        original.checks[2].check_kind,
        EngramVerificationRecordCheckKind::Test
    );
    let text = engram_verification_record_text(&original).unwrap();
    let body_path = run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    fs::write(&body_path, &text).unwrap();
    assert_eq!(
        pending_of(&run.registration),
        EngramVerificationRecordPending::BodyMismatch,
        "an on-disk legacy body without the host's acknowledgment cannot be sent",
    );
    let verifications = ["fmt", "check", "clippy", "rust", "freeze", "docs"]
        .into_iter()
        .enumerate()
        .map(|(index, stage)| json!({"stage":stage,"record_id":format!("{:032x}", index + 1)}))
        .collect::<Vec<_>>();
    let receipt = serde_json::from_value(json!({
        "operation":PRODUCER, "producer":PRODUCER, "verifications":verifications,
        "run_outcome":"passed",
    }))
    .unwrap();
    run.registration.verification_record = Some(TestRunVerificationAck::Received {
        receipt,
        body_digest: engram_verification_record_body_digest(&original),
    });
    let EngramVerificationRecordPrepared::ReceiptHeld { frozen, .. } =
        engram_verification_record_prepare(&run.registration)
    else {
        panic!("the acknowledged legacy body must remain settleable");
    };
    assert_eq!(frozen.body, original);
    assert_eq!(fs::read_to_string(&body_path).unwrap(), text);
}
