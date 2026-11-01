//! Per-run result totals and fingerprint diagnostics in native resumes.
//! Uses disposable real index/wait fixtures, without launcher or live stores.
use super::*;

#[test]
fn resume_evidence_regression_counts_all_states_and_stages_beyond_the_summary_cap() {
    let fixture = WaitFixture::new("prompt-totals");
    let mut results = passed();
    let mut stages = vec![
        json!({"name":"pass", "state":"passed"}),
        json!({"name":"fail", "state":"failed"}),
        json!({"name":"interrupted", "state":"failed", "interrupted":true}),
        json!({"name":"running", "state":"running"}),
        json!({"name":"unrun", "state":"unrun"}),
        json!({"name":"unknown", "state":"future"}),
        json!({}),
    ];
    stages.extend((0..64).map(|index| json!({"name":format!("extra-{index}"), "state":"passed"})));
    results["stages"] = json!(stages);
    results["preflight"] = json!([
        {"name":"pass", "code":0}, {"name":"fail", "code":1},
        {"name":"interrupt", "code":1, "interrupted":true},
        {"name":"running", "state":"running"}, {"name":"unrun", "state":"unrun"},
        {}, {"name":"future", "state":"future", "code":0}
    ]);
    results["before"] = json!("a".repeat(64));
    results["after"] = results["before"].clone();
    fixture.write("test-totals", Some(results));
    fixture.scan(&[]);
    fixture.busy();
    fixture.register(&["test-totals"], "all").unwrap();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("Stage totals: passed 65, failed 1, interrupted 1, running 1, unrun 1, unknown 2"), "{}", prompts[0]);
    assert!(prompts[0].contains("Preflight totals: passed 1, failed 1, interrupted 1, running 1, unrun 1, unknown 2"));
    assert!(prompts[0].contains("Before/after fingerprint: matched"));
    assert!(prompts[0].contains("- `test-totals`: PASS"), "diagnostics do not redefine the verdict");
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.queued_prompts(), prompts);
}

#[test]
fn resume_evidence_regression_compares_before_and_after_conservatively() {
    for (before, after, expected) in [
        (json!("a".repeat(64)), json!("a".repeat(64)), "matched"),
        (json!("a".repeat(64)), json!("b".repeat(64)), "mismatched"),
        (Value::Null, json!("b".repeat(64)), "unknown"),
        (json!("a".repeat(64)), Value::Null, "unknown"),
        (json!(""), json!(""), "unknown"),
    ] {
        let fixture = WaitFixture::new("prompt-fingerprints");
        let mut results = passed();
        results["before"] = before;
        results["after"] = after;
        results["preflight"] = json!([]);
        fixture.write("test-fingerprint", Some(results));
        fixture.scan(&[]);
        fixture.busy();
        fixture.register(&["test-fingerprint"], "all").unwrap();
        let prompt = fixture.queued_prompts().pop().unwrap();
        assert!(prompt.contains(&format!("Before/after fingerprint: {expected}")), "{prompt}");
        assert!(prompt.contains("Preflight totals: passed 0, failed 0, interrupted 0, running 0, unrun 0, unknown 0"));
        assert!(prompt.contains("- `test-fingerprint`: PASS"));
    }
}

#[test]
fn resume_evidence_regression_unavailable_results_do_not_invent_zero_totals() {
    for unavailable in ["missing", "malformed", "oversized"] {
        let fixture = WaitFixture::new(&format!("prompt-{unavailable}"));
        let directory = fixture.write("test-unavailable", None);
        if unavailable != "missing" {
            fs::write(directory.join("results.json"), if unavailable == "malformed" {
                b"{malformed".to_vec()
            } else { vec![b' '; TEST_RUN_JSON_MAX_BYTES + 1] }).unwrap();
        }
        fixture.write("test-done", Some(passed()));
        fixture.scan(&[]);
        fixture.busy();
        fixture.register(&["test-unavailable", "test-done"], "any").unwrap();
        let prompt = fixture.queued_prompts().pop().unwrap();
        let section = prompt.split("### Run `test-unavailable`").nth(1).unwrap()
            .split("### Run `test-done`").next().unwrap();
        assert!(section.contains("Stage totals: unknown (results unavailable)"), "{prompt}");
        assert!(section.contains("Preflight totals: unknown (results unavailable)"));
        assert!(section.contains("Before/after fingerprint: unknown"));
        assert!(!section.contains("passed 0"));
        assert!(prompt.contains("- `test-done`: PASS"));
    }
}

