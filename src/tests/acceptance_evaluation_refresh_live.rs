//! Producer control for distinct whole judgments by one independent session.
//! Uses the pinned binary and a disposable store; no operator data is read.
use super::*;

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn one_independent_evaluator_records_two_whole_attempts() {
    let fixture = completion_fixture();
    let holder = &fixture.live.session_id;
    fixture.command(
        holder,
        &[
            "control-policy",
            "set-acceptance-evaluation",
            "--modes",
            "independent-session",
            "--mechanical-basis",
            "observed",
            "--authorized-by",
            "disposable-evaluator-reuse-test",
            "--idempotency-key",
            "independent-reuse-policy",
        ],
        false,
    );
    let added = fixture.work(
        holder,
        &[
            "add",
            "Judge this disposable run twice",
            "--accept",
            "The fixture note was inspected",
            "--json",
        ],
    );
    let work = added["work"]["short_ref"]
        .as_str()
        .or_else(|| added["short_ref"].as_str())
        .unwrap();
    fixture.work(holder, &["claim", work, "--json"]);
    let evaluator = {
        let mut inner = fixture.live.state.inner.lock().unwrap();
        inner
            .create_session(
                Agent::Codex,
                Some("Independent test evaluator".to_owned()),
                fixture.root.to_string_lossy().into_owned(),
                Some(fixture.project_id.clone()),
                None,
            )
            .session
            .id
            .clone()
    };
    let mut hashes = Vec::new();
    let mut cuts = Vec::new();
    for ordinal in 1..=2 {
        fixture.work(
            holder,
            &[
                "note",
                work,
                &format!("Fixture evidence for judgment {ordinal}"),
                "--json",
            ],
        );
        let shown = fixture.work(&evaluator, &["show", work, "--notes", "--json"]);
        let mut notes = Vec::new();
        objects_with_key(&shown, "locator", &mut notes);
        let locator = notes
            .iter()
            .find_map(|n| n["locator"].as_str())
            .expect("a store-minted evidence locator");
        let shown = fixture.work(&evaluator, &["show", work, "--json"]);
        let acceptance = shown["acceptance_basis"].as_i64().unwrap().to_string();
        let cut = shown["evidence_basis"].as_i64().unwrap();
        cuts.push(cut);
        let key = format!("independent-test-evaluator:attempt:{ordinal}");
        let receipt = fixture.work(&evaluator, &[
            "evaluate", work, "--mode", "independent-session", "--acceptance-basis", &acceptance,
            "--evidence-basis", &cut.to_string(), "--verdict", "1=pass:judgment",
            "--rationale", &format!("1=Attempt {ordinal}; evaluator session {evaluator}. Inspected all fixture evidence."),
            "--evidence", &format!("1={locator}"), "--attempt", &key, "--json",
        ]);
        let evaluation = &receipt["evaluation"];
        assert_eq!(evaluation["passed"], 1, "{receipt:#}");
        assert_eq!(evaluation["verdicts_total"], 1, "{receipt:#}");
        let hash = evaluation["hash"]
            .as_str()
            .expect("producer evaluation hash")
            .to_owned();
        hashes.push(hash);
    }
    assert!(
        cuts[1] > cuts[0],
        "the second judgment reads newly recorded evidence"
    );
    assert_ne!(
        hashes[0], hashes[1],
        "a fresh key records a second whole judgment"
    );
    let shown = fixture.work(&evaluator, &["show", work, "--evaluations", "--json"]);
    let serialized = shown.to_string();
    for hash in hashes {
        assert!(
            serialized.contains(&hash),
            "both whole records survive: {shown:#}"
        );
    }
}
