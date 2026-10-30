// The original identity of a session_bind sent outside queued admission,
// kept in process memory while its outcome is unknown, and its exact
// recovery on the same connection.
//
// Owns: the retained bind (`EngramRetainedBind`), written under the session's
// bind claim before a non-admission bind is sent; how the answer to that send
// settles it (accepted, proven unsent, or refused by one of Engram's
// definitive no-write bind refusals) or leaves it unresolved (a lost or
// invalid reply, or any other error); the replay of an unresolved one, as the
// identical request under its key, before any other bind of the session goes
// out; and its abandonment, told to the session's holder, when its
// connection or project settings changed or Engram refused the replay
// itself. A refusal of the replay never settles the original: whether Engram
// recorded the original stays unknown, and the holder is told so.
//
// Does not own: building a new bind request or adopting a reply's routing
// token (`bind_engram_target_uncoordinated_off_lock`,
// `engram_host_adapter.rs`); the per-session bind claim
// (`EngramSessionState::bind_in_progress`) that serializes every bind;
// queued admission binds, whose prepared request the queue retains
// (`engram_queued_admission.rs`); the source-root naming bind, whose key is
// derived from its whole intent and which refuses while a retained bind is
// held (`engram_source_roots.rs`); restart-safe custody. The retention is
// process memory only: a host crash or restart loses it, and so does a
// connection or settings change after which it is not replayed.
// New module.

/// Engram's refusals of a session_bind that come before its one storage
/// transaction or roll it back, so the call they answer recorded nothing:
/// an undecodable request, invalid bind fields or a begun turn
/// (`invalid_control_session`), a stale claim fence, a key already bound to
/// another intent, and a superseded control connection. This holds while
/// Engram's handler maps an error raised after the bind commits (its
/// verification-applicability step) to `storage_error`, which settles
/// nothing here; a change that let such a step answer one of these codes, or
/// a certified-not-applied one, would make a committed bind read as unwritten.
const ENGRAM_SESSION_BIND_NO_WRITE_REFUSALS: [&str; 5] = [
    "invalid_request",
    "invalid_control_session",
    "stale_fence",
    "control_session_bind_conflict",
    "control_connection_superseded",
];

/// A non-admission session_bind this process sent whose outcome is not
/// settled. Process memory only (`EngramSessionState::retained_bind`).
#[derive(Clone, Debug)]
struct EngramRetainedBind {
    /// The exact request sent, idempotency key included.
    request: EngramControlRequest,
    /// The connection and project settings it was sent under: it is replayed
    /// only while both are unchanged.
    connection: EngramConnectionConfig,
    project_id: String,
    settings: EngramProjectSettings,
}

impl EngramRetainedBind {
    fn new(target: &EngramBindingTarget, request: &EngramControlRequest) -> Self {
        Self {
            request: request.clone(),
            connection: target.connection.clone(),
            project_id: target.project_id.clone(),
            settings: target.settings.clone(),
        }
    }

    fn key(&self) -> &str {
        engram_bind_request_key(&self.request).unwrap_or("")
    }

    /// Whether it may be replayed for `target`: the same connection, project
    /// and settings it was sent under.
    fn is_replayable_for(&self, target: &EngramBindingTarget) -> bool {
        self.connection == target.connection
            && self.project_id == target.project_id
            && self.settings.same_admission_settings(&target.settings)
    }
}

/// The idempotency key of a session_bind request.
fn engram_bind_request_key(request: &EngramControlRequest) -> Option<&str> {
    match request {
        EngramControlRequest::SessionBind {
            idempotency_key, ..
        } => Some(idempotency_key.as_str()),
        _ => None,
    }
}

/// Whether Engram's answer refused this session_bind call with a refusal that
/// records nothing (`ENGRAM_SESSION_BIND_NO_WRITE_REFUSALS`).
fn engram_bind_refused_without_write(error: &EngramTransportError) -> bool {
    error.kind == EngramTransportErrorKind::Remote
        && error
            .code
            .as_deref()
            .is_some_and(|code| ENGRAM_SESSION_BIND_NO_WRITE_REFUSALS.contains(&code))
}

