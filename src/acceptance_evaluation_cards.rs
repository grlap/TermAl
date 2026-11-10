// Owns acceptance outcome text in evaluator results/cards and post-submit refresh.
// Does not own verdict execution, durable acknowledgement or card rendering.
// Projects receipt truth alongside acceptance_settings.rs; never interprets child verdicts.

fn acceptance_evaluator_unavailable_result(
    delegation: &DelegationRecord,
) -> Option<(&'static str, &'static str)> {
    if delegation.mode != DelegationMode::Evaluator || delegation_submission_recorded(delegation) {
        return None;
    }
    Some(
        if delegation
            .acceptance_evaluation
            .as_ref()
            .is_some_and(|target| !target.submission.is_none())
        {
            (
                "Evaluator finished with an unconfirmed submission outcome. Reconcile the retained tracker write; no evaluator verdict is confirmed.",
                "Submission recording remains pending or unconfirmed; the retained write was preserved.",
            )
        } else {
            (
                "Evaluator finished: no submission recorded. The child's output is diagnostic text, not an acceptance verdict.",
                "No submission recorded; acceptance verdict is unavailable.",
            )
        },
    )
}

fn acceptance_evaluator_terminal_outcome(
    inner: &mut StateInner,
    delegation: &DelegationRecord,
    outcome: DelegationChildOutcome,
) -> DelegationChildOutcome {
    if delegation.mode != DelegationMode::Evaluator {
        return outcome;
    }
    // The submit owner may temporarily expose Recorded before its durability
    // acknowledgement. Its guard refreshes this lifecycle when it releases.
    if let Some(refresh_deferred) = inner
        .acceptance_evaluation_submissions_in_flight
        .get_mut(&delegation.id)
    {
        *refresh_deferred |= !matches!(&outcome, DelegationChildOutcome::Running);
        return DelegationChildOutcome::Running;
    }
    // A result packet describes the run, never the tracker's judgment. Keep
    // actual transport failures separate and preserve the paged child output.
    let (changed_files, files_inspected, commands_run) = match outcome {
        DelegationChildOutcome::Completed {
            changed_files,
            files_inspected,
            commands_run,
            ..
        } => (changed_files, files_inspected, commands_run),
        DelegationChildOutcome::IdleWithoutResult => (Vec::new(), Vec::new(), Vec::new()),
        DelegationChildOutcome::Failed { .. }
            if inner
                .find_session_index(&delegation.child_session_id)
                .is_some_and(|i| {
                    delegation_child_result_for_status(&inner.sessions[i]).is_some()
                }) =>
        {
            (Vec::new(), Vec::new(), Vec::new())
        }
        other => return other,
    };
    DelegationChildOutcome::Completed {
        summary: delegation
            .acceptance_evaluation
            .as_ref()
            .map(acceptance_evaluation_outcome_line)
            .unwrap_or_else(|| "Evaluator finished without an acceptance target.".to_owned()),
        findings: Vec::new(),
        changed_files,
        files_inspected,
        commands_run,
        notes: vec![
            "Human-readable child output remains available through paged full-output reads."
                .to_owned(),
        ],
    }
}

// Cards take raw run detail, while the public evaluator result independently
// names its receipt. Do not decorate that receipt line a second time.
fn delegation_result_card_detail(
    delegation: &DelegationRecord,
    result: &DelegationResult,
) -> String {
    if delegation.mode == DelegationMode::Evaluator && result.status == DelegationStatus::Completed
    {
        "Child output is diagnostic, not an acceptance verdict.".to_owned()
    } else {
        compact_delegation_public_summary(&result.summary)
    }
}

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
        .contains_key(&delegation.id)
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
            .map(|r| delegation_result_card_detail(&delegation, r))
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
            Ok(revision) => self.publish_parent_delegation_card_delta(&inner, revision, delta),
            Err(error) => eprintln!("acceptance card refresh failed: {error:#}"),
        }
    }
}
