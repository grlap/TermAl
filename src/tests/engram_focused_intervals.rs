// Owns focused launcher interval correlation through the Build recorder and
// the shared native Node completion boundary. Added beside, and mounted by,
// src/tests/engram_build_checks.rs; reuses its owned artifact fixtures.
// Does not own launcher execution, live verification, or source/grant fences.
use super::*;

const OBSERVED: &str = "2000-01-01T00:00:10.000Z";
const REQUEST: &str = "2000-01-01T00:00:09.850Z";
const STAGE_START: &str = "2000-01-01T00:00:09.900Z";
const STAGE_END: &str = "2000-01-01T00:00:10.100Z";
const RUN_END: &str = "2000-01-01T00:00:10.150Z";

struct ObservedStartScope {
    previous: Option<(String, Option<String>)>,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl ObservedStartScope {
    fn new() -> Self {
        Self {
            previous: TEST_ENGRAM_COMMAND_START_TIMING
                .with(|slot| slot.replace(Some((OBSERVED.to_owned(), None)))),
            _thread_bound: std::marker::PhantomData,
        }
    }
}

impl Drop for ObservedStartScope {
    fn drop(&mut self) {
        TEST_ENGRAM_COMMAND_START_TIMING.with(|slot| slot.replace(self.previous.take()));
    }
}

fn interval_records(f: &mut FocusedBuildFixture, request: &str) {
    assert_eq!(
        f.check.started_at, OBSERVED,
        "real recorder used the fixture clock"
    );
    f.request["started"] = json!(request);
    f.results["started"] = json!(request);
    f.results["ended"] = json!(RUN_END);
    f.results["stages"][0]["started"] = json!(STAGE_START);
    f.results["stages"][0]["ended"] = json!(STAGE_END);
}

#[test]
fn engram_focused_interval_late_observation_records_passed_build() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let _clock = ObservedStartScope::new();
    let mut f = FocusedBuildFixture::new("focused-interval-latency", 0);
    interval_records(&mut f, REQUEST);
    let revision = f.turn.revision();
    one_build(
        &f.finish(EngramCommandExit::Code(0)),
        &revision,
        "succeeded",
    );
}

#[test]
fn engram_focused_interval_exact_slack_boundary_records_passed_build() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let _clock = ObservedStartScope::new();
    let mut f = FocusedBuildFixture::new("focused-interval-boundary", 0);
    interval_records(&mut f, "2000-01-01T00:00:00.000Z");
    f.results["stages"][0]["started"] = json!("2000-01-01T00:00:00.050Z");
    let revision = f.turn.revision();
    one_build(
        &f.finish(EngramCommandExit::Code(0)),
        &revision,
        "succeeded",
    );
}

#[test]
fn engram_focused_interval_late_observation_preserves_native_build_failure() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let _clock = ObservedStartScope::new();
    let mut f = FocusedBuildFixture::new("focused-interval-native-failure", 101);
    interval_records(&mut f, REQUEST);
    let revision = f.turn.revision();
    one_build(&f.finish(EngramCommandExit::Code(101)), &revision, "failed");
}

#[test]
fn engram_focused_interval_node_completion_uses_the_same_latency_policy() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let _clock = ObservedStartScope::new();
    let mut f = FocusedBuildFixture::new("focused-interval-node-boundary", 0);
    interval_records(&mut f, REQUEST);
    // Reuse the matched owned artifacts, adapting only the Node command and
    // runner facts. This exercises the production Node completion boundary;
    // it does not claim the Build recorder started a Node check.
    let mut node_check = f.check.clone();
    node_check.command = engram_check_command(
        "node scripts/test-launcher.mjs focused -- node --test fixture.test.mjs",
    )
    .unwrap();
    f.request["stages"][0]["command"] = json!("node");
    f.request["stages"][0]["args"] = json!(["--test", "fixture.test.mjs"]);
    f.results["stages"][0]["command"] =
        json!([f.turn.root.join("node.exe"), "--test", "fixture.test.mjs"]);
    f.results["stages"][0]["tests"] =
        json!({"runner":"node-test", "passed":1, "failed":0, "ignored":0});
    f.save();
    let (exit, _, passed, diagnostic) = engram_focused_node_completion(
        &node_check,
        &f.turn.session_id,
        &f.output,
        EngramCommandExit::Code(0),
    );
    assert!(matches!(exit, EngramCommandExit::Code(0)), "{diagnostic:?}");
    assert!(passed);
    assert!(diagnostic.is_none());
}

#[test]
fn engram_focused_interval_negative_controls_name_the_failed_comparison() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let _clock = ObservedStartScope::new();
    let cases: &[(&str, fn(&mut FocusedBuildFixture))] = &[
        ("request.started < observed_start - 10s", |f| {
            interval_records(f, "1999-12-31T23:59:59.999Z");
            f.results["stages"][0]["started"] = json!("2000-01-01T00:00:00.050Z");
        }),
        ("early run.ended < observed_start", |f| {
            f.results["stages"][0]["ended"] = json!("2000-01-01T00:00:09.950Z");
            f.results["ended"] = json!("2000-01-01T00:00:09.999Z");
        }),
        ("request.started > stage.started", |f| {
            f.results["stages"][0]["started"] = json!("2000-01-01T00:00:09.849Z");
        }),
        ("stage.started > stage.ended", |f| {
            f.results["stages"][0]["ended"] = json!("2000-01-01T00:00:09.899Z");
            // Avoid triggering the early completed-run guard instead.
            f.results["ended"] = json!(RUN_END);
        }),
        ("stage.ended > run.ended", |f| {
            f.results["ended"] = json!("2000-01-01T00:00:10.099Z");
        }),
        ("run.ended > host now", |f| {
            f.results["ended"] = json!("2099-01-01T00:00:00.000Z");
        }),
        ("request.started != results.started", |f| {
            f.results["started"] = json!("2000-01-01T00:00:09.851Z");
        }),
        ("invalid request.started", |f| {
            f.request["started"] = json!("PRIVATE_INVALID_TIME")
        }),
        ("invalid stage.started", |f| {
            f.results["stages"][0]["started"] = Value::Null
        }),
        ("invalid stage.ended", |f| {
            f.results["stages"][0]["ended"] = Value::Null
        }),
        ("invalid run.ended", |f| f.results["ended"] = Value::Null),
    ];
    for (case, mutate) in cases {
        let mut f = FocusedBuildFixture::new("focused-interval-negative", 0);
        interval_records(&mut f, REQUEST);
        mutate(&mut f);
        let revision = f.turn.revision();
        let checkpoint = f.finish(EngramCommandExit::Code(0));
        one_build(&checkpoint, &revision, "unknown");
        let row = &checkpoint["verification_evidence"][0];
        let text = row["summary"].as_str().unwrap();
        assert!(text.contains("observed native exit 0"), "{case}: {text}");
        assert!(
            text.contains("interval validation rejected"),
            "{case}: {text}"
        );
        assert!(
            text.contains(case),
            "must name failed comparison {case}: {text}"
        );
        assert!(!text.contains("PRIVATE_INVALID_TIME"), "{text}");
        assert!(text.len() <= ENGRAM_CHECK_SUMMARY_MAX_BYTES);
        assert!(
            row.get("refs")
                .is_none_or(|refs| !refs.as_array().unwrap().contains(&json!("exit:0")))
        );
    }
}
