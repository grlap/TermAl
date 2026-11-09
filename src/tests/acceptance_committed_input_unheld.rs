//! An independent requester cannot waive the holder's registered input checks.
use super::*;

#[test]
fn unheld_requester_observes_failed_dirty_and_admitted_committed_inputs() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for case in ["failed", "dirty", "passed", "unsighted"] {
        let (state, requester, root, original) = super::super::successor::successor_fixture();
        let head = run_git_test_command_output(&worktree(&root), &["rev-parse", "HEAD"]);
        let transport = install_sighting_transport(
            &state,
            &root,
            1,
            if case == "unsighted" {
                json!({"state":"absent"})
            } else {
                present_sighting()
            },
        );
        let registration = register_committed_run(
            &state,
            &original.named_by_session,
            &root,
            committed_results(
                if case == "failed" { "failed" } else { "passed" },
                input_record(&head, "clean"),
            ),
        );
        if case != "failed" {
            host_observed(&state, &registration, &head, acknowledged("received"));
        }
        if case == "dirty" {
            fs::write(
                worktree(&root).join("judged.txt"),
                "changed after passing run\n",
            )
            .unwrap();
            acceptance_owner_reports_dirty(&state, &requester, &root, "judged.txt");
        }
        let base = independent_reader();
        let response = state.request_acceptance_evaluation_with_runner(
            &requester,
            evaluation_request(Some(Agent::Codex)),
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "held") {
                    assert_eq!(connection.session_id, requester);
                    Ok(json!({"items":[],"omitted":0}))
                } else {
                    base(connection, args, timeout)
                }
            },
        );
        if case == "passed" {
            let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response.unwrap()
            else {
                panic!("an eligible committed input with a sighting admits the coordinator");
            };
            let target = delegation
                .delegation
                .acceptance_evaluation
                .as_ref()
                .unwrap();
            assert_judges_the_admitted_commit(target, &registration, &root, &head, "received");
            assert_eq!(
                sighting_reads(&transport),
                1,
                "the unheld route must also confirm the canonical root"
            );
        } else {
            let error = refusal(&state, response, case);
            assert_eq!(error.status, StatusCode::CONFLICT, "{case}: {error:?}");
            assert!(
                error.message.contains(match case {
                    "failed" => "eligible",
                    "dirty" => "clean",
                    "unsighted" => "has no sighting",
                    _ => unreachable!(),
                }),
                "{case}: {error:?}"
            );
        }
    }
}
