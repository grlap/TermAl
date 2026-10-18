// Owns standalone/domain namespace exclusion and binding history admission.
// Does not migrate data or enroll participants. Split from mailboxes.rs and
// federation_mailbox_bindings.rs to serialize the raw-to-qualified transition.

impl AppState {
    fn standalone_mailbox_creations(&self, sessions: [&str; 2]) -> Result<[String; 2], ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let mut creations = [String::new(), String::new()];
        for (index, session) in sessions.into_iter().enumerate() {
            let record = federation_mailbox_root_locked(&inner, session)?;
            if record.federation_mailbox_binding.is_some() {
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
    fn authorize_standalone_mailbox<'a>(
        &'a self,
        sessions: [&str; 2],
        creations: &[String; 2],
    ) -> Result<StateMutexGuard<'a, StateInner>> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        for (session, creation) in sessions.into_iter().zip(creations) {
            let record = federation_mailbox_root_locked(&inner, session).map_err(|_| {
                mailbox_store_error(
                    MailboxStoreErrorKind::Conflict,
                    "standalone mailbox participant changed; retry admission",
                )
            })?;
            if record.federation_mailbox_binding.is_some()
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
                .is_some_and(|index| inner.sessions[index].federation_mailbox_binding.is_some())
        };
        if bound {
            self.federation_mailbox_actor(session)
        } else {
            Ok(None)
        }
    }
}

impl MailboxStore {
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

fn refuse_standalone_mailbox_history(history: &[String]) -> Result<(), ApiError> {
    if history.is_empty() {
        return Ok(());
    }
    Err(ApiError::conflict(format!(
        "cannot bind session with standalone history in {} mailbox(es): {}; use a fresh session; existing mail remains readable and acknowledgeable locally",
        history.len(), history.join(", "),
    )))
}
