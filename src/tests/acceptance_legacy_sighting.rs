// Revision-only sightings remain readable, but cannot admit an evaluation.
// Exercise the real request's Legacy fallback before either a brief or an
// evaluator exists. Current-sighting positives remain in the parent suite.
use super::*;

#[test]
fn legacy_and_absent_sightings_refuse_identically_before_any_evaluator_or_brief() {
    for mode in ["same_session", "sub_agent", "independent_session"] {
        let (state, parent, _store, named_root) = named_root_parent();
        let mut absent_refusal = None;
        for sighting in [
            json!({"state": "absent"}),
            json!({"state": "present", "record": "historical-sighting", "position": 2,
                "revision": "content-v1:historical"}),
        ] {
            let transport = install_sighting_transport(&state, &named_root, 1, sighting);
            let response = state.request_acceptance_evaluation_with_runner(
                &parent,
                evaluation_request((mode != "same_session").then_some(Agent::Codex)),
                reader_admitting(mode),
            );
            let error = refusal(&state, response, mode);
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert_eq!(
                error.kind,
                Some(ApiErrorKind::AcceptanceEvaluationNamedRootSightingUnavailable)
            );
            assert_eq!(
                transport
                    .requests()
                    .iter()
                    .filter(|request| {
                        request.request["operation"] == "named_root_sighting_read"
                    })
                    .count(),
                1
            );
            let observed = (error.status, error.kind, error.message);
            if let Some(expected) = &absent_refusal {
                assert_eq!(
                    &observed, expected,
                    "both shapes give the same refusal and remedy"
                );
            } else {
                absent_refusal = Some(observed);
            }
        }
    }
}