/// Whether `error`, answering the one send of a retained bind's original,
/// proves that send recorded nothing: it never left this host, Engram
/// certified it unapplied, or Engram refused it before or within a rolled
/// back transaction. A lost or invalid reply, a deadline, a cancellation or
/// any other error proves nothing and leaves the original unresolved.
fn engram_bind_send_proven_unapplied(error: &EngramTransportError) -> bool {
    error.request_never_sent
        || error.stopped_before_transport()
        || error.invocation_application() == EngramInvocationApplication::CertifiedNotApplied
        || engram_bind_refused_without_write(error)
}

/// `binding` if it is the answer to `session_id`'s bind: a routing token, a
/// phase, and a status that names this session. Engram names the session in
/// every status, so one that names none is malformed and fails closed, as the
/// containment settlement requires. Any other answer is invalid: it settles
/// nothing and its token is never used. Received values are withheld from
/// the error; only their location is named.
fn engram_validated_bind_answer(
    binding: EngramSessionBindingResponse,
    session_id: &str,
) -> std::result::Result<EngramSessionBindingResponse, EngramTransportError> {
    let invalid = |path: &str| -> std::result::Result<
        EngramSessionBindingResponse,
        EngramTransportError,
    > {
        Err(EngramTransportError::protocol(format!(
            "Engram's bind answer is not this session's binding: field path={path} \
             (value withheld)"
        )))
    };
    if binding.routing_token.trim().is_empty() {
        return invalid("$.routing_token");
    }
    if binding.status.phase.trim().is_empty() {
        return invalid("$.status.phase");
    }
    if binding.status.session_id.as_deref() != Some(session_id) {
        return invalid("$.status.session_id");
    }
    Ok(binding)
}

/// Whether a new bind may replace the session's recovered original: only
/// while the current status it was answered with holds no turn. A session in
/// a turn is not rebound under it; the turn is settled first.
fn engram_recovered_bind_may_be_replaced(
    recovery: &EngramBindRecovery,
) -> std::result::Result<(), EngramTransportError> {
    if engram_phase_holds_no_turn(&recovery.binding.status.phase) {
        return Ok(());
    }
    Err(EngramTransportError::local_state(
        "The recovered Engram bind reports a session in a turn: no new bind replaces it \
         until that turn is settled",
    ))
}

/// The line its holder is told when a retained bind is given up unresolved.
fn engram_retained_bind_abandoned_line(retained: &EngramRetainedBind, why: &str) -> String {
    format!(
        "[TermAl] Engram session bind: an earlier bind of this session (key {}) lost its \
         answer, so whether Engram recorded it is unknown; it is not sent again because \
         {why}. A new bind replaces it.",
        retained.key()
    )
}

/// The recovered original: Engram's answer to its exact replay, with the
/// session's current status, and the root guard taken for it.
struct EngramBindRecovery {
    binding: EngramSessionBindingResponse,
    request: EngramControlRequest,
    root_guard: Option<EngramAcknowledgedRootGuard>,
}

