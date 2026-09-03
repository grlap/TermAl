// Remote delta localization and application.
//
// Extracted from remote_routes.rs so route resolution/proxying stays
// separate from the high-volume state-delta apply machinery. This file is
// included into the crate root and therefore shares the surrounding types.
//
// The event match intentionally keeps per-variant mutation, persistence, and
// publish ordering explicit. Those variants differ in hydration, idempotency,
// and narrow-delta semantics; hiding them behind a generic closure would make
// the authority and durability audit harder without crossing the Rust module
// size threshold.

impl AppState {
    /// Commits state localized from a remote response while preserving a
    /// full-snapshot retry obligation if the synchronous persistence fallback
    /// fails after mutating memory or advancing remote recovery watermarks.
    fn commit_remote_localization_locked(&self, inner: &mut StateInner) -> Result<u64> {
        match self.commit_locked(inner) {
            Ok(revision) => Ok(revision),
            Err(error) => {
                inner.remote_delta_persist_dirty = true;
                Err(error)
            }
        }
    }

    /// Session-create counterpart to [`Self::commit_remote_localization_locked`].
    /// A failed synchronous create persist leaves the proxy authoritative in
    /// memory, so later remote recovery must retry the full snapshot before
    /// treating an equal response or delta as a no-op.
    fn commit_remote_session_created_locked(
        &self,
        inner: &mut StateInner,
        record: &SessionRecord,
    ) -> Result<u64> {
        let publish_recovered_snapshot = inner.remote_delta_persist_dirty;
        match self.commit_session_created_locked(inner, record) {
            Ok(revision) => {
                inner.remote_delta_persist_dirty = false;
                // The synchronous create fallback persists the full current
                // snapshot. If that write also discharged an earlier remote
                // localization debt, publish the recovered snapshot before
                // the caller emits the narrow SessionCreated delta so peers
                // observe every mutation the successful write made durable.
                if publish_recovered_snapshot {
                    self.publish_state_locked(inner);
                }
                Ok(revision)
            }
            Err(error) => {
                inner.remote_delta_persist_dirty = true;
                Err(error)
            }
        }
    }

    /// Commits a state-bearing remote delta while remembering a failed
    /// synchronous persistence attempt. The mutation may already be durable
    /// when a post-commit integrity check fails, so rolling memory back would
    /// risk inverting memory and SQLite; a full-snapshot retry is safe in both
    /// the pre-commit and post-commit failure cases.
    fn commit_remote_delta_persisted_locked(&self, inner: &mut StateInner) -> Result<u64> {
        match self.commit_persisted_delta_locked(inner) {
            Ok(revision) => {
                inner.remote_delta_persist_dirty = false;
                Ok(revision)
            }
            Err(error) => {
                inner.remote_delta_persist_dirty = true;
                Err(error)
            }
        }
    }

    /// Commits a narrow streaming delta while preserving recovery state if the
    /// post-shutdown synchronous full-snapshot fallback fails. These mutations
    /// are already authoritative in memory. Mark the remote revision applied
    /// on failure as well as arming persistence debt; the next remote frame or
    /// recovery snapshot first persists and publishes the current full state
    /// before the watermark can skip replay.
    fn commit_remote_streaming_delta_locked(
        &self,
        inner: &mut StateInner,
        remote_id: &str,
        remote_revision: u64,
        replay_key: &Option<RemoteDeltaReplayKey>,
    ) -> Result<u64> {
        match self.commit_delta_locked(inner) {
            Ok(revision) => Ok(revision),
            Err(error) => {
                inner.remote_delta_persist_dirty = true;
                inner.note_remote_applied_revision(remote_id, remote_revision);
                self.note_remote_applied_delta_replay(replay_key);
                Err(error)
            }
        }
    }

    /// Retries a failed state-bearing remote-delta persist before an exact
    /// replay is accepted as a semantic no-op. Re-publish the current snapshot
    /// at the revision already allocated by the failed apply so clients also
    /// observe the mutation whose narrow delta was never emitted.
    fn retry_remote_delta_persist_if_dirty_locked(&self, inner: &mut StateInner) -> Result<()> {
        if !inner.remote_delta_persist_dirty {
            return Ok(());
        }
        self.persist_internal_locked(inner)?;
        inner.remote_delta_persist_dirty = false;
        self.publish_state_locked(inner);
        Ok(())
    }

    /// Applies a single `DeltaEvent` from a remote's SSE stream to local
    /// state and re-publishes it under the matching local session /
    /// orchestrator ids. Remote ids in the payload (session_id,
    /// project_id, orchestrator_id) are remapped to their local proxy
    /// counterparts before publish. Errors here cause
    /// `dispatch_remote_event` (src/remote_sync.rs) to fall back to
    /// `resync_remote_state_snapshot_with_authority`.
    fn apply_remote_delta_event(
        &self,
        remote_id: &str,
        event: DeltaEvent,
    ) -> Result<(), anyhow::Error> {
        self.apply_remote_delta_event_with_expected_route(remote_id, None, None, None, event)
    }

