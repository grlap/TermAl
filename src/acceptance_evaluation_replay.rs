// Parent recovery of an ended evaluator's retained write. The parent supplies
// only a delegation id; verdicts, source bytes, actor and attempt key all come
// from the durable submission. Uses the ordinary submission flight and writer.

const TERMAL_REPLAY_ACCEPTANCE_EVALUATION_TOOL_NAME: &str = "termal_replay_acceptance_evaluation";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReplayAcceptanceEvaluationRequest {
    delegation_id: String,
}

#[derive(Serialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum AcceptanceEvaluationReplayResponse {
    Recorded {
        #[serde(flatten)]
        submission: AcceptanceEvaluationSubmitResponse,
    },
    Refused {
        delegation_id: String,
        attempt_key: String,
        code: Option<String>,
        message: String,
        // A refused resend cannot prove that the original send did not commit.
        original_outcome_unknown: bool,
    },
}

fn acceptance_replay_durability_error(error: ApiError) -> ApiError {
    if error.is_enqueued_durability_deadline() {
        error.at_durability_boundary(DurabilityPendingOperation::AcceptanceEvaluationSubmission)
    } else {
        error
    }
}

fn acceptance_evaluation_replay_tool_definition() -> Value {
    json!({
        "name": TERMAL_REPLAY_ACCEPTANCE_EVALUATION_TOOL_NAME,
        "description": "Resend an ended evaluator's retained Unconfirmed acceptance submission once. Parent root only. Supply its delegationId, never verdicts or a new key. TermAl verifies the stored digest and sends the original payload, source and evaluator actor under the original attempt key. Recorded attempts refuse without another write. A timeout stays Unconfirmed; a typed refused resend also leaves the original outcome unknown. This creates no evaluator or new judgment.",
        "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        "inputSchema": {"type": "object", "additionalProperties": false,
            "required": ["delegationId"], "properties": {
                "delegationId": {"type": "string", "minLength": 1, "maxLength": 160}
            }}
    })
}

impl TermalDelegationMcpBridge {
    fn tool_replay_acceptance_evaluation(&self, arguments: Value) -> Result<Value> {
        let request: ReplayAcceptanceEvaluationRequest = serde_json::from_value(arguments)
            .context("invalid acceptance evaluation replay request")?;
        self.post_long_call(
            DelegationLongCall::EvaluationSubmit,
            &format!("/api/sessions/{}/acceptance-evaluation-replay", self.serving_session_id),
            &json!({"delegationId": request.delegation_id}),
        ).map_err(|error| {
            if termal_delegation_outcome_is_unknown(&error) {
                return termal_delegation_unknown_outcome(&error, format!(
                    "the retained evaluation replay outcome is unknown; inspect the delegation's stored submission before another request: {error}"
                ));
            }
            error
        })
    }
}

impl AppState {
    fn replay_acceptance_evaluation(
        &self,
        parent: &str,
        request: ReplayAcceptanceEvaluationRequest,
    ) -> Result<AcceptanceEvaluationReplayResponse, ApiError> {
        let cwd = self.engram_host_launch_workdir();
        self.replay_acceptance_evaluation_with_runner(
            parent,
            request,
            |connection, args, timeout| {
                run_acceptance_evaluation_submit(connection, args, timeout, &cwd)
            },
        )
    }

