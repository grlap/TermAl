// Owns the one operator action that resumes every paused local session queue
// at once, after a provider outage or a subscription change, and the listing
// that shows each paused session with its pause reason before and after it.
// Does not decide how a queue resumes: every resumed session goes through
// the existing local per-session Resume
// (`AppState::resume_local_session_queue_fenced`), fenced so that an
// operator's Stop after the plan still wins. Never resumes on its own; only an
// explicit, non-dry-run request resumes anything.
// New module; the per-session path it calls lives in session_lifecycle.rs.

use super::*;
use crate::session_lifecycle::OperatorStopFence;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ResumePausedQueuesRequest {
    /// List what the action would do without resuming anything. Required, so
    /// that no request resumes by leaving it out.
    pub(crate) dry_run: bool,
    /// When given, only these sessions may resume, such as the ones a preview
    /// showed the operator; any other paused session is skipped.
    #[serde(default)]
    pub(crate) session_ids: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ResumePausedQueueAction {
    Resume,
    Skip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResumePausedQueueSkipReason {
    /// An operator paused this queue explicitly, by Stop; only that
    /// session's own Resume lifts it.
    OperatorPaused,
    /// An operator deleted this session and the Delete is held until its
    /// observation is reported; only that session's own Resume cancels it.
    DeletePending,
    /// The request named the sessions it may resume, and not this one.
    NotPreviewed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ResumePausedQueueOutcome {
    /// The queue's pause is lifted.
    Resumed,
    /// The Resume was accepted but the queue is still paused, for the reason
    /// the row now shows.
    StillPaused,
    Failed,
    /// The run's time budget ran out before this session's turn; nothing was
    /// done to it, and running the action again resumes it.
    NotStarted,
}

/// How long a run keeps starting per-session Resumes. A session not reached
/// by then is reported `notStarted`, so the run answers in bounded time
/// instead of outliving its caller with every outcome lost.
pub(crate) const RESUME_PAUSED_RUN_BUDGET: Duration = Duration::from_secs(120);

/// What a caller waits for a real run's answer: the run budget plus room for
/// the Resume that was started last.
pub(crate) const RESUME_PAUSED_CALL_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResumePausedQueueRow {
    pub(crate) session_id: String,
    pub(crate) name: String,
    pub(crate) queue_pause_reason: Option<QueuePauseReason>,
    pub(crate) action: ResumePausedQueueAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) skip_reason: Option<ResumePausedQueueSkipReason>,
    /// Present only after a real run, for a row the action resumed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) outcome: Option<ResumePausedQueueOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    /// The operator's Stops seen when this row was planned.
    #[serde(skip)]
    pub(crate) fence: Option<OperatorStopFence>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResumePausedQueuesResponse {
    pub(crate) dry_run: bool,
    pub(crate) sessions: Vec<ResumePausedQueueRow>,
    /// Paused delegation children are not listed or resumed here: their
    /// parent session owns them. Counted so the operator knows they exist.
    pub(crate) paused_delegation_sessions_not_shown: usize,
}

impl AppState {
    /// Every paused local root session, each marked resume or skip, and the
    /// number of paused delegation sessions left out. With `allowed`, a
    /// session outside it is skipped. Reads only.
    pub(crate) fn paused_queue_plan(
        &self,
        allowed: Option<&[String]>,
    ) -> (Vec<ResumePausedQueueRow>, usize) {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let mut rows = Vec::new();
        let mut delegation_sessions = 0;
        for record in &inner.sessions {
            if !record.orchestrator_auto_dispatch_blocked || !record.is_local_session() {
                continue;
            }
            if record.session.parent_delegation_id.is_some() {
                delegation_sessions += 1;
                continue;
            }
            if record.hidden {
                continue;
            }
            let skip_reason = if record.engram.observation_delete_requested {
                Some(ResumePausedQueueSkipReason::DeletePending)
            } else if record.engram.operator_paused {
                Some(ResumePausedQueueSkipReason::OperatorPaused)
            } else if allowed.is_some_and(|ids| !ids.contains(&record.session.id)) {
                Some(ResumePausedQueueSkipReason::NotPreviewed)
            } else {
                None
            };
            rows.push(ResumePausedQueueRow {
                session_id: record.session.id.clone(),
                name: record.session.name.clone(),
                queue_pause_reason: record.queue_pause_reason.clone(),
                action: if skip_reason.is_some() {
                    ResumePausedQueueAction::Skip
                } else {
                    ResumePausedQueueAction::Resume
                },
                skip_reason,
                outcome: None,
                error: None,
                fence: Some(OperatorStopFence::capture(record)),
            });
        }
        (rows, delegation_sessions)
    }

    /// The operator's action. A dry run lists the plan and changes nothing;
    /// otherwise each row marked resume goes through the per-session Resume,
    /// one at a time, and its outcome is recorded on the row. A session an
    /// operator Stopped after the plan is skipped, not resumed.
    pub(crate) fn resume_paused_queues(
        &self,
        dry_run: bool,
        allowed: Option<&[String]>,
    ) -> ResumePausedQueuesResponse {
        self.resume_paused_queues_until(
            dry_run,
            allowed,
            std::time::Instant::now() + RESUME_PAUSED_RUN_BUDGET,
        )
    }

    /// `resume_paused_queues`, starting no Resume at or after `deadline`.
    pub(crate) fn resume_paused_queues_until(
        &self,
        dry_run: bool,
        allowed: Option<&[String]>,
        deadline: std::time::Instant,
    ) -> ResumePausedQueuesResponse {
        let (mut sessions, paused_delegation_sessions_not_shown) = self.paused_queue_plan(allowed);
        if !dry_run {
            for row in &mut sessions {
                if row.action != ResumePausedQueueAction::Resume {
                    continue;
                }
                if std::time::Instant::now() >= deadline {
                    row.outcome = Some(ResumePausedQueueOutcome::NotStarted);
                } else {
                    self.resume_planned_row(row);
                }
            }
        }
        ResumePausedQueuesResponse {
            dry_run,
            sessions,
            paused_delegation_sessions_not_shown,
        }
    }

    pub(crate) fn resume_planned_row(&self, row: &mut ResumePausedQueueRow) {
        let result = self.resume_local_session_queue_fenced(&row.session_id, row.fence);
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = inner
            .find_visible_session_index(&row.session_id)
            .map(|index| &inner.sessions[index]);
        let stopped_since =
            record.is_some_and(|record| row.fence.is_some_and(|fence| !fence.matches(record)));
        if let Some(record) = record.filter(|_| stopped_since) {
            if record.engram.observation_delete_requested {
                // The operator's Delete came during this action; it stays.
                row.action = ResumePausedQueueAction::Skip;
                row.skip_reason = Some(ResumePausedQueueSkipReason::DeletePending);
            } else if record.engram.operator_paused {
                // The operator's Stop came during this action; it wins whether
                // or not this Resume got in first.
                row.action = ResumePausedQueueAction::Skip;
                row.skip_reason = Some(ResumePausedQueueSkipReason::OperatorPaused);
            } else {
                // The operator Stopped and then Resumed it themselves: report
                // the queue as it is now, without this action's result.
                row.outcome = Some(if record.orchestrator_auto_dispatch_blocked {
                    ResumePausedQueueOutcome::StillPaused
                } else {
                    ResumePausedQueueOutcome::Resumed
                });
            }
            row.queue_pause_reason = record.queue_pause_reason.clone();
            return;
        }
        match result {
            Ok(()) => match record {
                Some(record) if record.orchestrator_auto_dispatch_blocked => {
                    row.outcome = Some(ResumePausedQueueOutcome::StillPaused);
                    row.queue_pause_reason = record.queue_pause_reason.clone();
                }
                Some(_) => row.outcome = Some(ResumePausedQueueOutcome::Resumed),
                None => {
                    row.outcome = Some(ResumePausedQueueOutcome::Failed);
                    row.error = Some("the session no longer exists".to_owned());
                }
            },
            Err(error) => {
                row.outcome = Some(ResumePausedQueueOutcome::Failed);
                row.error = Some(error.message);
            }
        }
    }
}

/// Whether a request may run the action as far as its browser headers say.
/// A browser's Fetch Metadata must say the request comes from this origin.
/// Without it, an `Origin` header must name the origin the request was sent
/// to. A request with neither, such as the CLI's, is not a browser request.
fn resume_paused_request_is_same_origin(headers: &axum::http::HeaderMap) -> bool {
    if let Some(site) = headers.get("sec-fetch-site") {
        return matches!(site.to_str(), Ok("same-origin" | "none"));
    }
    let Some(origin) = headers.get(axum::http::header::ORIGIN) else {
        return true;
    };
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|host| host.to_str().ok());
    match (origin.to_str().ok(), host) {
        (Some(origin), Some(host)) => origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
            .is_some_and(|authority| authority.eq_ignore_ascii_case(host)),
        _ => false,
    }
}

/// Lists, or with `dryRun: false` resumes, every paused local session queue.
/// The body must be JSON, which a browser cannot send to another origin
/// without a preflight that CORS refuses; a browser request from another
/// origin is refused as well.
pub(crate) async fn resume_paused_queues(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<ResumePausedQueuesRequest>, JsonRejection>,
) -> Result<Json<ResumePausedQueuesResponse>, ApiError> {
    if !resume_paused_request_is_same_origin(&headers) {
        return Err(ApiError::from_status(
            StatusCode::FORBIDDEN,
            "Resume paused sessions only from TermAl itself",
        ));
    }
    let Json(request) = request.map_err(|e| api_json_rejection("resume-paused request", e))?;
    let response = run_blocking_api(move || {
        Ok(state.resume_paused_queues(request.dry_run, request.session_ids.as_deref()))
    })
    .await?;
    Ok(Json(response))
}