    /// Applies a delta returned by a request while retaining the exact lease.
    /// Marker responses use this path so an A -> B -> A settings cycle cannot
    /// make a retired pre-cycle response current again by routing-byte equality.
    fn apply_remote_delta_event_for_request(
        &self,
        lease: &RemoteRequestLease,
        event: DeltaEvent,
    ) -> Result<(), anyhow::Error> {
        self.apply_remote_delta_event_with_expected_route(
            &lease.pinned.id,
            Some(&lease.pinned),
            Some(&lease.connection),
            Some(lease.state_continuity_generation),
            event,
        )
    }

    fn apply_remote_delta_event_for_bridge(
        &self,
        expected_remote: &RemoteConfig,
        expected_connection: &RemoteConnection,
        event: DeltaEvent,
    ) -> Result<(), anyhow::Error> {
        self.apply_remote_delta_event_with_expected_route(
            &expected_remote.id,
            Some(expected_remote),
            Some(expected_connection),
            None,
            event,
        )
    }

    fn apply_remote_delta_event_with_expected_route(
        &self,
        remote_id: &str,
        expected_remote: Option<&RemoteConfig>,
        expected_connection: Option<&RemoteConnection>,
        expected_state_continuity_generation: Option<u64>,
        event: DeltaEvent,
    ) -> Result<(), anyhow::Error> {
        let ensure_expected_route = |inner: &StateInner| -> Result<(), anyhow::Error> {
            if let Some(expected_remote) = expected_remote {
                self.ensure_remote_apply_authority_locked(
                    inner,
                    expected_remote,
                    expected_connection,
                )
                .map_err(|err| anyhow::Error::new(RemoteAuthorityApplyError(err)))?;
            }
            if let (Some(connection), Some(generation)) = (
                expected_connection,
                expected_state_continuity_generation,
            ) {
                connection
                    .ensure_state_continuity_generation(generation)
                    .map_err(|err| anyhow::Error::new(RemoteAuthorityApplyError(err)))?;
            }
            Ok(())
        };
        let remote_revision = delta_event_revision(&event);
        let authority_generation = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            ensure_expected_route(&inner)?;
            // Persistence debt belongs to the already-authoritative in-memory
            // state, so discharge it before any revision or replay shortcut can
            // accept this remote frame as a successful no-op.
            self.retry_remote_delta_persist_if_dirty_locked(&mut inner)?;
            if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                return Ok(());
            }
            expected_connection.map_or_else(
                || self.remote_registry.config_generation.load(Ordering::Acquire),
                |connection| connection.authority_generation,
            )
        };
        let remote_delta_replay_key = Self::remote_delta_replay_key_for_generation(
            remote_id,
            authority_generation,
            &event,
        );
        if self.should_skip_remote_applied_delta_replay(&remote_delta_replay_key) {
            return Ok(());
        }
        match event {
            DeltaEvent::SessionCreated {
                session,
                session_id,
                ..
            } => {
                if session.id != session_id {
                    return Err(anyhow!(
                        "remote created session payload id `{}` did not match event id `{session_id}`",
                        session.id
                    ));
                }
                let Some((published_session_id, delta_session, revision)) = ({
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    ensure_expected_route(&inner)?;
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    let local_project_ids_by_remote_project_id =
                        remote_project_id_map(&inner, remote_id);
                    let local_project_id = local_project_id_for_remote_project(
                        &local_project_ids_by_remote_project_id,
                        session.project_id.as_deref(),
                    );
                    let (local_session_id, changed) = ensure_remote_proxy_session_record(
                        &mut inner,
                        remote_id,
                        &session,
                        local_project_id.map(LocalProjectId::into_inner),
                        // Keep exact redelivery state-bearing: if a prior
                        // SessionCreated persist failed after mutating memory,
                        // replay rewrites and commits the record again instead
                        // of taking the no-op branch below.
                        true,
                    );
                    let local_index =
                        inner.find_session_index(&local_session_id).ok_or_else(|| {
                            anyhow!("local proxy session `{local_session_id}` not found")
                        })?;
                    if !changed {
                        self.retry_remote_delta_persist_if_dirty_locked(&mut inner)?;
                        inner.note_remote_applied_revision(remote_id, remote_revision);
                        None
                    } else {
                        let local_record =
                            inner.sessions.get(local_index).cloned().ok_or_else(|| {
                                anyhow!("local proxy session `{local_session_id}` not found")
                            })?;
                        let revision = self
                            .commit_remote_session_created_locked(&mut inner, &local_record)?;
                        let local_record = inner.sessions.get(local_index).ok_or_else(|| {
                            anyhow!("local proxy session `{local_session_id}` not found")
                        })?;
                        let delta_session =
                            AppState::wire_session_summary_from_record(local_record);
                        let published_session_id = delta_session.id.clone();
                        inner.note_remote_applied_revision(remote_id, remote_revision);
                        Some((published_session_id, delta_session, revision))
                    }
                }) else {
                    self.note_remote_applied_delta_replay(&remote_delta_replay_key);
                    return Ok(());
                };
                self.publish_delta(&DeltaEvent::SessionCreated {
                    revision,
                    session_id: published_session_id,
                    session: delta_session,
                });
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::MessageCreated {
                message,
                message_count: remote_message_count,
                message_id,
                message_index,
                preview,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                status,
                ..
            } => {
                if message.id() != message_id {
                    return Err(anyhow!(
                        "remote created message payload id `{}` did not match event id `{message_id}`",
                        message.id()
                    ));
                }
                if message_index >= usize::try_from(remote_message_count).unwrap_or(usize::MAX) {
                    return Err(anyhow!(
                        "remote MessageCreated index `{message_index}` is outside messageCount `{remote_message_count}` for session `{session_id}`"
                    ));
                }
                let hydration_outcome = self.hydrate_unloaded_remote_session_for_delta(
                    remote_id,
                    &session_id,
                    authority_generation,
                    remote_revision,
                    remote_message_count,
                    remote_session_mutation_stamp,
                    expected_remote,
                    expected_connection,
                    expected_state_continuity_generation,
                )?;
                if self.should_skip_delta_after_remote_hydration(
                    hydration_outcome,
                    &remote_delta_replay_key,
                ) {
                    return Ok(());
                }
                let (
                    local_session_id,
                    applied_message_index,
                    revision,
                    message_count,
                    session_mutation_stamp,
                ) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    ensure_expected_route(&inner)?;
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    self.retry_remote_delta_persist_if_dirty_locked(&mut inner)?;
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (
                        local_session_id,
                        applied_message_index,
                        message_count,
                        session_mutation_stamp,
                    ) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let applied_message_index = if let Some(existing_index) =
                            message_index_on_record(record, &message_id)
                        {
                            let local_message_index = message_index
                                .checked_sub(record.message_start_index)
                                .ok_or_else(|| {
                                    anyhow!(
                                        "remote MessageCreated index `{message_index}` predates the retained transcript window for existing message `{message_id}` in session `{session_id}`"
                                    )
                                })?;
                            let max_index_after_removal =
                                record.session.messages.len().saturating_sub(1);
                            if local_message_index > max_index_after_removal {
                                return Err(anyhow!(
                                    "remote MessageCreated index `{message_index}` is out of bounds for existing message `{message_id}` in session `{session_id}`"
                                ));
                            }
                            record.session.messages.remove(existing_index);
                            record
                                .session
                                .messages
                                .insert(local_message_index, message.clone());
                            record.message_positions =
                                build_message_positions(&record.session.messages);
                            message_index
                        } else {
                            if record.session.messages.is_empty() {
                                record.message_start_index = message_index;
                            }
                            let local_message_index = message_index
                                .checked_sub(record.message_start_index)
                                .ok_or_else(|| {
                                    anyhow!(
                                        "remote MessageCreated index `{message_index}` predates the retained transcript window in session `{session_id}`"
                                    )
                                })?;
                            if local_message_index > record.session.messages.len() {
                                return Err(anyhow!(
                                    "remote MessageCreated index `{message_index}` leaves a gap in session `{session_id}`"
                                ));
                            }
                            insert_message_on_record(
                                record,
                                local_message_index,
                                message.clone(),
                            );
                            message_index
                        };
                        record.session.preview = preview.clone();
                        record.session.status = status;
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        (
                            record.session.id.clone(),
                            applied_message_index,
                            session_message_count(record),
                            record.mutation_stamp,
                        )
                    };
                    let revision = self.commit_remote_delta_persisted_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (
                        local_session_id,
                        applied_message_index,
                        revision,
                        message_count,
                        session_mutation_stamp,
                    )
                };
                self.publish_delta(&DeltaEvent::MessageCreated {
                    revision,
                    session_id: local_session_id,
                    message_id,
                    message_index: applied_message_index,
                    message_count,
                    message,
                    preview,
                    status,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::MessageUpdated {
                message,
                message_count: remote_message_count,
                message_id,
                message_index: _,
                preview,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                status,
                ..
            } => {
                {
                    let inner = self.inner.lock().expect("state mutex poisoned");
                    ensure_expected_route(&inner)?;
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                }
                if message.id() != message_id {
                    return Err(anyhow!(
                        "remote updated message payload id `{}` did not match event id `{message_id}`",
                        message.id()
                    ));
                }
                let hydration_outcome = self.hydrate_unloaded_remote_session_for_delta(
                    remote_id,
                    &session_id,
                    authority_generation,
                    remote_revision,
                    remote_message_count,
                    remote_session_mutation_stamp,
                    expected_remote,
                    expected_connection,
                    expected_state_continuity_generation,
                )?;
                if self.should_skip_delta_after_remote_hydration(
                    hydration_outcome,
                    &remote_delta_replay_key,
                ) {
                    return Ok(());
                }
                let (
                    local_session_id,
                    applied_message_index,
                    message_count,
                    revision,
                    session_mutation_stamp,
                ) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    ensure_expected_route(&inner)?;
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    self.retry_remote_delta_persist_if_dirty_locked(&mut inner)?;
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (
                        local_session_id,
                        applied_message_index,
                        message_count,
                        session_mutation_stamp,
                    ) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let Some(local_message_index) =
                            message_index_on_record(record, &message_id)
                        else {
                            return Err(anyhow!(
                                "remote MessageUpdated for unknown message `{message_id}` in session `{session_id}`"
                            ));
                        };
                        let existing_message = record
                            .session
                            .messages
                            .get_mut(local_message_index)
                            .expect("message_index_on_record returned an out-of-bounds index");
                        *existing_message = message.clone();
                        record.session.preview = preview.clone();
                        record.session.status = status;
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        (
                            record.session.id.clone(),
                            global_message_index(record, local_message_index),
                            session_message_count(record),
                            record.mutation_stamp,
                        )
                    };
                    let revision = self.commit_remote_delta_persisted_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (
                        local_session_id,
                        applied_message_index,
                        message_count,
                        revision,
                        session_mutation_stamp,
                    )
                };
                self.publish_delta(&DeltaEvent::MessageUpdated {
                    revision,
                    session_id: local_session_id,
                    message_id,
                    message_index: applied_message_index,
                    message_count,
                    message,
                    preview,
                    status,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::TextDelta {
                delta,
                message_count: remote_message_count,
                message_id,
                preview,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                text_start_byte: remote_text_start_byte,
                ..
            } => {
                let hydration_outcome = self.hydrate_unloaded_remote_session_for_delta(
                    remote_id,
                    &session_id,
                    authority_generation,
                    remote_revision,
                    remote_message_count,
                    remote_session_mutation_stamp,
                    expected_remote,
                    expected_connection,
                    expected_state_continuity_generation,
                )?;
                if self.should_skip_delta_after_remote_hydration(
                    hydration_outcome,
                    &remote_delta_replay_key,
                ) {
                    return Ok(());
                }
                let (
                    local_session_id,
                    message_index,
                    message_count,
                    revision,
                    text_start_byte,
                    session_mutation_stamp,
                ) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    ensure_expected_route(&inner)?;
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    self.retry_remote_delta_persist_if_dirty_locked(&mut inner)?;
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let record = inner
                        .session_by_index(index)
                        .expect("remote session index should be valid");
                    let message = record
                        .session
                        .messages
                        .iter()
                        .find(|message| message.id() == message_id)
                        .ok_or_else(|| anyhow!("remote message `{message_id}` not found"))?;
                    let actual = match message {
                        Message::Text { text, .. } => text.len(),
                        _ => {
                            return Err(anyhow!(
                                "remote message `{message_id}` is not a text message"
                            ));
                        }
                    };
                    if remote_text_start_byte != actual {
                        return Err(anyhow!(
                            "remote text delta for message `{message_id}` starts at byte {remote_text_start_byte} but the local mirror is at byte {actual}"
                        ));
                    }
                    let (
                        local_session_id,
                        message_index,
                        message_count,
                        text_start_byte,
                        session_mutation_stamp,
                    ) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let local_message_index = message_index_on_record(record, &message_id)
                            .ok_or_else(|| anyhow!("remote message `{message_id}` not found"))?;
                        let Some(message) = record.session.messages.get_mut(local_message_index)
                        else {
                            return Err(anyhow!(
                                "remote message index `{local_message_index}` is out of bounds"
                            ));
                        };
                        let text_start_byte = match message {
                            Message::Text { text, .. } => {
                                let text_start_byte = text.len();
                                text.push_str(&delta);
                                text_start_byte
                            }
                            _ => {
                                return Err(anyhow!(
                                    "remote message `{message_id}` is not a text message"
                                ));
                            }
                        };
                        if let Some(next_preview) = preview.as_ref() {
                            record.session.preview = next_preview.clone();
                        }
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        (
                            record.session.id.clone(),
                            global_message_index(record, local_message_index),
                            session_message_count(record),
                            text_start_byte,
                            record.mutation_stamp,
                        )
                    };
                    let revision = self.commit_remote_streaming_delta_locked(
                        &mut inner,
                        remote_id,
                        remote_revision,
                        &remote_delta_replay_key,
                    )?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (
                        local_session_id,
                        message_index,
                        message_count,
                        revision,
                        text_start_byte,
                        session_mutation_stamp,
                    )
                };
                self.publish_delta(&DeltaEvent::TextDelta {
                    revision,
                    session_id: local_session_id,
                    message_id,
                    message_index,
                    message_count,
                    text_start_byte,
                    delta,
                    preview,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::TextReplace {
                message_count: remote_message_count,
                message_id,
                preview,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                text,
                ..
            } => {
                let hydration_outcome = self.hydrate_unloaded_remote_session_for_delta(
                    remote_id,
                    &session_id,
                    authority_generation,
                    remote_revision,
                    remote_message_count,
                    remote_session_mutation_stamp,
                    expected_remote,
                    expected_connection,
                    expected_state_continuity_generation,
                )?;
                if self.should_skip_delta_after_remote_hydration(
                    hydration_outcome,
                    &remote_delta_replay_key,
                ) {
                    return Ok(());
                }
                let (
                    local_session_id,
                    message_index,
                    message_count,
                    revision,
                    session_mutation_stamp,
                ) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    ensure_expected_route(&inner)?;
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    self.retry_remote_delta_persist_if_dirty_locked(&mut inner)?;
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (local_session_id, message_index, message_count, session_mutation_stamp) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let local_message_index = message_index_on_record(record, &message_id)
                            .ok_or_else(|| anyhow!("remote message `{message_id}` not found"))?;
                        let Some(message) = record.session.messages.get_mut(local_message_index)
                        else {
                            return Err(anyhow!(
                                "remote message index `{local_message_index}` is out of bounds"
                            ));
                        };
                        match message {
                            Message::Text {
                                text: current_text, ..
                            } => {
                                current_text.clear();
                                current_text.push_str(&text);
                            }
                            _ => {
                                return Err(anyhow!(
                                    "remote message `{message_id}` is not a text message"
                                ));
                            }
                        }
                        if let Some(next_preview) = preview.as_ref() {
                            record.session.preview = next_preview.clone();
                        }
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        (
                            record.session.id.clone(),
                            global_message_index(record, local_message_index),
                            session_message_count(record),
                            record.mutation_stamp,
                        )
                    };
                    let revision = self.commit_remote_streaming_delta_locked(
                        &mut inner,
                        remote_id,
                        remote_revision,
                        &remote_delta_replay_key,
                    )?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (
                        local_session_id,
                        message_index,
                        message_count,
                        revision,
                        session_mutation_stamp,
                    )
                };
                self.publish_delta(&DeltaEvent::TextReplace {
                    revision,
                    session_id: local_session_id,
                    message_id,
                    message_index,
                    message_count,
                    text,
                    preview,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::CommandUpdate {
                command,
                command_language,
                message_count: remote_message_count,
                message_id,
                message_index,
                output,
                output_language,
                preview,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                status,
                ..
            } => {
                if message_index >= usize::try_from(remote_message_count).unwrap_or(usize::MAX) {
                    return Err(anyhow!(
                        "remote CommandUpdate index `{message_index}` is outside messageCount `{remote_message_count}` for session `{session_id}`"
                    ));
                }
                let hydration_outcome = self.hydrate_unloaded_remote_session_for_delta(
                    remote_id,
                    &session_id,
                    authority_generation,
                    remote_revision,
                    remote_message_count,
                    remote_session_mutation_stamp,
                    expected_remote,
                    expected_connection,
                    expected_state_continuity_generation,
                )?;
                if self.should_skip_delta_after_remote_hydration(
                    hydration_outcome,
                    &remote_delta_replay_key,
                ) {
                    return Ok(());
                }
                let (
                    local_session_id,
                    created_message,
                    applied_message_index,
                    message_count,
                    revision,
                    session_status,
                    session_mutation_stamp,
                ) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    ensure_expected_route(&inner)?;
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    self.retry_remote_delta_persist_if_dirty_locked(&mut inner)?;
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (
                        local_session_id,
                        created_message,
                        applied_message_index,
                        message_count,
                        session_status,
                        session_mutation_stamp,
                    ) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let (created_message, applied_message_index) = if let Some(existing_index) =
                            message_index_on_record(record, &message_id)
                        {
                            let Some(message) = record.session.messages.get_mut(existing_index)
                            else {
                                return Err(anyhow!(
                                    "remote message index `{existing_index}` is out of bounds"
                                ));
                            };
                            match message {
                                Message::Command {
                                    command: existing_command,
                                    command_language: existing_command_language,
                                    output: existing_output,
                                    output_language: existing_output_language,
                                    status: existing_status,
                                    ..
                                } => {
                                    *existing_command = command.clone();
                                    *existing_command_language = command_language.clone();
                                    *existing_output = output.clone();
                                    *existing_output_language = output_language.clone();
                                    *existing_status = status;
                                    (None, global_message_index(record, existing_index))
                                }
                                _ => {
                                    return Err(anyhow!(
                                        "remote message `{message_id}` is not a command message"
                                    ));
                                }
                            }
                        } else {
                            if record.session.messages.is_empty() {
                                record.message_start_index = message_index;
                            }
                            let local_message_index = message_index
                                .checked_sub(record.message_start_index)
                                .ok_or_else(|| {
                                    anyhow!(
                                        "remote CommandUpdate index `{message_index}` predates the retained transcript window in session `{session_id}`"
                                    )
                                })?;
                            if local_message_index > record.session.messages.len() {
                                return Err(anyhow!(
                                    "remote CommandUpdate index `{message_index}` leaves a gap in session `{session_id}`"
                                ));
                            }
                            let message = Message::Command {
                                id: message_id.clone(),
                                timestamp: stamp_now(),
                                author: Author::Assistant,
                                command: command.clone(),
                                command_language: command_language.clone(),
                                output: output.clone(),
                                output_language: output_language.clone(),
                                status,
                            };
                            insert_message_on_record(
                                record,
                                local_message_index,
                                message.clone(),
                            );
                            (Some(message), message_index)
                        };
                        record.session.preview = preview.clone();
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        (
                            record.session.id.clone(),
                            created_message,
                            applied_message_index,
                            session_message_count(record),
                            record.session.status,
                            record.mutation_stamp,
                        )
                    };
                    let revision = if created_message.is_some() {
                        self.commit_remote_delta_persisted_locked(&mut inner)?
                    } else {
                        self.commit_remote_streaming_delta_locked(
                            &mut inner,
                            remote_id,
                            remote_revision,
                            &remote_delta_replay_key,
                        )?
                    };
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (
                        local_session_id,
                        created_message,
                        applied_message_index,
                        message_count,
                        revision,
                        session_status,
                        session_mutation_stamp,
                    )
                };
                if let Some(message) = created_message {
                    self.publish_delta(&DeltaEvent::MessageCreated {
                        revision,
                        session_id: local_session_id,
                        message_id,
                        message_index: applied_message_index,
                        message_count,
                        message,
                        preview,
                        status: session_status,
                        session_mutation_stamp: Some(session_mutation_stamp),
                    });
                } else {
                    self.publish_delta(&DeltaEvent::CommandUpdate {
                        revision,
                        session_id: local_session_id,
                        message_id,
                        message_index: applied_message_index,
                        message_count,
                        command,
                        command_language,
                        output,
                        output_language,
                        status,
                        preview,
                        session_mutation_stamp: Some(session_mutation_stamp),
                    });
                }
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::ParallelAgentsUpdate {
                agents,
                message_count: remote_message_count,
                message_id,
                message_index,
                preview,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                ..
            } => {
                if message_index >= usize::try_from(remote_message_count).unwrap_or(usize::MAX) {
                    return Err(anyhow!(
                        "remote ParallelAgentsUpdate index `{message_index}` is outside messageCount `{remote_message_count}` for session `{session_id}`"
                    ));
                }
                let hydration_outcome = self.hydrate_unloaded_remote_session_for_delta(
                    remote_id,
                    &session_id,
                    authority_generation,
                    remote_revision,
                    remote_message_count,
                    remote_session_mutation_stamp,
                    expected_remote,
                    expected_connection,
                    expected_state_continuity_generation,
                )?;
                if self.should_skip_delta_after_remote_hydration(
                    hydration_outcome,
                    &remote_delta_replay_key,
                ) {
                    return Ok(());
                }
                let (
                    local_session_id,
                    created_message,
                    applied_message_index,
                    message_count,
                    revision,
                    session_status,
                    session_mutation_stamp,
                ) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    ensure_expected_route(&inner)?;
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    self.retry_remote_delta_persist_if_dirty_locked(&mut inner)?;
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (
                        local_session_id,
                        created_message,
                        applied_message_index,
                        message_count,
                        session_status,
                        session_mutation_stamp,
                    ) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let (created_message, applied_message_index) = if let Some(existing_index) =
                            message_index_on_record(record, &message_id)
                        {
                            let Some(message) = record.session.messages.get_mut(existing_index)
                            else {
                                return Err(anyhow!(
                                    "remote message index `{existing_index}` is out of bounds"
                                ));
                            };
                            match message {
                                Message::ParallelAgents {
                                    agents: existing_agents,
                                    ..
                                } => {
                                    *existing_agents = agents.clone();
                                    (None, global_message_index(record, existing_index))
                                }
                                _ => {
                                    return Err(anyhow!(
                                        "remote message `{message_id}` is not a parallel-agents message"
                                    ));
                                }
                            }
                        } else {
                            if record.session.messages.is_empty() {
                                record.message_start_index = message_index;
                            }
                            let local_message_index = message_index
                                .checked_sub(record.message_start_index)
                                .ok_or_else(|| {
                                    anyhow!(
                                        "remote ParallelAgentsUpdate index `{message_index}` predates the retained transcript window in session `{session_id}`"
                                    )
                                })?;
                            if local_message_index > record.session.messages.len() {
                                return Err(anyhow!(
                                    "remote ParallelAgentsUpdate index `{message_index}` leaves a gap in session `{session_id}`"
                                ));
                            }
                            let message = Message::ParallelAgents {
                                id: message_id.clone(),
                                timestamp: stamp_now(),
                                author: Author::Assistant,
                                agents: agents.clone(),
                            };
                            insert_message_on_record(
                                record,
                                local_message_index,
                                message.clone(),
                            );
                            (Some(message), message_index)
                        };
                        record.session.preview = preview.clone();
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        (
                            record.session.id.clone(),
                            created_message,
                            applied_message_index,
                            session_message_count(record),
                            record.session.status,
                            record.mutation_stamp,
                        )
                    };
                    let revision = if created_message.is_some() {
                        self.commit_remote_delta_persisted_locked(&mut inner)?
                    } else {
                        self.commit_remote_streaming_delta_locked(
                            &mut inner,
                            remote_id,
                            remote_revision,
                            &remote_delta_replay_key,
                        )?
                    };
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (
                        local_session_id,
                        created_message,
                        applied_message_index,
                        message_count,
                        revision,
                        session_status,
                        session_mutation_stamp,
                    )
                };
                if let Some(message) = created_message {
                    self.publish_delta(&DeltaEvent::MessageCreated {
                        revision,
                        session_id: local_session_id,
                        message_id,
                        message_index: applied_message_index,
                        message_count,
                        message,
                        preview,
                        status: session_status,
                        session_mutation_stamp: Some(session_mutation_stamp),
                    });
                } else {
                    self.publish_delta(&DeltaEvent::ParallelAgentsUpdate {
                        revision,
                        session_id: local_session_id,
                        message_id,
                        message_index: applied_message_index,
                        message_count,
                        agents,
                        preview,
                        session_mutation_stamp: Some(session_mutation_stamp),
                    });
                }
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::ConversationMarkerCreated {
                marker,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                ..
            } => {
                if marker.session_id != session_id {
                    return Err(anyhow!(
                        "remote marker payload session id `{}` did not match event id `{session_id}`",
                        marker.session_id
                    ));
                }
                let (local_session_id, localized_marker, revision, session_mutation_stamp) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    ensure_expected_route(&inner)?;
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    self.retry_remote_delta_persist_if_dirty_locked(&mut inner)?;
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let local_session_id = inner.sessions[index].session.id.clone();
                    let localized_marker =
                        localize_remote_conversation_marker(marker, &local_session_id).map_err(
                            |err| anyhow!("remote marker color was invalid: {}", err.message),
                        )?;
                    let session_mutation_stamp = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        if let Some(existing_index) = record
                            .session
                            .markers
                            .iter()
                            .position(|entry| entry.id == localized_marker.id)
                        {
                            record.session.markers[existing_index] = localized_marker.clone();
                        } else {
                            record.session.markers.push(localized_marker.clone());
                        }
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        record.mutation_stamp
                    };
                    let revision = self.commit_remote_delta_persisted_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (
                        local_session_id,
                        localized_marker,
                        revision,
                        session_mutation_stamp,
                    )
                };
                self.publish_delta(&DeltaEvent::ConversationMarkerCreated {
                    revision,
                    session_id: local_session_id,
                    marker: localized_marker,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::ConversationMarkerUpdated {
                marker,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                ..
            } => {
                if marker.session_id != session_id {
                    return Err(anyhow!(
                        "remote marker payload session id `{}` did not match event id `{session_id}`",
                        marker.session_id
                    ));
                }
                let (local_session_id, localized_marker, revision, session_mutation_stamp) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    ensure_expected_route(&inner)?;
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    self.retry_remote_delta_persist_if_dirty_locked(&mut inner)?;
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let local_session_id = inner.sessions[index].session.id.clone();
                    let localized_marker =
                        localize_remote_conversation_marker(marker, &local_session_id).map_err(
                            |err| anyhow!("remote marker color was invalid: {}", err.message),
                        )?;
                    let session_mutation_stamp = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        if let Some(existing_index) = record
                            .session
                            .markers
                            .iter()
                            .position(|entry| entry.id == localized_marker.id)
                        {
                            record.session.markers[existing_index] = localized_marker.clone();
                        } else {
                            record.session.markers.push(localized_marker.clone());
                        }
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        record.mutation_stamp
                    };
                    let revision = self.commit_remote_delta_persisted_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (
                        local_session_id,
                        localized_marker,
                        revision,
                        session_mutation_stamp,
                    )
                };
                self.publish_delta(&DeltaEvent::ConversationMarkerUpdated {
                    revision,
                    session_id: local_session_id,
                    marker: localized_marker,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::ConversationMarkerDeleted {
                marker_id,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                ..
            } => {
                let Some((local_session_id, revision, session_mutation_stamp)) = ({
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    ensure_expected_route(&inner)?;
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let existing_index = inner.sessions[index]
                        .session
                        .markers
                        .iter()
                        .position(|entry| entry.id == marker_id);
                    let Some(existing_index) = existing_index else {
                        self.retry_remote_delta_persist_if_dirty_locked(&mut inner)?;
                        inner.note_remote_applied_revision(remote_id, remote_revision);
                        drop(inner);
                        self.note_remote_applied_delta_replay(&remote_delta_replay_key);
                        return Ok(());
                    };
                    let (local_session_id, session_mutation_stamp) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let local_session_id = record.session.id.clone();
                        record.session.markers.remove(existing_index);
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        (local_session_id, record.mutation_stamp)
                    };
                    let revision = self.commit_remote_delta_persisted_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    Some((local_session_id, revision, session_mutation_stamp))
                }) else {
                    self.note_remote_applied_delta_replay(&remote_delta_replay_key);
                    return Ok(());
                };
                self.publish_delta(&DeltaEvent::ConversationMarkerDeleted {
                    revision,
                    session_id: local_session_id,
                    marker_id,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::OrchestratorsUpdated {
                orchestrators,
                sessions,
                ..
            } => {
                let (revision, localized_orchestrators) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    ensure_expected_route(&inner)?;
                    if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                        return Ok(());
                    }
                    let local_project_ids_by_remote_project_id =
                        remote_project_id_map(&inner, remote_id);
                    let remote_sessions_by_id = (!sessions.is_empty()).then(|| {
                        sessions
                            .iter()
                            .map(|session| (session.id.as_str(), session))
                            .collect::<HashMap<_, _>>()
                    });
                    let rollback_state = (
                        inner.next_session_number,
                        inner.sessions.clone(),
                        inner.orchestrator_instances.clone(),
                    );
                    if let Err(err) = sync_remote_orchestrators_inner(
                        &mut inner,
                        remote_id,
                        &orchestrators,
                        &local_project_ids_by_remote_project_id,
                        remote_sessions_by_id.as_ref(),
                    ) {
                        inner.next_session_number = rollback_state.0;
                        inner.sessions = rollback_state.1;
                        inner.orchestrator_instances = rollback_state.2;
                        return Err(err);
                    }
                    let revision = self.commit_remote_delta_persisted_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (revision, inner.orchestrator_instances.clone())
                };
                self.publish_orchestrators_updated(revision, localized_orchestrators);
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::CodexUpdated {
                revision: _,
                codex: _,
            } => {
                // CodexState is process-global runtime metadata, not localized
                // remote proxy state. Mark the remote revision consumed for
                // monotonicity, but intentionally do not fold the Codex payload
                // into local state; this watermark means "consumed" for this
                // informational variant, not "reflected in the proxy model".
                #[cfg(test)]
                self.remote_registry
                    .run_test_before_remote_informational_delta_watermark();
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                ensure_expected_route(&inner)?;
                if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                    return Ok(());
                }
                self.retry_remote_delta_persist_if_dirty_locked(&mut inner)?;
                inner.note_remote_applied_revision(remote_id, remote_revision);
                drop(inner);
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::DelegationCreated { .. }
            | DeltaEvent::DelegationWaitCreated { .. }
            | DeltaEvent::DelegationWaitConsumed { .. }
            | DeltaEvent::DelegationWaitResumeDispatchFailed { .. }
            | DeltaEvent::DelegationUpdated { .. }
            | DeltaEvent::DelegationCompleted { .. }
            | DeltaEvent::DelegationFailed { .. }
            | DeltaEvent::DelegationCanceled { .. } => {
                // Delegations are local parent/child session relationships.
                // Cross-machine delegation is a non-goal for this phase, so
                // consume the remote revision without mirroring the payload.
                #[cfg(test)]
                self.remote_registry
                    .run_test_before_remote_informational_delta_watermark();
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                ensure_expected_route(&inner)?;
                if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                    return Ok(());
                }
                self.retry_remote_delta_persist_if_dirty_locked(&mut inner)?;
                inner.note_remote_applied_revision(remote_id, remote_revision);
                drop(inner);
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
        }
        Ok(())
    }
}
