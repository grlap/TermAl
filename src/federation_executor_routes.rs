// Owns the authenticated durable-receive endpoint. It exposes no operator API.
// A canceled HTTP waiter cannot release the blocking storage worker's permit.

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationExecutorReceipt {
    protocol_version: u16,
    operation: FederationExecutorOperation,
    payload_digest: String,
    received: bool,
}

fn federation_executor_routes() -> Router<FederationListenerState> {
    Router::new().route(FEDERATION_EXECUTOR_WAKE_ROUTE, post(serve_federation_executor_wake))
        .layer(DefaultBodyLimit::max(32 * 1024))
}

async fn serve_federation_executor_wake(
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
    Json(request): Json<FederationExecutorWakeRequest>,
) -> Result<(StatusCode, Json<FederationExecutorReceipt>), ApiError> {
    let lease = peer.lease.ok_or_else(federation_stale_link)?;
    let app = lease.channel.app.clone().ok_or_else(federation_transport_unavailable)?;
    let permit = peer.mailbox_permit.ok_or_else(federation_transport_unavailable)?;
    let receive = app.federation_executor_wake_signal.begin_receive()?;
    run_blocking_api(move || {
        let _receive = receive;
        let basis = lease.with_current(&app, |_, basis| Ok(basis.clone()))?;
        let operation = request.operation(basis.enrollment_generation)?;
        let authority = FederationExecutorAuthority { app: &app, operation: &operation, lease: Some(&lease) };
        app.refresh_federation_executor_reservation_domain()?;
        app.mailbox_store.admit_federation_executor_operation_while_running(&operation,
            || {
                app.federation_executor_wake_signal.check_receiving()?;
                #[cfg(test)]
                app.federation_executor_wake_signal.observe("receiveWriterAdmission");
                Ok(())
            }, || {
            let authority = authority.lock()?;
            let live = app.federation_executor_wake_signal.live.lock().expect("live activation mutex poisoned");
            let work = app.federation_executor_wake_signal.activation_work.lock().expect("activation work mutex poisoned");
            #[cfg(test)]
            app.federation_executor_wake_signal.observe("receiveAuthorized");
            Ok(FederationExecutorReceiveGuard { authority, live, work })
        }, |guard, record| app.capture_federation_executor_receive(guard, record, permit))
            .map_err(mailbox_api_error)?;
        // Session queue/fence/provider work belongs exclusively to the owned
        // supervisor worker after this durable receipt, outside HTTP's budget.
        Ok((StatusCode::ACCEPTED, Json(FederationExecutorReceipt {
            protocol_version: FEDERATION_PROTOCOL_VERSION,
            payload_digest: operation.digest(), operation, received: true,
        })))
    }).await
}
