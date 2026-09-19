// Owns acceptance outcome text in parent evaluator cards and post-submit refresh.
// Does not own verdict execution, durable acknowledgement or card rendering.
// New feature boundary alongside acceptance_settings.rs; projects receipt truth onto cards.

fn acceptance_card_detail(
    inner: &StateInner,
    delegation: &DelegationRecord,
    detail: &str,
    terminal: bool,
) -> String {
    let Some(evaluation) = delegation.acceptance_evaluation.as_ref() else {
        return detail.to_owned();
    };
    // Recorded is installed before its persistence fence is acknowledged.
    // A card must not promote that intermediate value into a durable verdict.
    let outcome = if inner
        .acceptance_evaluation_submissions_in_flight
        .contains(&delegation.id)
    {
        format!(
            "Acceptance evaluation of `{}` ({}): submission in progress; awaiting confirmation.",
            acceptance_brief_text(&evaluation.work_ref, MAX_ACCEPTANCE_BRIEF_LABEL_CHARS),
            evaluation.mode.word()
        )
    } else if matches!(evaluation.submission, AcceptanceEvaluationSubmission::None) && !terminal {
        format!(
            "Acceptance evaluation of `{}` ({}): evaluating; no verdict recorded yet.",
            acceptance_brief_text(&evaluation.work_ref, MAX_ACCEPTANCE_BRIEF_LABEL_CHARS),
            evaluation.mode.word()
        )
    } else {
        acceptance_evaluation_outcome_line(evaluation)
    };
    // Child prose describes the run, never substitutes for its tracker receipt.
    format!("{outcome} · {detail}")
}

impl AppState {
    // Presentation is best-effort; a card persistence failure cannot undo or
    // reclassify the tracker's acknowledged submission outcome.
    fn refresh_acceptance_evaluation_card(&self, child: &str) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(delegation) = inner
            .delegations
            .iter()
            .find(|d| d.child_session_id == child && d.acceptance_evaluation.is_some())
            .cloned()
        else {
            return;
        };
        let status = match delegation.status {
            DelegationStatus::Completed => ParallelAgentStatus::Completed,
            DelegationStatus::Failed | DelegationStatus::Canceled => ParallelAgentStatus::Error,
            _ => ParallelAgentStatus::Running,
        };
        let detail = delegation
            .result
            .as_ref()
            .map(|r| compact_delegation_public_summary(&r.summary))
            .unwrap_or_else(|| delegation_running_detail_locked(&inner, &delegation));
        if parent_delegation_card_matches_locked(&inner, &delegation, status, &detail) {
            return;
        }
        let Some(delta) =
            update_parent_delegation_card_locked(&mut inner, &delegation, status, detail)
        else {
            return;
        };
        match self.commit_locked(&mut inner) {
            Ok(revision) => self.publish_parent_delegation_card_delta(revision, delta),
            Err(error) => eprintln!("acceptance card refresh failed: {error:#}"),
        }
    }
}
