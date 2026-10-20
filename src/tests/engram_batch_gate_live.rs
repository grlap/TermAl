// Owns the operator-run check of a batch full gate (src/engram_batch_binds.rs)
// against a real Engram binary and a disposable store: one session holds two
// real claims, A and B, named on the same worktree at the same tree; B has a
// checkpointed turn of its own there; a background full gate launched from
// A's turn is credited on A, the real store binds that passed record to B on
// B's own run, and B's acceptance evaluation cites the bound record.
// Does not own the scripted checks of the same flow
// (src/tests/engram_batch_gate_flow.rs) or the carried gate fixture and
// helpers of its parent module (src/tests/engram_carried_gate_live.rs).
// New module, a child of the carried gate live module so the launcher's
// `live` mode runs it.
use super::*;

/// Runs a real Engram command that is not a work word against the fixture's
/// disposable store only.
fn live_engram_admin(gate: &LiveCarriedGate, args: &[&str]) {
    let output = Command::new(live_engram_binary())
        .arg("--project-file")
        .arg(&gate.project_file)
        .arg("--home")
        .arg(&gate.home)
        .args(args)
        .output()
        .expect("real Engram should launch");
    assert!(
        output.status.success(),
        "engram {args:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Names `work`'s source root, for the fixture's session, as the worktree.
fn name_root_for(gate: &LiveCarriedGate, work: &str) -> EngramSourceRootResponse {
    gate.fixture
        .state
        .name_engram_source_root(
            &gate.fixture.session_id,
            EngramSourceRootRequest {
                work: work.to_owned(),
                path: Some(Some(gate.worktree.to_string_lossy().into_owned())),
            },
        )
        .unwrap_or_else(|error| panic!("real named_root_bind names {work}: {error:?}"))
}

/// The rows of a `show --notes` reply that carry a verification, with their
/// locators.
fn verification_rows(shown: &Value) -> Vec<Value> {
    let mut found = Vec::new();
    let mut stack = vec![shown.clone()];
    while let Some(value) = stack.pop() {
        match value {
            Value::Object(map) => {
                if map.get("verification").is_some_and(Value::is_object) {
                    found.push(Value::Object(map.clone()));
                }
                stack.extend(map.into_iter().map(|(_, value)| value));
            }
            Value::Array(values) => stack.extend(values),
            _ => {}
        }
    }
    found
}

fn short_ref_of(added: &Value) -> String {
    added["work"]["short_ref"]
        .as_str()
        .or_else(|| added["short_ref"].as_str())
        .unwrap_or_else(|| panic!("the item has a short ref: {added:#}"))
        .to_owned()
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_batch_gate_credited_on_one_claim_is_bound_to_the_other_on_its_own_run() {
    // Claim A is the fixture's own item; claim B is taken after it, so B is
    // the session's focus when the session first binds.
    let gate = live_carried_gate("carried-batch");
    let work_a = gate.work_ref.clone();
    live_engram_admin(
        &gate,
        &[
            "control-policy",
            "set-acceptance-evaluation",
            "--modes",
            "same-session",
            "--mechanical-basis",
            "observed",
            "--require-source-freshness",
            "--authorized-by",
            "disposable-batch-gate-test",
            "--idempotency-key",
            "batch-gate-policy",
        ],
    );
    let added = live_engram_work(
        &gate,
        &[
            "add",
            "Batch member live check",
            "--accept",
            "The full gate passes on this item's run",
            "--bind",
            "1=test",
            "--json",
        ],
    );
    let work_b = short_ref_of(&added);
    live_engram_work(&gate, &["claim", &work_b, "--json"]);
    gate.fixture
        .state
        .ensure_engram_session_bound_off_lock(&gate.fixture.session_id)
        .expect("real claims bind")
        .expect("control is enabled");
    let bound_b = live_record(&gate, |record| record.engram.work_binding.clone().unwrap());

    // Both items name the same worktree, at one tree.
    let named_b = name_root_for(&gate, &work_b);
    let named_a = name_root_for(&gate, &work_a);
    let revision = named_b
        .source_revision
        .clone()
        .expect("the named worktree is measured");
    assert_eq!(named_a.source_revision.as_ref(), Some(&revision));

    // B's own turn, on the frozen tree, sights the root for B.
    begin_live_turn(&gate, "Look at the batch worktree for the member.");
    assert_eq!(
        live_record(&gate, |record| record.engram.work_binding.clone().unwrap()),
        bound_b,
        "the member's turn runs on the member's claim"
    );
    finish_live_root(&gate.fixture.state, &gate.fixture.session_id);

    // Focus returns to A through a real claim; the next admission rebinds
    // to it, and A's turn launches the gate in the background.
    live_engram_work(&gate, &["claim", &work_a, "--json"]);
    begin_live_turn(&gate, "Launch the batch gate.");
    let bound_a = live_record(&gate, |record| record.engram.work_binding.clone().unwrap());
    assert_ne!(bound_a.work_id, bound_b.work_id, "the launch runs on A's claim");
    launch_live_gate(&gate);
    let members = live_record(&gate, |record| {
        record.engram.carried_checks[0].bind_targets.clone()
    });
    assert_eq!(members.len(), 1, "{members:?}");
    assert_eq!(members[0].short_ref, work_b);
    assert_eq!(members[0].claim_fence, bound_b.claim_fence);
    finish_live_root(&gate.fixture.state, &gate.fixture.session_id);

    // The gate passes; A's next checkpoint credits it, and the answer naming
    // its record makes B's bind owed.
    finish_live_run(&gate);
    begin_live_turn(&gate, "Read the batch gate's summary.");
    finish_live_root(&gate.fixture.state, &gate.fixture.session_id);
    let checkpoints = gate.fixture.transport.requests_for("turn_checkpoint");
    let settling = checkpoints.last().expect("the settling turn is checkpointed");
    assert_eq!(
        settling["verification_evidence"].as_array().map(Vec::len),
        Some(1),
        "{settling:#}"
    );
    let owed = live_record(&gate, |record| record.engram.owed_binds.clone());
    assert_eq!(owed.len(), 1, "{owed:?}");
    assert_eq!(owed[0].target.short_ref, work_b);
    assert_eq!(owed[0].measurement.source_revision, revision);

    // The first tick decides B from its real claim and sighting and confirms
    // that decision stored; the next sends it, and the real store binds it.
    gate.fixture
        .state
        .engram_owed_binds_tick(chrono::Utc::now(), DEADLOCK_GUARD);
    assert!(
        gate.fixture
            .transport
            .requests_for("verification_bind")
            .is_empty(),
        "nothing is sent before the decision is stored"
    );
    let decided = live_record(&gate, |record| record.engram.owed_binds.clone());
    assert_eq!(decided.len(), 1, "{decided:?}");
    assert!(decided[0].decided.is_some(), "{decided:?}");
    gate.fixture
        .state
        .engram_owed_binds_tick(chrono::Utc::now(), DEADLOCK_GUARD);
    let binds = gate.fixture.transport.requests_for("verification_bind");
    assert_eq!(binds.len(), 1, "{binds:#?}");
    assert!(
        live_record(&gate, |record| record.engram.owed_binds.is_empty()),
        "the real store's answer settles the member"
    );
    let told = live_record(&gate, |record| {
        record.engram.pending_source_root_line.clone()
    })
    .unwrap_or_default();
    assert!(
        told.contains(&format!(
            "the full gate is also recorded for {work_b} (bound), on its own run."
        )),
        "{told}"
    );

    // The real store holds the passed record on B's own run.
    let shown_b = live_engram_work(&gate, &["show", &work_b, "--notes", "--json"]);
    let rows = verification_rows(&shown_b);
    eprintln!("live batch gate: the member's verifications: {rows:#?}");
    assert_eq!(rows.len(), 1, "one bound record on the member: {shown_b:#}");
    assert_eq!(rows[0]["verification"]["check_kind"], "test");
    assert_eq!(rows[0]["verification"]["result"], "passed");
    assert_eq!(rows[0]["verification"]["source_revision"], revision);
    let bound_record = rows[0]["locator"]
        .as_str()
        .expect("the bound record has a locator")
        .to_owned();
    let shown_a = live_engram_work(&gate, &["show", &work_a, "--notes", "--json"]);
    let original = verification_rows(&shown_a);
    assert_eq!(original.len(), 1, "the gate's own record on A: {shown_a:#}");
    assert_ne!(
        original[0]["locator"], rows[0]["locator"],
        "B's record is its own, bound from A's"
    );

    // B's evaluation cites the bound record, which the real store finds on
    // B's own run.
    let shown = live_engram_work(&gate, &["show", &work_b, "--json"]);
    let acceptance = shown["acceptance_basis"]
        .as_i64()
        .expect("acceptance basis")
        .to_string();
    let evidence = shown["evidence_basis"]
        .as_i64()
        .expect("evidence cut")
        .to_string();
    let evaluated = live_engram_work(
        &gate,
        &[
            "evaluate",
            &work_b,
            "--mode",
            "same-session",
            "--acceptance-basis",
            &acceptance,
            "--evidence-basis",
            &evidence,
            "--verdict",
            "1=pass:observed",
            "--rationale",
            "1=The batch full gate's record, bound to this item's run, passed",
            "--evidence",
            &format!("1={bound_record}"),
            "--source-fingerprint",
            &revision,
            "--attempt",
            "batch-member",
            "--json",
        ],
    );
    eprintln!("live batch gate: the member's evaluation: {evaluated:#}");
    // Engram's reply counts citations rather than naming them. The one cited
    // record is the bound one, and Engram admits a citation only from the
    // evaluated item's own run, so a passed verdict citing it on B's run is
    // B's evaluation citing the bound record there.
    let evaluation = &evaluated["evaluation"];
    assert_eq!(evaluation["run_id"], bound_b.run_id, "{evaluated:#}");
    assert_eq!(evaluation["source_fingerprint"], revision, "{evaluated:#}");
    assert_eq!(evaluation["passed"], 1, "{evaluated:#}");
    assert_eq!(evaluation["verdicts"][0]["verdict"], "pass", "{evaluated:#}");
    assert_eq!(evaluation["verdicts"][0]["citations"], 1, "{evaluated:#}");
}
