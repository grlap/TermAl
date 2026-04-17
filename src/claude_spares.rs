// Hidden Claude spare pre-warming.
//
// The `claude` CLI has a noticeable cold-start cost — spawning the
// subprocess, loading the JS runtime, completing the protocol
// handshake, and reaching an idle "ready for the first prompt" state
// takes roughly one to two seconds on a warm machine. That latency
// would otherwise sit in front of the user's very first turn in every
// new Claude session. To hide it, TermAl pre-spawns a Claude subprocess
// into an idle "hidden spare" slot keyed by the session's dimensions
// (workdir, project, model, approval mode, effort). When the user
// actually creates a session whose dimensions match a ready spare,
// `StateInner::create_session` promotes the spare in place — the
// warmed process becomes the real session's runtime and the first
// prompt flies the moment it arrives. See `session_crud.rs` for the
// promotion path (`find_matching_hidden_claude_spare` +
// `reset_hidden_claude_spare_record`).
//
// `seed_hidden_claude_spares` is the startup primer: on every load it
// walks the persisted session list and ensures each visible Claude
// session has a matching hidden spare ready for the next create.
// `try_start_hidden_claude_spare` is the per-spare actuator: it spawns
// the `claude` subprocess for one hidden record and stashes the handle
// as `SessionRuntime::Claude(handle)` on that record so it survives
// under `StateInner` until promotion time.
//
// Why Claude-only: Codex sessions share one long-lived app-server
// process (see `shared_codex_mgr.rs`), so there is no per-session
// handshake to amortize. ACP agents (Gemini, Cursor) negotiate over
// a lightweight stdio protocol where handshake latency is dominated
// by the agent binary's own startup rather than anything TermAl can
// prewarm usefully — the extra slot + lifecycle bookkeeping would
// cost more than it saved.
//
// Failure mode: spare spawns are best-effort. If the subprocess fails
// to start we log to stderr and bail silently; the next real turn
// simply falls back to the usual cold-start path. The pool never
// surfaces errors to the UI.

impl AppState {
    /// Pre-warms a hidden Claude spare for every already-persisted
    /// Claude session so the user's next create in any of those slots
    /// lands on a ready runtime.
    ///
    /// Called once from `AppState::new` right after sessions are loaded
    /// from SQLite. The work happens in two phases so the state mutex
    /// is never held across a subprocess spawn: first pass, under the
    /// lock, we collect the `(workdir, project, model, approval_mode,
    /// effort)` contexts of every visible, local Claude session and
    /// ask `StateInner::ensure_hidden_claude_spare` to reserve a
    /// hidden record for each; second pass, with the lock released, we
    /// call `try_start_hidden_claude_spare` for every reserved record,
    /// which is the step that actually spawns the `claude` child
    /// process. Holding the state mutex across the subprocess spawn
    /// would stall every other state reader for the full handshake
    /// window.
    fn seed_hidden_claude_spares(&self) {
        let spare_ids = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let contexts = inner
                .sessions
                .iter()
                .filter(|record| {
                    !record.hidden
                        && !record.is_remote_proxy()
                        && record.session.agent == Agent::Claude
                })
                .map(|record| {
                    (
                        record.session.workdir.clone(),
                        record.session.project_id.clone(),
                        record.session.model.clone(),
                        record
                            .session
                            .claude_approval_mode
                            .unwrap_or_else(default_claude_approval_mode),
                        record
                            .session
                            .claude_effort
                            .unwrap_or_else(default_claude_effort),
                    )
                })
                .collect::<Vec<_>>();
            let mut spare_ids = Vec::new();
            for (workdir, project_id, model, approval_mode, effort) in contexts {
                if let Some(session_id) = inner.ensure_hidden_claude_spare(
                    workdir,
                    project_id,
                    model,
                    approval_mode,
                    effort,
                ) {
                    spare_ids.push(session_id);
                }
            }
            spare_ids
        };

        for session_id in spare_ids {
            self.try_start_hidden_claude_spare(&session_id);
        }
    }

    /// Spawns the `claude` subprocess for one hidden spare record and
    /// parks the handle on its `SessionRecord` so a future matching
    /// `create_session` can promote it instead of cold-starting.
    ///
    /// Idempotent and cheap to call speculatively: the guard under the
    /// state lock re-checks that the record still exists, is still
    /// hidden, is a local Claude session, and has no runtime already
    /// attached (`SessionRuntime::None`). If any of those invariants
    /// fail — the spare was promoted to a real session in the
    /// meantime, killed, or already warmed by a concurrent call — the
    /// function returns without spawning, which is what makes "spawn
    /// only if no existing spare" safe to enforce from multiple call
    /// sites (startup seeding, post-create replenishment, etc.).
    ///
    /// `spawn_claude_runtime` runs outside the state lock for the same
    /// latency reason as `seed_hidden_claude_spares`. On success the
    /// function re-acquires the lock, re-validates the record
    /// (another thread may have raced and claimed or killed it while
    /// the child was starting; in that case we kill the process we
    /// just launched and drop it), then installs the handle. Spawn
    /// failures are logged to stderr and swallowed — the spare pool is
    /// a pure latency optimization, so surfacing errors to the user
    /// would be noise; the next real turn for this session will just
    /// pay the normal cold-start cost instead.
    fn try_start_hidden_claude_spare(&self, session_id: &str) {
        let spawn_request = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            if !record.hidden
                || record.is_remote_proxy()
                || record.session.agent != Agent::Claude
                || !matches!(record.runtime, SessionRuntime::None)
            {
                return;
            }

            reset_hidden_claude_spare_record(record);
            Some((
                record.session.id.clone(),
                record.session.workdir.clone(),
                record.session.model.clone(),
                record
                    .session
                    .claude_approval_mode
                    .unwrap_or_else(default_claude_approval_mode),
                record
                    .session
                    .claude_effort
                    .unwrap_or_else(default_claude_effort),
                record.external_session_id.clone(),
            ))
        };

        let Some((session_id, cwd, model, approval_mode, effort, resume_session_id)) =
            spawn_request
        else {
            return;
        };

        let handle = match spawn_claude_runtime(
            self.clone(),
            session_id.clone(),
            cwd,
            model,
            approval_mode,
            effort,
            resume_session_id,
            None,
        ) {
            Ok(handle) => handle,
            Err(err) => {
                eprintln!("claude hidden pool> failed to warm spare `{session_id}`: {err:#}");
                return;
            }
        };

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(&session_id) else {
            let _ = handle.kill();
            return;
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if record.session.agent != Agent::Claude || !matches!(record.runtime, SessionRuntime::None)
        {
            let _ = handle.kill();
            return;
        }
        record.runtime = SessionRuntime::Claude(handle);
    }
}