#[test]
fn resume_evidence_regression_keeps_all_run_totals_with_bounded_failure_excerpts() {
    let fixture = WaitFixture::new("prompt-evidence-budget");
    let ids: Vec<String> = (0..16).map(|index| format!("test-budget-{index}")).collect();
    for id in &ids {
        let mut results = failed(&"x".repeat(TEST_RUN_WAIT_EXCERPT_MAX_BYTES));
        results["preflight"] = json!([]);
        results["before"] = json!("a".repeat(64));
        results["after"] = json!("b".repeat(64));
        fixture.write(id, Some(results));
    }
    fixture.scan(&[]);
    fixture.busy();
    fixture.register(&ids.iter().map(String::as_str).collect::<Vec<_>>(), "all").unwrap();
    let prompt = fixture.queued_prompts().pop().unwrap();
    assert!(prompt.len() <= TEST_RUN_WAIT_PROMPT_MAX_BYTES);
    assert!(!prompt.contains(DELEGATION_WAIT_RESUME_TRUNCATED_MARKER));
    assert_eq!(prompt.matches("Stage totals: passed 0, failed 1").count(), 16);
    assert_eq!(prompt.matches("Preflight totals: passed 0, failed 0").count(), 16);
    assert_eq!(prompt.matches("Before/after fingerprint: mismatched").count(), 16);
    for id in &ids {
        assert!(prompt.contains(&format!("### Run `{id}` (full)")));
    }
}

#[test]
fn resume_evidence_regression_malformed_explicit_preflight_states_are_unknown() {
    let fixture = WaitFixture::new("prompt-malformed-state");
    let mut results = passed();
    results["preflight"] = json!([
        {"state":false,"code":0}, {"state":null,"code":0}, {"state":{},"code":1},
        {"state":[],"code":1}, {"state":7,"code":0},
        {"code":0}, {"code":1},
        {"state":false,"code":0,"interrupted":true}
    ]);
    fixture.write("test-malformed", Some(results));
    fixture.scan(&[]);
    fixture.busy();
    fixture.register(&["test-malformed"], "all").unwrap();
    let prompt = fixture.queued_prompts().pop().unwrap();
    assert!(prompt.contains("Preflight totals: passed 1, failed 1, interrupted 1, running 0, unrun 0, unknown 5"), "{prompt}");
    assert!(prompt.contains("- `test-malformed`: PASS"));
}

#[test]
fn resume_evidence_regression_readable_missing_or_non_array_collections_are_unknown() {
    for collection in [None, Some(Value::Null), Some(json!({})), Some(json!(false))] {
        let fixture = WaitFixture::new("prompt-missing-collection");
        let mut results = passed();
        results.as_object_mut().unwrap().remove("stages");
        if let Some(collection) = collection {
            results["stages"] = collection.clone();
            results["preflight"] = collection;
        }
        fixture.write("test-collection", Some(results));
        fixture.scan(&[]);
        fixture.busy();
        fixture.register(&["test-collection"], "all").unwrap();
        let prompt = fixture.queued_prompts().pop().unwrap();
        assert!(prompt.contains("Stage totals: unknown"), "{prompt}");
        assert!(prompt.contains("Preflight totals: unknown"));
        assert!(!prompt.contains("passed 0"));
    }
}

#[test]
fn resume_evidence_regression_admission_reserves_totals_at_the_mandatory_limit() {
    // Long display paths exercise admission without creating long native
    // paths. The real index entries retain their original request identities.
    // 600-byte paths leave room; 740-byte paths fit only if the new evidence
    // reservation is omitted. This distinction is well clear of UUID lengths.
    for path_bytes in [600, 740] {
        let fixture = WaitFixture::new(&format!("prompt-admission-{path_bytes}"));
        let ids: Vec<String> = (0..16).map(|index| format!("test-bound-{index}")).collect();
        for id in &ids {
            fixture.write(id, Some(passed()));
        }
        fixture.scan(&[]);
        fixture.busy();
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let prefix = test_run_display_path(&fixture.root);
            for position in 0..inner.test_runs.entries.len() {
                inner.test_runs.entries.update(position, |entry| {
                    entry.summary.run_dir = format!("{prefix}/{}", "x".repeat(path_bytes - prefix.len() - 1));
                    assert_eq!(entry.summary.run_dir.len(), path_bytes);
                });
            }
        }
        let response = fixture.register(&ids.iter().map(String::as_str).collect::<Vec<_>>(), "all");
        if path_bytes == 600 {
            assert!(response.is_ok(), "the shorter control fits");
        } else {
            let error = response.err().expect("mandatory evidence must count at admission");
            assert_eq!(error.status, StatusCode::BAD_REQUEST);
            assert!(error.message.contains("49152-byte limit"), "{}", error.message);
            assert!(fixture.pending_waits().is_empty());
            assert!(fixture.queued_prompts().is_empty());
        }
    }
}
