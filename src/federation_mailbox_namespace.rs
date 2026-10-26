// Owns native/domain namespace selection and binding history admission.
// Does not migrate data or enroll participants. Split from mailboxes.rs and
// federation_mailbox_bindings.rs to serialize the raw-to-qualified transition.

#[derive(Clone, Copy)]
enum NativeMailboxNamespace {
    Standalone,
    Local,
}

fn require_domain_mailbox_target(target: &str) -> Result<(), ApiError> {
    if target.starts_with("session-") {
        return Err(ApiError::not_found(
            "bare native ids select local sends; this domain-only endpoint requires a qualified participant id or domain alias"));
    }
    Ok(())
}

impl AppState {
    fn is_local_native_mailbox_target(&self, target: &str) -> Result<bool, ApiError> {
        if !target.starts_with("session-") { return Ok(false); }
        let inner = self.inner.lock().expect("state mutex poisoned");
        federation_mailbox_root_locked(&inner, target)?;
        Ok(true)
    }

    fn native_mailbox_creations(&self, sessions: [&str; 2], namespace: NativeMailboxNamespace)
        -> Result<[String; 2], ApiError>
    {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let mut creations = [String::new(), String::new()];
        for (index, session) in sessions.into_iter().enumerate() {
            let record = federation_mailbox_root_locked(&inner, session)?;
            if matches!(namespace, NativeMailboxNamespace::Standalone) && record.mailbox_binding().is_some() {
                return Err(ApiError::conflict(
                    "standalone mailbox participants must both be unbound; target or sender participates in the central domain; use an explicitly bound sender and qualified id",
                ));
            }
            creations[index] = record.federation_native_creation_id.clone();
        }
        Ok(creations)
    }

    /// Called after writer admission and SQLite preparation. The returned
    /// state guard fences both exact native creations through append commit.
    fn authorize_native_mailbox<'a>(&'a self, sessions: [&str; 2], creations: &[String; 2],
        namespace: NativeMailboxNamespace) -> Result<StateMutexGuard<'a, StateInner>>
    {
        let inner = self.inner.lock().expect("state mutex poisoned");
        for (session, creation) in sessions.into_iter().zip(creations) {
            let record = federation_mailbox_root_locked(&inner, session).map_err(|_| {
                mailbox_store_error(
                    MailboxStoreErrorKind::Conflict,
                    "standalone mailbox participant changed; retry admission",
                )
            })?;
            if (matches!(namespace, NativeMailboxNamespace::Standalone) && record.mailbox_binding().is_some())
                || record.federation_native_creation_id != *creation
            {
                return Err(mailbox_store_error(MailboxStoreErrorKind::Conflict,
                    "standalone mailbox participant changed namespace or native creation; retry admission"));
            }
        }
        Ok(inner)
    }

    // Legacy routes retain their original unbound validation and error text.
    // Protocol and domain routes still require a valid native root explicitly.
    fn federation_mailbox_actor_if_bound(
        &self,
        session: &str,
    ) -> Result<Option<FederationMailboxActor>, ApiError> {
        let bound = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_session_index(session)
                .is_some_and(|index| inner.sessions[index].mailbox_binding().is_some())
        };
        if bound {
            self.federation_mailbox_actor(session)
        } else {
            Ok(None)
        }
    }
}

impl MailboxStore {
    fn native_sender_has_domain_intent(&self, session: &str, creation: &str, key: &str) -> Result<bool> {
        let Some(connection) = self.connection_if_enabled() else { return Ok(false); };
        native_sender_has_domain_intent(&connection, session, creation, key)
    }

    /// The caller holds this store's writer admission through the binding
    /// installation. No SQLite connection remains locked while taking state.
    fn standalone_mailbox_history(&self, session: &str) -> Result<Vec<String>> {
        let Some(connection) = self.connection_if_enabled() else {
            return Ok(Vec::new());
        };
        let mut statement = connection.prepare(
            "SELECT mailbox_id FROM mailbox_participants WHERE session_id=?1 ORDER BY mailbox_id",
        )?;
        let history = statement
            .query_map([session], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(history)
    }
}

fn native_sender_has_domain_intent(connection: &rusqlite::Connection,
    session: &str, creation: &str, key: &str) -> Result<bool>
{
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM federation_mailbox_intents
         WHERE owner_session_id=?1 AND owner_creation_id=?2 AND idempotency_key=?3)",
        rusqlite::params![session, creation, key], |row| row.get(0))?)
}

fn refuse_standalone_mailbox_history(history: &[String]) -> Result<(), ApiError> {
    if history.is_empty() {
        return Ok(());
    }
    Err(ApiError::conflict(format!(
        "cannot bind session with standalone history in {} mailbox(es): {}; use a fresh session; existing mail remains readable and acknowledgeable locally",
        history.len(), history.join(", "),
    )))
}