impl AppState {
    /// Retains `request`, about to be sent for `target` outside queued
    /// admission, as the session's original until its answer settles it.
    /// The caller holds the session's bind claim.
    fn retain_engram_bind(
        &self,
        target: &EngramBindingTarget,
        request: &EngramControlRequest,
    ) -> std::result::Result<(), EngramTransportError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(&target.connection.session_id) else {
            return Err(EngramTransportError::local_state(
                "session disappeared before its Engram bind was sent",
            ));
        };
        inner.sessions[index].engram.retained_bind =
            Some(EngramRetainedBind::new(target, request));
        Ok(())
    }

    /// Settles the retained original `request` by the answer to its one send:
    /// gone once accepted or proven unapplied, kept unresolved otherwise.
    fn settle_retained_engram_bind<T>(
        &self,
        session_id: &str,
        request: &EngramControlRequest,
        answer: &std::result::Result<T, EngramTransportError>,
    ) {
        let settled = match answer {
            Ok(_) => true,
            Err(error) => engram_bind_send_proven_unapplied(error),
        };
        if settled {
            self.release_retained_engram_bind(session_id, request);
        } else if let Err(error) = answer {
            eprintln!(
                "engram> session={session_id} bind {} unresolved ({error}); it is replayed \
                 identically before the session is bound again",
                engram_bind_request_key(request).unwrap_or("")
            );
        }
    }

    /// Drops the retained bind if it is still `request`'s.
    fn release_retained_engram_bind(&self, session_id: &str, request: &EngramControlRequest) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let engram = &mut inner.sessions[index].engram;
        let key = engram_bind_request_key(request).unwrap_or("");
        if engram
            .retained_bind
            .as_ref()
            .is_some_and(|retained| retained.key() == key)
        {
            engram.retained_bind = None;
        }
    }

    /// Gives up the retained bind if it is still `request`'s, unresolved, and
    /// tells the session's holder why.
    fn abandon_retained_engram_bind(
        &self,
        session_id: &str,
        request: &EngramControlRequest,
        why: &str,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let engram = &mut inner.sessions[index].engram;
        let Some(retained) = engram.retained_bind.take_if(|retained| {
            retained.key() == engram_bind_request_key(request).unwrap_or("")
        }) else {
            return;
        };
        let line = engram_retained_bind_abandoned_line(&retained, why);
        eprintln!("engram> session={session_id} {line}");
        engram.set_pending_source_root_line(line);
    }

    /// Recovers the session's retained original before any other bind of it
    /// is sent. An original retained under another connection or other
    /// settings is given up, told, and never replayed. Otherwise its exact
    /// request is sent again under its key: a valid answer
    /// (`engram_validated_bind_answer`) settles it and is returned with the
    /// session's current status; an invalid or lost answer, or any other that
    /// settles nothing, keeps it for the next bind, and this bind fails; a definitive
    /// refusal of the replay gives it up, told, still unresolved. Returns
    /// `None` when there is nothing left to recover before a new bind.
    fn recover_retained_engram_bind(
        &self,
        target: &EngramBindingTarget,
        deadline: std::time::Instant,
        owner: Option<&EngramQueuedAdmissionOwner>,
    ) -> std::result::Result<Option<EngramBindRecovery>, EngramTransportError> {
        let session_id = target.connection.session_id.as_str();
        let retained = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return Ok(None);
            };
            inner.sessions[index].engram.retained_bind.clone()
        };
        let Some(retained) = retained else {
            return Ok(None);
        };
        if !retained.is_replayable_for(target) {
            self.abandon_retained_engram_bind(
                session_id,
                &retained.request,
                "its control connection or the project's Engram settings changed since it was sent",
            );
            return Ok(None);
        }
        if let Some(owner) = owner {
            self.require_queued_engram_owner(session_id, owner, "Engram bind recovery")?;
        }
        let request = retained.request;
        let root_guard = self.guard_engram_root_read_until(
            target,
            engram_retained_work_binding(&request),
            deadline,
        )?;
        let answer = target
            .adapter
            .request(&target.connection, &request, target.rpc_timeout_until(deadline)?)
            .and_then(parse_engram_result::<EngramSessionBindingResponse>)
            .and_then(|binding| engram_validated_bind_answer(binding, session_id))
            .map_err(|mut error| {
                error.causal_failure = Some(EngramCausalFailure::request(&error, &request));
                error
            });
        match answer {
            Ok(binding) => {
                self.release_retained_engram_bind(session_id, &request);
                Ok(Some(EngramBindRecovery {
                    binding,
                    request,
                    root_guard,
                }))
            }
            Err(error) if engram_bind_refused_without_write(&error) => {
                if engram_error_is_stale_fence(&error) {
                    self.note_engram_refused_work_binding(
                        session_id,
                        engram_retained_work_binding(&request),
                    );
                }
                self.abandon_retained_engram_bind(
                    session_id,
                    &request,
                    &format!(
                        "Engram refused its replay ({}), which says nothing of the earlier send",
                        error.code.as_deref().unwrap_or("refused")
                    ),
                );
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
}

/// The work binding a session_bind request was sent with.
fn engram_retained_work_binding(
    request: &EngramControlRequest,
) -> Option<&EngramControlWorkBinding> {
    match request {
        EngramControlRequest::SessionBind { work_binding, .. } => work_binding.as_ref(),
        _ => None,
    }
}
