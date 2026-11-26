// Selects original native mailbox ownership before domain/relay routing.
// Does not retry refused domain operations against a different store or actor.

use super::*;

pub(crate) const NATIVE_MAILBOX_ROUTE_MISS_LIMIT: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OriginalMailboxLookup {
    Member,
    NoMembership,
    StoreUnavailable,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeMailboxRouteMiss {
    pub(crate) at: String,
    pub(crate) operation: &'static str,
    caller_session_id: Option<String>,
    pub(crate) mailbox_id: Option<String>,
    pub(crate) coordination_store_id: Option<String>,
    native_lookup: OriginalMailboxLookup,
}

impl AppState {
    pub(crate) async fn replay_original_native_mailbox_send(
        &self,
        native: &str,
        request: &SendMailboxMessageRequest,
        test_request: Option<&MailboxTestPayload>,
    ) -> Result<Option<MailboxAppendReceipt>, ApiError> {
        let app = self.clone();
        let native = native.to_owned();
        let request = request.clone();
        let test_request = test_request.cloned();
        run_blocking_api(move || {
            let creation = {
                let inner = app.inner.lock().expect("state mutex poisoned");
                federation_mailbox_root_locked(&inner, &native)?
                    .federation_native_creation_id
                    .clone()
            };
            let stored = {
                let Some(connection) = app.mailbox_store.connection_if_enabled() else {
                    return Ok(None);
                };
                mailbox_message_for_idempotency_key(&connection, &native, &request.idempotency_key)
                    .map_err(mailbox_api_error)?
            };
            let Some(stored) = stored else {
                return Ok(None);
            };
            if stored.test_request != test_request {
                return Err(ApiError::conflict(
                    "mailbox retry changed its original test-request custody",
                ));
            }
            let target_matches = stored.target_session_id == request.target_session_id
                || app
                    .canonical_local_native_mailbox_target(&request.target_session_id)?
                    .as_deref()
                    == Some(stored.target_session_id.as_str());
            if request
                .class
                .as_deref()
                .is_some_and(|class| class != "routine")
                || !target_matches
                || stored.body != request.message
                || stored.topic != request.topic
                || stored.state_stamp != request.state_stamp
            {
                return Err(ApiError::conflict(
                    "idempotency key belongs to a different original native mailbox message",
                ));
            }
            let through = {
                let connection = app.mailbox_store.connection().map_err(mailbox_api_error)?;
                mailbox_processed_through(&connection, &stored.mailbox_id, &native)
                    .map_err(mailbox_api_error)?
            };
            let disposition = match stored.dispatch_outcome {
                Some(outcome) => outcome,
                None => app
                    .mailbox_store
                    .wait_for_final_dispatch_outcome(&stored.message_id)
                    .map_err(mailbox_api_error)?,
            };
            let inner = app.inner.lock().expect("state mutex poisoned");
            if federation_mailbox_root_locked(&inner, &native)?.federation_native_creation_id
                != creation
            {
                return Err(ApiError::conflict(
                    "native mailbox sender changed during retry",
                ));
            }
            Ok(Some(MailboxAppendReceipt {
                notification_cause: None,
                mailbox_id: stored.mailbox_id,
                message_id: stored.message_id,
                sequence: stored.sequence,
                unread_depth: stored.unread_depth_at_append,
                notification_disposition: disposition,
                duplicate: true,
                sender_processed_through: through,
                sender_cursor_advanced: false,
                receipt_note: None,
            }))
        })
        .await
    }

    pub(crate) async fn lookup_original_mailbox_resource(
        &self,
        native: &str,
        id: &str,
        message: bool,
    ) -> Result<OriginalMailboxLookup, ApiError> {
        let store = self.mailbox_store.clone();
        let native = native.to_owned();
        let id = id.to_owned();
        run_blocking_api(move || {
            let Some(connection) = store.connection_if_enabled() else {
                return Ok(OriginalMailboxLookup::StoreUnavailable);
            };
            let sql = if message {
                "SELECT EXISTS(SELECT 1 FROM mailbox_participants p JOIN mailbox_messages m ON m.mailbox_id=p.mailbox_id WHERE p.session_id=?1 AND m.id=?2)"
            } else {
                "SELECT EXISTS(SELECT 1 FROM mailbox_participants WHERE session_id=?1 AND mailbox_id=?2)"
            };
            let owns = connection
                .query_row(sql, rusqlite::params![native, id], |row| row.get::<_, bool>(0))
                .map_err(|error| mailbox_api_error(anyhow!(error)))?;
            Ok(if owns {
                OriginalMailboxLookup::Member
            } else {
                OriginalMailboxLookup::NoMembership
            })
        }).await
    }

    pub(crate) fn describe_mailbox_route_miss(
        &self,
        mut error: ApiError,
        operation: &'static str,
        native: &str,
        mailbox: &str,
        lookup: OriginalMailboxLookup,
    ) -> ApiError {
        if error.status != StatusCode::NOT_FOUND || lookup == OriginalMailboxLookup::Member {
            return error;
        }
        // The path is caller input. Retain only generated identifier shapes,
        // and never retain receipts, message data or the domain error body.
        let caller_session_id = native
            .strip_prefix("session-")
            .filter(|suffix| {
                !suffix.is_empty()
                    && suffix.len() <= 20
                    && suffix.bytes().all(|byte| byte.is_ascii_digit())
            })
            .map(|_| native.to_owned());
        let mailbox_id = mailbox
            .strip_prefix("mailbox-")
            .filter(|suffix| suffix.len() == 36 && mailbox_relay_uuid(suffix))
            .map(|_| mailbox.to_owned());
        let coordination_store_id = self.mailbox_store.coordination_store_id.clone();
        let reason = if lookup == OriginalMailboxLookup::StoreUnavailable {
            "the original native mailbox store was unavailable"
        } else {
            "no original native mailbox membership row matched"
        };
        error.message = format!(
            "{}; domain mailbox {operation} returned 404 after {reason} for caller {} and mailbox {} in coordination store {} (server instance {})",
            error.message,
            caller_session_id
                .as_deref()
                .unwrap_or("<invalid session id>"),
            // The caller learns the id it sent, escaped and bounded; the
            // retained record below keeps generated identifier shapes only.
            mailbox_id
                .clone()
                .unwrap_or_else(|| mailbox_id_echo(mailbox)),
            coordination_store_id.as_deref().unwrap_or("unavailable"),
            self.server_instance_id,
        );
        let record = NativeMailboxRouteMiss {
            at: stamp_now(),
            operation,
            caller_session_id,
            mailbox_id,
            coordination_store_id,
            native_lookup: lookup,
        };
        let mut history = self
            .mailbox_store
            .native_route_misses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if history.len() == NATIVE_MAILBOX_ROUTE_MISS_LIMIT {
            history.pop_front();
        }
        history.push_back(record);
        error.with_kind(ApiErrorKind::NativeMailboxRouteMiss)
    }
}
