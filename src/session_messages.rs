// Session-message mutation helpers — the five `AppState` methods that
// every runtime event handler ultimately calls to add to or modify a
// session transcript. Each method follows the same two-step pattern:
// mutate `SessionRecord.session.messages` under the state mutex and
// publish a matching `DeltaEvent` (see `src/wire.rs`) so every connected
// SSE subscriber sees the change.
//
// Three of the five participate in streaming-text reconciliation.
// `append_text_delta` and `replace_text_message` are driven by the
// `TurnRecorder` trait via `recorder_text_delta` and
// `recorder_replace_streaming_text` (see `src/recorders.rs`);
// `finish_streaming_text` is recorder-local and just clears the open
// message id so the next event starts a fresh one. `append_text_delta`
// lazily creates a placeholder `Message::Text` when the message_id is
// unknown — used when a delta arrives before any explicit "message
// start" event. `replace_text_message` rewrites the open streaming text
// wholesale when the agent's `message_stop` payload diverges from the
// streamed delta draft.
//
// `upsert_command_message` and `upsert_parallel_agents_message` use
// `message_id` as a stable upsert key: the first call creates a
// `Message::Command` / `Message::ParallelAgents`, later calls with the
// same id mutate that same message in place so an in-progress command
// can update its output/status without allocating a new message. The
// recorder layer (`recorder_command_started` / `recorder_command_completed`
// / `recorder_upsert_parallel_agents`) assigns and remembers the key.
//
// `message_id` is the stable identifier carried on every SSE event;
// `message_index` is the current position in `session.messages` (a
// shifting projection — today the vec is append-only so it's stable,
// but any future reorder would invalidate it). Each mutation may also
// update `session.preview` (the sidebar summary); pending-interaction
// callers rely on `latest_pending_interaction_preview` from
// `src/session_interaction.rs` to override the preview when an approval
// / user-input / elicitation is outstanding.
//
// Runtime dispatch paths: Claude NDJSON in `src/claude.rs`, Codex
// app-server events in `src/codex_events.rs`, ACP session updates in
// `src/acp.rs` — all reach these through the trait-backed recorders.

impl AppState {
    /// Appends a new `Message` variant to the session transcript. Allocates
    /// no message id itself — the caller provides one via
    /// `Message::id()`. Updates `session.preview` from the message body
    /// (via `Message::preview_text`), flips `session.status` to `Approval`
    /// when the new message is an interaction request, and publishes a
    /// `DeltaEvent::MessageCreated`. Called by every recorder `push_*`
    /// path (see `src/recorders.rs`) for whole-block events (text,
    /// thinking, diff, approval, subagent result, etc.).
    fn push_message(&self, session_id: &str, message: Message) -> Result<()> {
        let (revision, message, message_index, message_count, preview, status, session_mutation_stamp) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let (message_index, message_count, preview, status, session_mutation_stamp) = {
                let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
                if let Some(next_preview) = message.preview_text() {
                    record.session.preview = next_preview;
                }
                if matches!(
                    message,
                    Message::Approval { .. }
                        | Message::UserInputRequest { .. }
                        | Message::McpElicitationRequest { .. }
                        | Message::CodexAppRequest { .. }
                ) {
                    record.session.status = SessionStatus::Approval;
                }
                let message_index = push_message_on_record(record, message.clone());
                (
                    message_index,
                    session_message_count(record),
                    record.session.preview.clone(),
                    record.session.status,
                    record.mutation_stamp,
                )
            };
            let revision = self.commit_persisted_delta_locked(&mut inner)?;
            (
                revision,
                message,
                message_index,
                message_count,
                preview,
                status,
                session_mutation_stamp,
            )
        };

