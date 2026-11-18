// Exercise the shared Codex router and ordinary measured carry owners.
use super::*;

fn arm_opening(turn: &EOCheckedTurn, runtime: &SharedCodexRuntime) -> u64 {
    let generation = turn.record(|record| record.active_turn_generation);
    let mut sessions = runtime.sessions.lock().unwrap();
    let shared = sessions.get_mut(&turn.session_id).unwrap();
    shared.active_turn_generation = Some(generation);
    shared.turn_id = None;
    shared.turn_started = false;
    shared.pending_turn_start_request_id = Some(format!("open-{generation}"));
    generation
}

fn route(turn: &EOCheckedTurn, runtime: &SharedCodexRuntime, message: Value) {
    handle_shared_codex_app_server_message(
        &message,
        &turn.state,
        &runtime.runtime_id,
        &Arc::new(Mutex::new(HashMap::new())),
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
    )
    .unwrap();
}

fn start_size_test(
    turn: &EOCheckedTurn,
    runtime: &SharedCodexRuntime,
    worktree: &FsPath,
    native_turn: &str,
) {
    route(
        turn,
        runtime,
        json!({"method":"item/started","params":{
            "threadId": THREAD, "turnId": native_turn, "item":{
                "type":"commandExecution","id":ITEM,"command":SIZE_TEST,"cwd":cwd(worktree)
            }
        }}),
    );
    turn.wait_for_snapshots();
}

fn end_size_test(native_turn: &str, legacy: bool) -> Value {
    if legacy {
        json!({"method":"codex/event/item_completed","params":{
            "conversationId":THREAD,"msg":{"turn_id":native_turn,"item":{
                "type":"CommandExecution","id":ITEM,"command":SIZE_TEST,
                "status":"completed","exitCode":0,"aggregatedOutput":PASSING
            }}
        }})
    } else {
        json!({"method":"item/completed","params":{
            "threadId":THREAD,"turnId":native_turn,"item":{
                "type":"commandExecution","id":ITEM,"command":SIZE_TEST,
                "status":"completed","exitCode":0,"aggregatedOutput":PASSING
            }
        }})
    }
}

fn observation(turn: &EOCheckedTurn, generation: u64) -> TestRunCommandObservation {
    let inner = turn.state.inner.lock().unwrap();
    inner
        .test_run_observations
        .values()
        .find(|row| {
            row.summary.owner_session_id.as_deref() == Some(turn.session_id.as_str())
                && row.turn_generation == generation
                && row.tool_identity == sha256_hex(ITEM.as_bytes())
        })
        .expect("the real provider start was retained")
        .clone()
}

fn close_provider_turn(
    turn: &EOCheckedTurn,
    runtime: &SharedCodexRuntime,
    native_turn: &str,
) -> Value {
    let host_turn = turn.record(|record| {
        record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .unwrap()
            .host_turn_ref
            .clone()
    });
    route(
        turn,
        runtime,
        json!({"method":"turn/completed","params":{
            "threadId":THREAD,"turn":{"id":native_turn,"status":"completed"}
        }}),
    );
    turn.observation_for(&host_turn)
}

#[test]
fn provisional_opening_cannot_settle_through_carry_after_different_turn_proof() {
    let (turn, worktree) = two_turns("provisional-carry");
    let runtime = shared_runtime(&turn);
    let generation = arm_opening(&turn, &runtime);
    route(
        &turn,
        &runtime,
        json!({"method":"turn/started","params":{"threadId":THREAD,"turn":{}}}),
    );
    start_size_test(&turn, &runtime, &worktree, "provisional-a");
    let original = observation(&turn, generation);
    assert!(original.summary.ended_at.is_none());
    turn.record(|record| {
        assert!(
            record
                .engram
                .active_turn_checks
                .iter()
                .any(|check| check.key == ITEM)
        );
    });
    route(
        &turn,
        &runtime,
        json!({"method":"turn/started","params":{"threadId":THREAD,"turn":{"id":"proven-b"}}}),
    );
    let report = close_provider_turn(&turn, &runtime, "proven-b");
    assert!(
        observed_checks(&report)
            .iter()
            .any(|check| check["observed_result"] == "indeterminate")
    );
    turn.record(|record| {
        assert!(
            record
                .engram
                .carried_checks
                .iter()
                .all(|carry| carry.check.key != ITEM)
        );
        assert!(
            record
                .engram
                .running_codex_tests
                .iter()
                .any(|writer| writer.key == ITEM)
        );
    });
    let closed = observation(&turn, generation);
    assert!(closed.same_start(&original));
    route(&turn, &runtime, end_size_test("provisional-a", false));
    assert_eq!(observation(&turn, generation), closed);
    assert!(closed.artifact.is_none() && closed.evidence.is_empty());
    turn.record(|record| {
        assert!(
            record
                .engram
                .running_codex_tests
                .iter()
                .any(|writer| writer.key == ITEM)
        );
    });
    assert_no_automatic_credit(&turn);
}

