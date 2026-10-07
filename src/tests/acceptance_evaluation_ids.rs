// Evaluation-ID rollout controls, separated from acceptance_evaluation.rs.
// Owns live producer field precedence, carried identities and receipt naming;
// not tracker execution or alias removal. Reuses the parent's pure JSON helpers.
use super::*;

const PREFERRED: &str = "11111111111111111111111111111111";
const LEGACY: &str = "22222222222222222222222222222222";
const CARRIED: &str = "33333333333333333333333333333333";

fn id_shapes() -> Vec<(Value, Option<&'static str>)> {
    vec![
        (json!({"evaluation": PREFERRED}), Some(PREFERRED)),
        (
            json!({"evaluation": PREFERRED, "hash": PREFERRED}),
            Some(PREFERRED),
        ),
        (
            json!({"evaluation": PREFERRED, "hash": LEGACY}),
            Some(PREFERRED),
        ),
        (json!({"hash": LEGACY}), Some(LEGACY)),
        (json!({}), None),
    ]
}

fn invalid_ids() -> Vec<Value> {
    vec![
        Value::Null,
        json!(false),
        json!(7),
        json!([]),
        json!({}),
        json!(""),
    ]
}

// The installed additive producer's full-show projection has work.evaluation
// containing evaluation/hash plus verdicts and work_revision. Only the fixture
// identities/criteria are synthetic; it is not moved to a terse-show projection.
fn id_full_receipt(ids: Value) -> Value {
    let mut evaluation = json!({
        "evaluated_cut": 124, "mode": "independent_session", "passed": 0,
        "stale": null, "work_revision": 7,
        "verdicts": [{"position": 1, "criterion": "The route exists",
            "verdict": "fail", "rationale": "The newer failure is retained"}],
        "carried_failure": {
            "evaluation": CARRIED, "revised_by": "executor", "judged_revision": 2,
            "supersedes_required": true, "judged_criteria": ["The old route exists"],
            "blocking": [{"criterion": 1, "verdict": "fail", "rationale": "Retained"}]
        }
    });
    evaluation
        .as_object_mut()
        .unwrap()
        .extend(ids.as_object().unwrap().clone());
    let mut full = full_receipt();
    full["work"]["evaluation"] = evaluation;
    full
}

