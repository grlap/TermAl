// Owns the local operator HTTP contract for one executor operation and its queue.
// Does not authenticate remote RPCs, execute work, or change queue/journal state.
// Split from the federation operator adapter; the shared operation-view helper
// owns journal reading and the exact recipient snapshot.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationExecutorObservationQuery {
    central_host_id: String,
    operation_key: String,
}

impl FederationExecutorObservationQuery {
    fn validate(&self) -> Result<(), ApiError> {
        for (text, limit) in [(&self.central_host_id, 256), (&self.operation_key, 1024)] {
            if text.trim().is_empty() || text.len() > limit || text.contains('\0') {
                return Err(ApiError::bad_request("invalid executor observation query"));
            }
        }
        Ok(())
    }
}

async fn get_federation_executor_operation(
    State(state): State<AppState>,
    query: Result<Query<FederationExecutorObservationQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(query) =
        query.map_err(|_| ApiError::bad_request("invalid executor observation query"))?;
    query.validate()?;
    run_blocking_api(move || state.federation_executor_operation_operator_view(query))
        .await
        .map(Json)
}

impl AppState {
    fn federation_executor_operation_operator_view(
        &self,
        query: FederationExecutorObservationQuery,
    ) -> Result<Value, ApiError> {
        let local = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .federation
            .local
            .clone()
            .ok_or_else(|| {
                ApiError::from_status(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "executor host identity is unavailable",
                )
            })?;
        let view = self
            .read_federation_executor_operation_view(&query.central_host_id, &query.operation_key)
            .map_err(|_| ApiError::internal("executor observation could not be read"))?
            .ok_or_else(|| ApiError::not_found("executor operation not found"))?;
        if self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .federation
            .local
            .as_ref()
            != Some(&local)
        {
            return Err(ApiError::conflict(
                "executor host identity changed; repeat the read",
            ));
        }
        let availability = if !view.executor_local {
            "notLocalExecutor"
        } else if !view.root_present {
            "missing"
        } else if !view.binding_current {
            "staleBinding"
        } else {
            "available"
        };
        let available = availability == "available";
        let pending_prompts = available.then(|| {
            view.pending_prompts
                .iter()
                .map(|prompt| {
                    json!({
                        "id": prompt.prompt_id,
                        "timestamp": prompt.timestamp,
                        "source": prompt.source,
                        "mailboxId": prompt.mailbox_id,
                        "messageId": prompt.message_id,
                        "sequence": prompt.sequence,
                    })
                })
                .collect::<Vec<_>>()
        });
        Ok(json!({
            "operation": {
                "operationKey": view.operation.operation_key,
                "commandKind": view.operation.command_kind,
                "targetBinding": view.operation.mailbox_binding(),
                "enrollmentGeneration": view.operation.enrollment_generation,
                "stage": view.stage,
                "effectState": view.effect_state,
                "queueEntryId": view.queue_entry_id,
                "runtimeAckPresent": view.runtime_acknowledged,
                "mirror": !view.executor_local,
                "bindingCurrent": view.binding_current,
            },
            "recipient": {
                "sessionId": view.operation.session_id,
                "availability": availability,
                "status": available.then_some(view.root_status).flatten(),
                "queuePaused": available.then_some(view.root_paused),
                "stopping": available.then_some(view.root_stopping),
                "receiptDisposition": available.then_some(view.receipt).flatten(),
                "queuePosition": available.then_some(view.queue_position).flatten(),
                "pendingPrompts": pending_prompts,
                "pendingPromptCount": available.then_some(view.pending_prompt_count),
                "pendingPromptsTruncated": available.then_some(view.pending_prompts_truncated),
            },
        }))
    }
}