    fn replay_acceptance_evaluation_with_runner(
        &self,
        parent: &str,
        request: ReplayAcceptanceEvaluationRequest,
        run: impl Fn(
            &EngramConnectionConfig,
            &[String],
            Duration,
        ) -> std::result::Result<EngramCliOutput, EngramTransportError>,
    ) -> Result<AcceptanceEvaluationReplayResponse, ApiError> {
        if request.delegation_id.is_empty() || request.delegation_id.len() > 160 {
            return Err(ApiError::bad_request("invalid replay delegationId"));
        }
        // Acquire the same guard as child submission and requester follow-up.
        // No tracker or filesystem IO occurs under this lock.
        let (authority, mut target, written, digest, original, earlier, _flight) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let parent_record = inner
                .find_session_index(parent)
                .map(|i| &inner.sessions[i])
                .filter(|r| !r.hidden && r.is_local_session())
                .ok_or_else(|| ApiError::conflict("acceptance_replay_parent_unavailable"))?;
            if parent_record.session.parent_delegation_id.is_some()
                || AppState::engram_session_has_child_binding_shape_locked(&inner, parent)
            {
                return Err(ApiError::conflict("acceptance_replay_requires_parent_root"));
            }
            let index =
                find_parent_delegation_index_locked(&inner, parent, &request.delegation_id)?;
            let delegation = &inner.delegations[index];
            if delegation.mode != DelegationMode::Evaluator
                || !matches!(delegation.write_policy, DelegationWritePolicy::ReadOnly)
                || !delegation_is_terminal(delegation.status)
            {
                return Err(ApiError::conflict(
                    "acceptance_replay_requires_ended_evaluator",
                ));
            }
            if inner
                .acceptance_evaluation_submissions_in_flight
                .contains_key(&request.delegation_id)
                || inner
                    .delegation_followup_admissions
                    .contains_key(&request.delegation_id)
            {
                return Err(ApiError::conflict("acceptance_replay_in_progress"));
            }
            let evaluation = delegation
                .acceptance_evaluation
                .as_ref()
                .ok_or_else(|| ApiError::conflict("acceptance_replay_missing_target"))?;
            if matches!(
                evaluation.submission,
                AcceptanceEvaluationSubmission::Recorded { .. }
            ) {
                return Err(ApiError::conflict(
                    ACCEPTANCE_EVALUATION_ALREADY_RECORDED_ERROR,
                ));
            }
            let AcceptanceEvaluationSubmission::Unconfirmed {
                payload_digest,
                original,
                reason,
                ..
            } = &evaluation.submission
            else {
                return Err(ApiError::conflict(
                    "acceptance_replay_requires_unconfirmed_submission",
                ));
            };
            if evaluation
                .attempt_history
                .as_ref()
                .and_then(|h| h.prepared_brief.as_ref())
                .is_some()
            {
                return Err(ApiError::conflict("acceptance_replay_brief_in_progress"));
            }
            if original.args.is_empty() || original.digest()? != *payload_digest {
                return Err(ApiError::conflict(
                    "acceptance_replay_payload_digest_mismatch",
                ));
            }
            if original.args.get(4) != Some(&delegation.child_session_id)
                || original
                    .args
                    .windows(2)
                    .filter(|pair| pair[0] == "--attempt")
                    .map(|pair| &pair[1])
                    .collect::<Vec<_>>()
                    != vec![&evaluation.attempt_key]
            {
                return Err(ApiError::conflict(
                    "acceptance_replay_original_identity_mismatch",
                ));
            }
            let target = acceptance_evaluation_host_target_locked(&inner, parent)?;
            if evaluation.store.as_ref() != Some(&target.store) {
                return Err(ApiError::conflict(
                    ACCEPTANCE_EVALUATION_STORE_CHANGED_ERROR,
                ));
            }
            let authority = AcceptanceEvaluationSubmitAuthority {
                delegation_id: request.delegation_id.clone(),
                target: evaluation.clone(),
            };
            let written = delegation.clone();
            let digest = payload_digest.clone();
            let original = original.clone();
            let earlier = reason.clone();
            let flight = AcceptanceEvaluationSubmissionInFlight::admit_locked(
                self,
                &mut inner,
                &request.delegation_id,
            )
            .ok_or_else(|| ApiError::conflict("acceptance_replay_in_progress"))?;
            // Release refreshes the terminal result only after the durable ACK.
            // Failed/refused resends retain their existing unavailable result.
            if let Some(refresh) = inner
                .acceptance_evaluation_submissions_in_flight
                .get_mut(&request.delegation_id)
            {
                *refresh = true;
            }
            (
                authority, target, written, digest, original, earlier, flight,
            )
        };
        validate_work_read_target(&target)?;
        self.confirm_acceptance_evaluation_submission_durable_classified(&authority, &written)
            .map_err(acceptance_replay_durability_error)?;
        let (actor, context) = acceptance_evaluation_args_identity(&original.args)
            .ok_or_else(|| ApiError::conflict("acceptance_replay_original_identity_unavailable"))?;
        target.connection.actor_id = actor;
        target.connection.actor_context = context;
        target.connection.session_id = original.args[4].clone();
        validate_acceptance_evaluation_command_size(&target.connection, &original.args)
            .map_err(|_| ApiError::conflict(
                "the saved evaluation command exceeds this host's command-line capacity; retain the original payload and inspect the task in the tracker before recovery on another host"
            ))?;
        let directory = engram_host_workdir(self.persistence_path.as_ref())
            .map_err(|_| ApiError::internal("evaluation source storage is unavailable"))?
            .join("evaluation-sources");
        let file = acceptance_source_replay_file(&original, &digest, None, &directory)?;
        // Exactly one send. The original unknown send already consumed the
        // first attempt; this request is its supported identical resend.
        let receipt = match run_acceptance_source_payload(
            &target.connection,
            &original,
            file.as_ref(),
            &run,
        ) {
            AcceptanceEvaluationRunOutcome::Receipt(receipt) => receipt,
            AcceptanceEvaluationRunOutcome::Refused { code, message, .. } => {
                self.keep_acceptance_evaluation_unconfirmed(
                    &authority,
                    digest,
                    original,
                    Some(&earlier),
                    &format!("the identical resend was refused: {message}"),
                );
                return Ok(AcceptanceEvaluationReplayResponse::Refused {
                    delegation_id: authority.delegation_id,
                    attempt_key: authority.target.attempt_key,
                    code,
                    message,
                    original_outcome_unknown: true,
                });
            }
            AcceptanceEvaluationRunOutcome::Unknown(detail)
            | AcceptanceEvaluationRunOutcome::NeverStarted(detail)
            | AcceptanceEvaluationRunOutcome::Locked(detail) => {
                let reason = self.keep_acceptance_evaluation_unconfirmed(
                    &authority,
                    digest,
                    original,
                    Some(&earlier),
                    &detail,
                );
                return Err(ApiError::bad_gateway(format!(
                    "acceptance_replay_unconfirmed: the original write outcome remains unknown: {reason}"
                )));
            }
        };
        let recorded_at = stamp_now();
        self.settle_open_acceptance_evaluation_submission_classified(
            &authority,
            AcceptanceEvaluationSubmission::Recorded {
                receipt: acceptance_evaluation_receipt_extract(&receipt),
                recorded_at: recorded_at.clone(),
            },
        )
        .map_err(|mut error| {
            if error.is_enqueued_durability_deadline() {
                return acceptance_replay_durability_error(error);
            }
            error.message = format!(
                "the tracker answered with a receipt, but TermAl could not confirm it persisted; replay the same delegation unchanged to recover its receipt: {}",
                error.message
            );
            error
        })?;
        if let Some(file) = &file {
            file.terminal();
        }
        let (receipt, receipt_truncated) = bounded_acceptance_evaluation_receipt(receipt);
        Ok(AcceptanceEvaluationReplayResponse::Recorded {
            submission: AcceptanceEvaluationSubmitResponse {
                schema_version: ACCEPTANCE_EVALUATION_SUBMISSION_SCHEMA_VERSION,
                delegation_id: authority.delegation_id,
                work_ref: authority.target.work_ref,
                mode: authority.target.mode,
                attempt_key: authority.target.attempt_key,
                recorded_at,
                receipt,
                receipt_truncated,
            },
        })
    }
}

async fn replay_session_acceptance_evaluation(
    AxumPath(parent): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<ReplayAcceptanceEvaluationRequest>, JsonRejection>,
) -> Result<Json<AcceptanceEvaluationReplayResponse>, ApiError> {
    let Json(request) =
        request.map_err(|e| api_json_rejection("acceptance evaluation replay", e))?;
    let permit =
        acquire_acceptance_evaluation_permit(ACCEPTANCE_EVALUATION_SUBMIT_PERMITS.clone())?;
    run_blocking_api(move || {
        let _permit = permit;
        state
            .replay_acceptance_evaluation(&parent, request)
            .map(Json)
    })
    .await
}