fn receipt_wire_id(receipt: &Value) -> Option<String> {
    let extract = acceptance_evaluation_receipt_extract(receipt);
    let wire = serde_json::to_value(&extract).unwrap();
    assert!(wire.get("evaluationHash").is_none(), "{wire}");
    wire.get("evaluationId")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

#[test]
fn acceptance_id_full_show_prefers_evaluation_and_preserves_carried_identity() {
    for (ids, expected) in id_shapes() {
        let task =
            parse_acceptance_evaluation_task(show_receipt(None), id_full_receipt(ids)).unwrap();
        let carried = task
            .carried_failure
            .as_ref()
            .expect("the carried ID is independent");
        assert_eq!(carried.evaluation, CARRIED);
        assert_eq!(
            carried
                .newest
                .as_ref()
                .map(|newest| newest.evaluation.as_str()),
            expected,
        );
        let seed = task.target_seed(
            AcceptanceEvaluationMode::IndependentSession,
            EngramAuthorityStoreKey {
                database_path: PathBuf::from("/repo/engram.db"),
                project_id: "fixture".to_owned(),
            },
            None,
            None,
            None,
        );
        assert_eq!(seed.supersedes.as_deref(), Some(CARRIED));
    }
}

#[test]
fn acceptance_id_present_invalid_full_field_is_not_rescued_by_hash() {
    let mut invalid = invalid_ids();
    invalid.extend([
        json!("not-a-record"),
        json!("A".repeat(32)),
        json!("1".repeat(31)),
    ]);
    for value in invalid {
        let full = id_full_receipt(json!({"evaluation": value, "hash": LEGACY}));
        match parse_acceptance_evaluation_task(show_receipt(None), full) {
            Ok(task) => assert!(
                task.carried_failure.unwrap().newest.is_none(),
                "a present invalid preferred ID must not expose the legacy newest failure"
            ),
            Err(error) => assert_eq!(error.status, StatusCode::BAD_GATEWAY),
        }
    }
}

#[test]
fn acceptance_id_valid_preferred_field_ignores_malformed_legacy() {
    for legacy in invalid_ids() {
        let ids = json!({"evaluation": PREFERRED, "hash": legacy});
        let task =
            parse_acceptance_evaluation_task(show_receipt(None), id_full_receipt(ids.clone()))
                .unwrap();
        assert_eq!(
            task.carried_failure.unwrap().newest.unwrap().evaluation,
            PREFERRED
        );
        assert_eq!(
            receipt_wire_id(&json!({"evaluation": ids})).as_deref(),
            Some(PREFERRED)
        );
    }
    let task = parse_acceptance_evaluation_task(
        show_receipt(None),
        id_full_receipt(json!({"evaluation": CARRIED, "hash": LEGACY})),
    )
    .unwrap();
    assert!(task.carried_failure.unwrap().newest.is_none());
}

#[test]
fn acceptance_id_full_show_keeps_32_and_64_character_record_ids() {
    for length in [32, 64] {
        let id = "a".repeat(length);
        let full = id_full_receipt(json!({"evaluation": id, "hash": LEGACY}));
        let task = parse_acceptance_evaluation_task(show_receipt(None), full).unwrap();
        assert_eq!(task.carried_failure.unwrap().newest.unwrap().evaluation, id);
    }
}

#[test]
fn acceptance_id_normal_minimal_and_replayed_receipts_use_presence_precedence() {
    for shape in ["normal", "minimal", "replayed"] {
        for (ids, expected) in id_shapes() {
            let mut evaluation = ids;
            evaluation["mode"] = json!("independent_session");
            if shape != "minimal" {
                evaluation["passed"] = json!(1);
                evaluation["verdicts_total"] = json!(2);
                evaluation["work_revision"] = json!(7);
                evaluation["evaluated_cut"] = json!(124);
            }
            evaluation["replayed"] = json!(shape == "replayed");
            let receipt = json!({"evaluation": evaluation});
            assert_eq!(receipt_wire_id(&receipt).as_deref(), expected, "{shape}");
            let extract = acceptance_evaluation_receipt_extract(&receipt);
            assert_eq!(extract.mode.as_deref(), Some("independent_session"));
            assert_eq!(extract.replayed, shape == "replayed");
            assert_eq!(extract.passed, (shape != "minimal").then_some(1));
            assert_eq!(extract.verdicts_total, (shape != "minimal").then_some(2));
        }
    }
    assert_eq!(receipt_wire_id(&json!({})), None);
    assert_eq!(receipt_wire_id(&json!({"evaluation": null})), None);
}

#[test]
fn acceptance_id_present_invalid_receipt_field_does_not_fall_back() {
    for value in invalid_ids()
        .into_iter()
        .filter(|value| value.as_str() != Some(""))
    {
        let receipt = json!({"evaluation": {"evaluation": value, "hash": LEGACY}});
        assert_eq!(receipt_wire_id(&receipt), None);
    }
    // The receipt extract's existing bounded-string behavior is not a new
    // record-ID validator: a present empty string stays empty, not legacy.
    assert_eq!(
        receipt_wire_id(&json!({"evaluation": {"evaluation": "", "hash": LEGACY}})),
        Some(String::new()),
    );
}

#[test]
fn acceptance_id_receipt_naming_roundtrips_existing_host_snapshot_without_live_rescue() {
    // Persisted host extracts previously used evaluationHash. This is snapshot
    // decoding, distinct from a live producer's evaluation/hash selection.
    let old = json!({"evaluationHash": LEGACY, "passed": 1, "verdictsTotal": 2});
    let loaded: AcceptanceEvaluationReceiptExtract = serde_json::from_value(old).unwrap();
    let wire = serde_json::to_value(&loaded).unwrap();
    assert_eq!(wire["evaluationId"], LEGACY);
    assert!(wire.get("evaluationHash").is_none());
    assert_eq!(
        serde_json::from_value::<AcceptanceEvaluationReceiptExtract>(wire).unwrap(),
        loaded,
    );
    let current: AcceptanceEvaluationReceiptExtract =
        serde_json::from_value(json!({"evaluationId": PREFERRED, "passed": 1, "verdictsTotal": 2}))
            .unwrap();
    assert_eq!(
        serde_json::to_value(current).unwrap()["evaluationId"],
        PREFERRED
    );
    assert!(
        serde_json::from_value::<AcceptanceEvaluationReceiptExtract>(json!({
            "evaluationId": PREFERRED, "evaluationHash": LEGACY
        }))
        .is_err(),
        "duplicate snapshot spellings are not live fallback"
    );
}