#[test]
fn addressed_carried_end_settles_the_predecessor_despite_a_reused_item() {
    for legacy in [false, true] {
        let (turn, worktree) = two_turns("exact-carried-predecessor");
        let runtime = shared_runtime(&turn);
        let first_generation = arm_opening(&turn, &runtime);
        route(
            &turn,
            &runtime,
            json!({"method":"turn/started","params":{"threadId":THREAD,"turn":{"id":"old"}}}),
        );
        start_size_test(&turn, &runtime, &worktree, "old");
        let first = observation(&turn, first_generation);
        close_provider_turn(&turn, &runtime, "old");
        turn.record(|record| {
            assert!(
                record
                    .engram
                    .carried_checks
                    .iter()
                    .any(|carry| carry.check.key == ITEM && carry.check.end.is_none())
            );
        });

        dispatch_next_turn(&turn);
        let next_generation = arm_opening(&turn, &runtime);
        route(
            &turn,
            &runtime,
            json!({"method":"turn/started","params":{"threadId":THREAD,"turn":{"id":"new"}}}),
        );
        start_size_test(&turn, &runtime, &worktree, "new");
        let next = observation(&turn, next_generation);
        assert_ne!(first.execution_id, next.execution_id);
        // An unaddressed frame is ambiguous while both executions are open.
        let mut ambiguous = end_size_test("old", false);
        ambiguous["params"]
            .as_object_mut()
            .unwrap()
            .remove("turnId");
        route(&turn, &runtime, ambiguous);
        assert_eq!(observation(&turn, first_generation), first);
        assert_eq!(observation(&turn, next_generation), next);

        // Exercise the typed predecessor END while the successor is open
        // and the legacy predecessor END after the successor has ended.
        if legacy {
            route(&turn, &runtime, end_size_test("new", false));
            assert_eq!(
                observation(&turn, next_generation).summary.exit_code,
                Some(0)
            );
            assert_eq!(observation(&turn, first_generation), first);
        }
        let successor_before_late_end = observation(&turn, next_generation);
        route(&turn, &runtime, end_size_test("old", legacy));
        wait_for_carried_snapshots(&turn);
        let finished = observation(&turn, first_generation);
        assert!(finished.same_start(&first));
        assert_eq!(finished.summary.state, TestRunState::Passed);
        assert_eq!(finished.summary.exit_code, Some(0));
        assert_eq!(
            observation(&turn, next_generation),
            successor_before_late_end
        );
        turn.record(|record| {
            assert!(
                record
                    .engram
                    .carried_checks
                    .iter()
                    .any(|carry| carry.check.key == ITEM && carry.check.end.is_some())
            );
            assert!(
                record
                    .engram
                    .running_codex_tests
                    .iter()
                    .all(|writer| writer.key != ITEM)
            );
            assert_eq!(
                record.engram.running_command_keys.contains_key(ITEM),
                !legacy
            );
        });
        if !legacy {
            route(&turn, &runtime, end_size_test("new", false));
        }
        assert_eq!(
            observation(&turn, next_generation).summary.exit_code,
            Some(0)
        );
        assert_no_automatic_credit(&turn);
    }
}