        self.publish_delta(&DeltaEvent::MessageCreated {
            revision,
            session_id: session_id.to_owned(),
            message_id: message.id().to_owned(),
            message_index,
            message_count,
            message,
            preview,
            status,
            session_mutation_stamp: Some(session_mutation_stamp),
        });
        Ok(())
    }

    /// Returns the last message ID.
    pub(crate) fn last_message_id(&self, session_id: &str) -> Result<Option<String>> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        Ok(inner.sessions[index]
            .session
            .messages
            .last()
            .map(|message| message.id().to_owned()))
    }

    /// Inserts a message immediately before the message with
    /// `anchor_message_id`, preserving the existing message indices
    /// up to the anchor. Used by the Codex path that buffers subagent
    /// results and must flush them ahead of the final assistant
    /// reply. Errors if the anchor is not found in the session.
    pub(crate) fn insert_message_before(
        &self,
        session_id: &str,
        anchor_message_id: &str,
        message: Message,
    ) -> Result<()> {
        let (revision, message, message_index, message_count, preview, status, session_mutation_stamp) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let (message_index, message_count, preview, status, session_mutation_stamp) = {
                let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
                let anchor_index =
                    message_index_on_record(record, anchor_message_id).ok_or_else(|| {
                        anyhow!(
                            "session `{session_id}` anchor message `{anchor_message_id}` not found"
                        )
                    })?;
                // This insertion path is currently reserved for subagent-result messages that do
                // not contribute preview/status text. Keep the existing session preview/status in
                // the emitted delta unless a future caller explicitly broadens that contract.
                let message_index = insert_message_on_record(record, anchor_index, message.clone());
                (
                    message_index,
                    session_message_count(record),
                    record.session.preview.clone(),
                    record.session.status,
                    record.mutation_stamp,
                )
            };
            let revision = self.commit_persisted_delta_locked(&mut inner)?;
            (
                revision,
                message,
                message_index,
                message_count,
                preview,
                status,
                session_mutation_stamp,
            )
        };

        self.publish_delta(&DeltaEvent::MessageCreated {
            revision,
            session_id: session_id.to_owned(),
            message_id: message.id().to_owned(),
            message_index,
            message_count,
            message,
            preview,
            status,
            session_mutation_stamp: Some(session_mutation_stamp),
        });
        Ok(())
    }

    /// Appends `delta` to the in-flight streaming `Message::Text` identified
    /// by `message_id` and publishes a `DeltaEvent::TextDelta`. The target
    /// message must already exist and be a `Text` variant — the lazy
    /// placeholder creation happens one layer up in
    /// `recorder_text_delta` (see `src/recorders.rs`), which pushes an
    /// empty `Message::Text` on the first delta of a turn and reuses
    /// that id for every subsequent delta. Refreshes `session.preview`
    /// from the growing text when non-empty. Called from every runtime's
    /// streaming-text path (Claude NDJSON in `src/claude.rs`, Codex
    /// reasoning/assistant deltas in `src/codex_events.rs`, ACP
    /// `text_delta` in `src/acp.rs`).
    fn append_text_delta(&self, session_id: &str, message_id: &str, delta: &str) -> Result<()> {
        let (preview, revision, message_index, message_count, session_mutation_stamp) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let mut preview = None;
            let (message_index, message_count, session_mutation_stamp) = {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                let message_index = message_index_on_record(record, message_id).ok_or_else(|| {
                    anyhow!("session `{session_id}` message `{message_id}` not found")
                })?;
                let session = &mut record.session;

                let Some(message) = session.messages.get_mut(message_index) else {
                    return Err(anyhow!(
                        "session `{session_id}` message index `{message_index}` is out of bounds"
                    ));
                };
                match message {
                    Message::Text { id, text, .. } if id == message_id => {
                        text.push_str(delta);
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            preview = Some(make_preview(trimmed));
                        }
                    }
                    _ => {
                        return Err(anyhow!(
                            "session `{session_id}` message `{message_id}` is not a text message"
                        ));
                    }
                }

                if let Some(next_preview) = preview.as_ref() {
                    session.preview = next_preview.clone();
                }
                (
                    message_index,
                    session_message_count(record),
                    record.mutation_stamp,
                )
            };
            let revision = self.commit_delta_locked(&mut inner)?;
            (
                preview,
                revision,
                message_index,
                message_count,
                session_mutation_stamp,
            )
        };

        self.publish_delta(&DeltaEvent::TextDelta {
            revision,
            session_id: session_id.to_owned(),
            message_id: message_id.to_owned(),
            message_index,
            message_count,
            delta: delta.to_owned(),
            preview,
            session_mutation_stamp: Some(session_mutation_stamp),
        });

        Ok(())
    }

    /// Replaces the full body of an existing streaming `Message::Text`
    /// with `text` (clearing the accumulated deltas) and publishes a
    /// `DeltaEvent::TextReplace`. Used when the agent emits a completed
    /// `message_stop` payload whose text diverges from what the deltas
    /// streamed — the recorder reconciliation in
    /// `recorder_replace_streaming_text` (see `src/recorders.rs`)
    /// forwards that authoritative text here. Errors if the message is
    /// not a `Text` variant. Refreshes `session.preview` from the
    /// replacement when non-empty. Called from the same runtime paths as
    /// `append_text_delta` when their message-stop handler detects a
    /// mismatch.
    fn replace_text_message(&self, session_id: &str, message_id: &str, text: &str) -> Result<()> {
        let (
            preview,
            revision,
            message_index,
            message_count,
            replacement_text,
            session_mutation_stamp,
        ) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let mut preview = None;
            let (message_index, message_count, session_mutation_stamp) = {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                let message_index = message_index_on_record(record, message_id).ok_or_else(|| {
                    anyhow!("session `{session_id}` message `{message_id}` not found")
                })?;
                let session = &mut record.session;

                let Some(message) = session.messages.get_mut(message_index) else {
                    return Err(anyhow!(
                        "session `{session_id}` message index `{message_index}` is out of bounds"
                    ));
                };
                match message {
                    Message::Text {
                        id,
                        text: current_text,
                        ..
                    } if id == message_id => {
                        current_text.clear();
                        current_text.push_str(text);
                        let trimmed = current_text.trim();
                        if !trimmed.is_empty() {
                            preview = Some(make_preview(trimmed));
                        }
                    }
                    _ => {
                        return Err(anyhow!(
                            "session `{session_id}` message `{message_id}` is not a text message"
                        ));
                    }
                }

                if let Some(next_preview) = preview.as_ref() {
                    session.preview = next_preview.clone();
                }
                (
                    message_index,
                    session_message_count(record),
                    record.mutation_stamp,
                )
            };
            let revision = self.commit_delta_locked(&mut inner)?;
            (
                preview,
                revision,
                message_index,
                message_count,
                text.to_owned(),
                session_mutation_stamp,
            )
        };

        self.publish_delta(&DeltaEvent::TextReplace {
            revision,
            session_id: session_id.to_owned(),
            message_id: message_id.to_owned(),
            message_index,
            message_count,
            text: replacement_text,
            preview,
            session_mutation_stamp: Some(session_mutation_stamp),
        });

        Ok(())
    }

    /// Creates or mutates a `Message::Command` keyed by `message_id`.
    /// On first call for an id the method allocates a fresh command
    /// message and publishes `DeltaEvent::MessageCreated`; subsequent
    /// calls with the same id overwrite command / output / status in
    /// place and publish `DeltaEvent::CommandUpdate`. Output language is
    /// inferred from the command string. Updates `session.preview`
    /// based on status (`Running` / `Success` / `Error`). Called from
    /// `recorder_command_started` and `recorder_command_completed`
    /// (see `src/recorders.rs`), which drive it from the Claude `bash`
    /// tool, Codex exec events, and ACP tool-call updates.
    fn upsert_command_message(
        &self,
        session_id: &str,
        message_id: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        let command_language = Some(shell_language().to_owned());
        let output_language = infer_command_output_language(command).map(str::to_owned);

        let (
            preview,
            revision,
            message_index,
            message_count,
            created_message,
            session_status,
            session_mutation_stamp,
        ) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let (
                message_index,
                message_count,
                created_message,
                preview,
                session_status,
                session_mutation_stamp,
            ) = {
                let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
                let (message_index, created_message) = if let Some(message_index) =
                    message_index_on_record(record, message_id)
                {
                    let Some(message) = record.session.messages.get_mut(message_index) else {
                        return Err(anyhow!(
                            "session `{session_id}` message index `{message_index}` is out of bounds"
                        ));
                    };
                    match message {
                        Message::Command {
                            id,
                            command: existing_command,
                            command_language: existing_command_language,
                            output: existing_output,
                            output_language: existing_output_language,
                            status: existing_status,
                            ..
                        } if id == message_id => {
                            *existing_command = command.to_owned();
                            *existing_command_language = command_language.clone();
                            *existing_output = output.to_owned();
                            *existing_output_language = output_language.clone();
                            *existing_status = status;
                            (message_index, None)
                        }
                        _ => {
                            return Err(anyhow!(
                                "session `{session_id}` message `{message_id}` is not a command message"
                            ));
                        }
                    }
                } else {
                    let message = Message::Command {
                        id: message_id.to_owned(),
                        timestamp: stamp_now(),
                        author: Author::Assistant,
                        command: command.to_owned(),
                        command_language: command_language.clone(),
                        output: output.to_owned(),
                        output_language: output_language.clone(),
                        status,
                    };
                    let message_index = push_message_on_record(record, message.clone());
                    (message_index, Some(message))
                };

                let preview = match status {
                    CommandStatus::Running => make_preview(&format!("Running {command}")),
                    CommandStatus::Success => {
                        if output.trim().is_empty() {
                            make_preview(&format!("Completed {command}"))
                        } else {
                            make_preview(output.trim())
                        }
                    }
                    CommandStatus::Error => {
                        if output.trim().is_empty() {
                            make_preview(&format!("Command failed: {command}"))
                        } else {
                            make_preview(output.trim())
                        }
                    }
                };
                record.session.preview = preview.clone();
                (
                    message_index,
                    session_message_count(record),
                    created_message,
                    preview,
                    record.session.status,
                    record.mutation_stamp,
                )
            };
            let revision = if created_message.is_some() {
                self.commit_persisted_delta_locked(&mut inner)?
            } else {
                self.commit_delta_locked(&mut inner)?
            };
            (
                preview,
                revision,
                message_index,
                message_count,
                created_message,
                session_status,
                session_mutation_stamp,
            )
        };

        if let Some(message) = created_message {
            self.publish_delta(&DeltaEvent::MessageCreated {
                revision,
                session_id: session_id.to_owned(),
                message_id: message_id.to_owned(),
                message_index,
                message_count,
                message,
                preview,
                status: session_status,
                session_mutation_stamp: Some(session_mutation_stamp),
            });
        } else {
            self.publish_delta(&DeltaEvent::CommandUpdate {
                revision,
                session_id: session_id.to_owned(),
                message_id: message_id.to_owned(),
                message_index,
                message_count,
                command: command.to_owned(),
                command_language,
                output: output.to_owned(),
                output_language,
                status,
                preview,
                session_mutation_stamp: Some(session_mutation_stamp),
            });
        }

        Ok(())
    }

    /// Creates or mutates a `Message::ParallelAgents` keyed by
    /// `message_id`. First call for an id pushes a new parallel-agents
    /// message and publishes `DeltaEvent::MessageCreated`; later calls
    /// replace the `agents` vec in place and publish
    /// `DeltaEvent::ParallelAgentsUpdate`, so per-subagent status
    /// transitions never append duplicate messages. Refreshes
    /// `session.preview` from the subagent roster
    /// (`parallel_agents_preview_text`). Called from
    /// `recorder_upsert_parallel_agents` (see `src/recorders.rs`) which
    /// currently only fires from Claude's `task` tool subagent
    /// progress tracking.
    fn upsert_parallel_agents_message(
        &self,
        session_id: &str,
        message_id: &str,
        agents: Vec<ParallelAgentProgress>,
    ) -> Result<()> {
        let (
            preview,
            revision,
            message_index,
            message_count,
            created_message,
            session_status,
            session_mutation_stamp,
        ) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let (
                message_index,
                message_count,
                created_message,
                preview,
                session_status,
                session_mutation_stamp,
            ) = {
                let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");

                let (message_index, created_message) = if let Some(message_index) =
                    message_index_on_record(record, message_id)
                {
                    let Some(message) = record.session.messages.get_mut(message_index) else {
                        return Err(anyhow!(
                            "session `{session_id}` message index `{message_index}` is out of bounds"
                        ));
                    };
                    match message {
                        Message::ParallelAgents {
                            id,
                            agents: existing_agents,
                            ..
                        } if id == message_id => {
                            *existing_agents = agents.clone();
                            (message_index, None)
                        }
                        _ => {
                            return Err(anyhow!(
                                "session `{session_id}` message `{message_id}` is not a parallel-agents message"
                            ));
                        }
                    }
                } else {
                    let message = Message::ParallelAgents {
                        id: message_id.to_owned(),
                        timestamp: stamp_now(),
                        author: Author::Assistant,
                        agents: agents.clone(),
                    };
                    let message_index = push_message_on_record(record, message.clone());
                    (message_index, Some(message))
                };

                let preview = parallel_agents_preview_text(&agents);
                record.session.preview = preview.clone();
                (
                    message_index,
                    session_message_count(record),
                    created_message,
                    preview,
                    record.session.status,
                    record.mutation_stamp,
                )
            };
            let revision = if created_message.is_some() {
                self.commit_persisted_delta_locked(&mut inner)?
            } else {
                self.commit_delta_locked(&mut inner)?
            };
            (
                preview,
                revision,
                message_index,
                message_count,
                created_message,
                session_status,
                session_mutation_stamp,
            )
        };

        if let Some(message) = created_message {
            self.publish_delta(&DeltaEvent::MessageCreated {
                revision,
                session_id: session_id.to_owned(),
                message_id: message_id.to_owned(),
                message_index,
                message_count,
                message,
                preview,
                status: session_status,
                session_mutation_stamp: Some(session_mutation_stamp),
            });
        } else {
            self.publish_delta(&DeltaEvent::ParallelAgentsUpdate {
                revision,
                session_id: session_id.to_owned(),
                message_id: message_id.to_owned(),
                message_index,
                message_count,
                agents,
                preview,
                session_mutation_stamp: Some(session_mutation_stamp),
            });
        }

        Ok(())
    }
}
