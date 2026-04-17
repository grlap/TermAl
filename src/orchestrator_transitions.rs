// Orchestrator transition scheduling + deadlock detection + prompt rendering.
//
// When an orchestrator session finishes a turn, the transition scheduler
// decides which (if any) downstream sessions should get dispatched with a
// consolidated prompt built from the upstream session's output. This
// module owns that whole surface: applying template settings on session
// create, walking the transition graph after a turn completes, detecting
// deadlocked consolidate nodes (nodes waiting on sources that already
// terminated), building the consolidated prompt, rendering transition
// summary/result text, and inspecting the pending-transition queue for
// next-action decisions.
//
// Covers (previously lines 1659-2575 of orchestrators.rs):
// - Template application: `apply_orchestrator_template_session_settings`
// - Per-session transition lookups:
//   `orchestrator_template_session_for_runtime_session`,
//   `orchestrator_template_session_for_instance_session`
// - Main scheduler: `schedule_orchestrator_transitions_for_completed_session`,
//   `acknowledge_pending_orchestrator_transition`,
//   `update_orchestrator_delivery_cursor`
// - Reference tracking: `referenced_sessions_for_orchestrators`
// - Deadlock detection: `mark_deadlocked_orchestrator_instances`,
//   `detect_deadlocked_consolidate_session_ids`,
//   `format_deadlocked_orchestrator_message`
// - Pending transition dispatch: `next_pending_transition_action`,
//   `collect_consolidated_pending_transitions`,
//   `inspect_consolidated_pending_transitions`
// - Prompt construction: `build_transition_result_text`,
//   `build_consolidated_transition_prompt`,
//   `combine_transition_summary_and_result`,
//   `render_transition_prompt`, `build_orchestrator_destination_prompt`
// - Transition message extraction:
//   `current_turn_transition_messages`,
//   `latest_transition_message_summary`, `transition_message_summary`,
//   `latest_transition_message_text`, `transition_message_text`
//
// Extracted from orchestrators.rs so that file can stay focused on
// template CRUD + instance lifecycle (create/pause/resume/stop).

/// Applies orchestrator template session settings.
fn apply_orchestrator_template_session_settings(
    record: &mut SessionRecord,
    template_session: &OrchestratorSessionTemplate,
) {
    record.session.name = template_session.name.clone();
    if let Some(model) = template_session.model.as_ref() {
        record.session.model = model.clone();
    }

    match record.session.agent {
        agent if agent.supports_codex_prompt_settings() => {
            let approval_policy = if template_session.auto_approve {
                CodexApprovalPolicy::Never
            } else {
                CodexApprovalPolicy::OnRequest
            };
            record.codex_approval_policy = approval_policy;
            record.session.approval_policy = Some(approval_policy);
            record.session.reasoning_effort = Some(record.codex_reasoning_effort);
            record.session.sandbox_mode = Some(record.codex_sandbox_mode);
        }
        agent if agent.supports_claude_approval_mode() => {
            record.session.claude_approval_mode = Some(if template_session.auto_approve {
                ClaudeApprovalMode::AutoApprove
            } else {
                ClaudeApprovalMode::Ask
            });
        }
        agent if agent.supports_cursor_mode() => {
            record.session.cursor_mode = Some(if template_session.auto_approve {
                CursorMode::Agent
            } else {
                CursorMode::Ask
            });
        }
        agent if agent.supports_gemini_approval_mode() => {
            record.session.gemini_approval_mode = Some(if template_session.auto_approve {
                GeminiApprovalMode::AutoEdit
            } else {
                GeminiApprovalMode::Plan
            });
        }
        _ => {}
    }
}

/// Acknowledges pending orchestrator transition.
fn acknowledge_pending_orchestrator_transition(
    inner: &mut StateInner,
    instance_index: usize,
    pending: &PendingTransition,
) {
    let instance = &mut inner.orchestrator_instances[instance_index];
    instance
        .pending_transitions
        .retain(|candidate| candidate.id != pending.id);
    update_orchestrator_delivery_cursor(
        instance,
        &pending.source_session_id,
        pending.completion_revision,
    );
}

/// Handles orchestrator template session for runtime session.
fn orchestrator_template_session_for_runtime_session(
    inner: &StateInner,
    session_id: &str,
) -> Option<OrchestratorSessionTemplate> {
    inner.orchestrator_instances.iter().find_map(|instance| {
        instance
            .session_instances
            .iter()
            .find(|session_instance| session_instance.session_id == session_id)
            .and_then(|session_instance| {
                instance
                    .template_snapshot
                    .sessions
                    .iter()
                    .find(|template_session| {
                        template_session.id == session_instance.template_session_id
                    })
                    .cloned()
            })
    })
}

/// Handles orchestrator template session for instance session.
fn orchestrator_template_session_for_instance_session(
    instance: &OrchestratorInstance,
    session_id: &str,
) -> Option<OrchestratorSessionTemplate> {
    instance
        .session_instances
        .iter()
        .find(|session_instance| session_instance.session_id == session_id)
        .and_then(|session_instance| {
            instance
                .template_snapshot
                .sessions
                .iter()
                .find(|template_session| {
                    template_session.id == session_instance.template_session_id
                })
                .cloned()
        })
}

/// Handles schedule orchestrator transitions for completed session.
fn schedule_orchestrator_transitions_for_completed_session(
    inner: &mut StateInner,
    stopping_orchestrator_session_ids: &HashMap<String, HashSet<String>>,
    session_id: &str,
    completion_revision: u64,
) -> bool {
    let Some(source_record) = inner
        .find_session_index(session_id)
        .and_then(|index| inner.sessions.get(index))
        .cloned()
    else {
        return false;
    };

    let mut changed = false;
    for instance in &mut inner.orchestrator_instances {
        if instance.status == OrchestratorInstanceStatus::Stopped {
            continue;
        }

        let Some(session_instance_index) = instance
            .session_instances
            .iter()
            .position(|candidate| candidate.session_id == session_id)
        else {
            continue;
        };

        let (template_session_id, last_delivered_completion_revision, prior_completion_revision) = {
            let session_instance = &mut instance.session_instances[session_instance_index];
            let prior_completion_revision = session_instance.last_completion_revision;
            let prior_delivered_completion_revision =
                session_instance.last_delivered_completion_revision;
            let next_completion_revision = Some(
                session_instance
                    .last_completion_revision
                    .unwrap_or(0)
                    .max(completion_revision),
            );
            session_instance.last_completion_revision = next_completion_revision;
            (
                session_instance.template_session_id.clone(),
                prior_delivered_completion_revision,
                prior_completion_revision,
            )
        };
        if prior_completion_revision != Some(completion_revision) {
            changed = true;
        }
        if completion_revision <= last_delivered_completion_revision.unwrap_or(0) {
            continue;
        }
        let source_template = instance
            .template_snapshot
            .sessions
            .iter()
            .find(|session| session.id == template_session_id)
            .cloned();
        let stopped_destination_session_ids = stopping_orchestrator_session_ids.get(&instance.id);
        let mut suppressed_stopped_destinations = false;

        for transition in instance
            .template_snapshot
            .transitions
            .iter()
            .filter(|transition| {
                transition.trigger == OrchestratorTransitionTrigger::OnCompletion
                    && transition.from_session_id == template_session_id
            })
        {
            if instance.pending_transitions.iter().any(|pending| {
                pending.transition_id == transition.id
                    && pending.source_session_id == session_id
                    && pending.completion_revision == completion_revision
            }) {
                continue;
            }

            let Some(destination_session_id) = instance
                .session_instances
                .iter()
                .find(|candidate| candidate.template_session_id == transition.to_session_id)
                .map(|candidate| candidate.session_id.clone())
            else {
                continue;
            };
            if stopped_destination_session_ids.is_some_and(|stopped_session_ids| {
                stopped_session_ids.contains(&destination_session_id)
            }) {
                suppressed_stopped_destinations = true;
                continue;
            }

            let result = build_transition_result_text(&source_record, transition.result_mode);
            let rendered_prompt = render_transition_prompt(
                transition,
                source_template.as_ref(),
                &source_record.session,
                &result,
            );
            instance.pending_transitions.push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: transition.id.clone(),
                source_session_id: session_id.to_owned(),
                destination_session_id,
                completion_revision,
                rendered_prompt,
                created_at: stamp_orchestrator_template_now(),
            });
            changed = true;
        }

        if suppressed_stopped_destinations {
            update_orchestrator_delivery_cursor(instance, session_id, completion_revision);
            let next_delivered_completion_revision = instance
                .session_instances
                .iter()
                .find(|candidate| candidate.session_id == session_id)
                .and_then(|candidate| candidate.last_delivered_completion_revision);
            if last_delivered_completion_revision != next_delivered_completion_revision {
                changed = true;
            }
        }
    }

    changed
}

/// Updates orchestrator delivery cursor.
fn update_orchestrator_delivery_cursor(
    instance: &mut OrchestratorInstance,
    source_session_id: &str,
    completion_revision: u64,
) {
    let still_pending_for_completion = instance.pending_transitions.iter().any(|pending| {
        pending.source_session_id == source_session_id
            && pending.completion_revision == completion_revision
    });
    if still_pending_for_completion {
        return;
    }

    if let Some(session_instance) = instance
        .session_instances
        .iter_mut()
        .find(|candidate| candidate.session_id == source_session_id)
    {
        session_instance.last_delivered_completion_revision = Some(
            session_instance
                .last_delivered_completion_revision
                .unwrap_or(0)
                .max(completion_revision),
        );
    }
}

fn referenced_sessions_for_orchestrators(
    inner: &StateInner,
    orchestrators: &[OrchestratorInstance],
) -> Vec<Session> {
    let referenced_session_ids = orchestrators
        .iter()
        .flat_map(|instance| {
            instance
                .session_instances
                .iter()
                .map(|session| session.session_id.as_str())
                .chain(instance.pending_transitions.iter().flat_map(|transition| {
                    [
                        transition.source_session_id.as_str(),
                        transition.destination_session_id.as_str(),
                    ]
                }))
                .chain(
                    instance
                        .active_session_ids_during_stop
                        .iter()
                        .flatten()
                        .map(String::as_str),
                )
                .chain(
                    instance
                        .stopped_session_ids_during_stop
                        .iter()
                        .map(String::as_str),
                )
        })
        .collect::<HashSet<_>>();
    if referenced_session_ids.is_empty() {
        return Vec::new();
    }

    inner
        .sessions
        .iter()
        .filter(|record| referenced_session_ids.contains(record.session.id.as_str()))
        .map(|record| record.session.clone())
        .collect()
}

/// Marks deadlocked orchestrator instances.
fn mark_deadlocked_orchestrator_instances(
    inner: &mut StateInner,
    stopping_orchestrator_ids: &HashSet<String>,
) -> bool {
    let deadlocks = inner
        .orchestrator_instances
        .iter()
        .enumerate()
        .filter_map(|(instance_index, instance)| {
            if instance.status != OrchestratorInstanceStatus::Running
                || instance.remote_id.is_some()
                || instance.stop_in_progress
                || stopping_orchestrator_ids.contains(&instance.id)
            {
                return None;
            }

            let deadlocked_session_ids = detect_deadlocked_consolidate_session_ids(inner, instance);
            if deadlocked_session_ids.is_empty() {
                return None;
            }

            Some((
                instance_index,
                deadlocked_session_ids.clone(),
                format_deadlocked_orchestrator_message(inner, instance, &deadlocked_session_ids),
            ))
        })
        .collect::<Vec<_>>();
    if deadlocks.is_empty() {
        return false;
    }

    for (instance_index, deadlocked_session_ids, error_message) in deadlocks {
        let instance_session_ids = {
            let instance = &mut inner.orchestrator_instances[instance_index];
            let instance_session_ids = instance
                .session_instances
                .iter()
                .map(|session| session.session_id.clone())
                .collect::<Vec<_>>();
            instance.status = OrchestratorInstanceStatus::Stopped;
            instance.pending_transitions.clear();
            instance.error_message = Some(error_message.clone());
            instance.completed_at = Some(stamp_orchestrator_template_now());
            instance_session_ids
        };
        for session_id in instance_session_ids {
            let Some(session_index) = inner.find_session_index(&session_id) else {
                continue;
            };
            clear_stopped_orchestrator_queued_prompts(
                    inner
                        .session_mut_by_index(session_index)
                        .expect("session index should be valid"),
                );
        }

        for session_id in deadlocked_session_ids {
            let Some(session_index) = inner.find_session_index(&session_id) else {
                continue;
            };
            let session = &mut inner
                .session_mut_by_index(session_index)
                .expect("session index should be valid")
                .session;
            session.status = SessionStatus::Error;
            session.preview = make_preview(&error_message);
        }
    }

    true
}

/// Detects deadlocked consolidate session IDs.
fn detect_deadlocked_consolidate_session_ids(
    inner: &StateInner,
    instance: &OrchestratorInstance,
) -> Vec<String> {
    let mut blocked_destinations = instance
        .pending_transitions
        .iter()
        .map(|pending| pending.destination_session_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .filter_map(|destination_session_id| {
            let destination_template = orchestrator_template_session_for_instance_session(
                instance,
                &destination_session_id,
            )?;
            if destination_template.input_mode != OrchestratorSessionInputMode::Consolidate {
                return None;
            }

            let destination_session_index = inner.find_session_index(&destination_session_id)?;
            let destination_record = &inner.sessions[destination_session_index];
            if matches!(
                destination_record.session.status,
                SessionStatus::Active | SessionStatus::Approval
            ) || !destination_record.queued_prompts.is_empty()
            {
                return None;
            }

            let inspection =
                inspect_consolidated_pending_transitions(instance, &destination_session_id)?;
            if inspection.missing_source_session_ids.is_empty() {
                return None;
            }

            Some((
                destination_session_id,
                inspection
                    .missing_source_session_ids
                    .into_iter()
                    .collect::<HashSet<_>>(),
            ))
        })
        .collect::<HashMap<_, _>>();
    if blocked_destinations.is_empty() {
        return Vec::new();
    }

    // Repeatedly prune any blocked destination that still depends on a source
    // outside the blocked set. The sessions that remain are waiting only on
    // each other, so they cannot make progress without an external completion.
    loop {
        let removable = blocked_destinations
            .iter()
            .filter(|(_, missing_source_session_ids)| {
                missing_source_session_ids
                    .iter()
                    .any(|session_id| !blocked_destinations.contains_key(session_id))
            })
            .map(|(destination_session_id, _)| destination_session_id.clone())
            .collect::<Vec<_>>();
        if removable.is_empty() {
            break;
        }
        for destination_session_id in removable {
            blocked_destinations.remove(&destination_session_id);
        }
    }

    let mut deadlocked_session_ids = blocked_destinations.into_keys().collect::<Vec<_>>();
    deadlocked_session_ids.sort();
    deadlocked_session_ids
}

/// Formats deadlocked orchestrator message.
fn format_deadlocked_orchestrator_message(
    inner: &StateInner,
    instance: &OrchestratorInstance,
    deadlocked_session_ids: &[String],
) -> String {
    let mut deadlocked_session_names = deadlocked_session_ids
        .iter()
        .map(|session_id| {
            inner
                .find_session_index(session_id)
                .and_then(|index| inner.sessions.get(index))
                .map(|record| record.session.name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .or_else(|| {
                    orchestrator_template_session_for_instance_session(instance, session_id)
                        .map(|session| session.name)
                })
                .unwrap_or_else(|| session_id.clone())
        })
        .collect::<Vec<_>>();
    deadlocked_session_names.sort();

    if deadlocked_session_names.len() == 1 {
        format!(
            "Orchestrator deadlock: consolidate session {} is waiting only on blocked consolidate inputs.",
            deadlocked_session_names[0]
        )
    } else {
        format!(
            "Orchestrator deadlock: consolidate sessions {} are waiting only on blocked consolidate inputs.",
            deadlocked_session_names.join(", ")
        )
    }
}

/// Builds transition result text.
fn build_transition_result_text(
    record: &SessionRecord,
    mode: OrchestratorTransitionResultMode,
) -> String {
    let current_turn_messages = current_turn_transition_messages(record);
    match mode {
        OrchestratorTransitionResultMode::None => String::new(),
        OrchestratorTransitionResultMode::LastResponse => {
            latest_transition_message_text(current_turn_messages).unwrap_or_default()
        }
        OrchestratorTransitionResultMode::Summary => {
            latest_transition_message_summary(current_turn_messages)
                .unwrap_or_else(|| record.session.preview.trim().to_owned())
        }
        OrchestratorTransitionResultMode::SummaryAndLastResponse => {
            let summary = latest_transition_message_summary(current_turn_messages)
                .unwrap_or_else(|| record.session.preview.trim().to_owned());
            let last_response =
                latest_transition_message_text(current_turn_messages).unwrap_or_default();
            combine_transition_summary_and_result(&summary, &last_response)
        }
    }
}

/// Returns the next pending transition action.
fn next_pending_transition_action(
    inner: &StateInner,
    stopping_orchestrator_ids: &HashSet<String>,
) -> Option<PendingTransitionAction> {
    for (instance_index, instance) in inner.orchestrator_instances.iter().enumerate() {
        if instance.status != OrchestratorInstanceStatus::Running
            || instance.remote_id.is_some()
            || instance.stop_in_progress
            || stopping_orchestrator_ids.contains(&instance.id)
        {
            continue;
        }

        for pending in &instance.pending_transitions {
            let destination_template = orchestrator_template_session_for_instance_session(
                instance,
                &pending.destination_session_id,
            );
            let Some(destination_session_index) =
                inner.find_session_index(&pending.destination_session_id)
            else {
                return Some(PendingTransitionAction::Acknowledge {
                    instance_index,
                    pendings: vec![pending.clone()],
                });
            };
            if inner.sessions[destination_session_index].orchestrator_auto_dispatch_blocked {
                continue;
            }

            let input_mode = destination_template
                .as_ref()
                .map(|template| template.input_mode)
                .unwrap_or_default();
            if input_mode == OrchestratorSessionInputMode::Queue {
                return Some(PendingTransitionAction::Deliver {
                    destination_session_id: pending.destination_session_id.clone(),
                    destination_template,
                    instance_index,
                    pendings: vec![pending.clone()],
                    rendered_prompt: pending.rendered_prompt.clone(),
                });
            }

            let Some(ConsolidatedPendingTransitions {
                prompt_pendings,
                acknowledged_pendings,
            }) =
                collect_consolidated_pending_transitions(instance, &pending.destination_session_id)
            else {
                continue;
            };
            if acknowledged_pendings.is_empty() {
                continue;
            }
            return Some(PendingTransitionAction::Deliver {
                destination_session_id: pending.destination_session_id.clone(),
                destination_template,
                instance_index,
                pendings: acknowledged_pendings,
                rendered_prompt: build_consolidated_transition_prompt(instance, &prompt_pendings),
            });
        }
    }

    None
}

/// Collects consolidated pending transitions.
fn collect_consolidated_pending_transitions(
    instance: &OrchestratorInstance,
    destination_session_id: &str,
) -> Option<ConsolidatedPendingTransitions> {
    let ConsolidatedPendingInspection {
        prompt_pendings,
        acknowledged_pendings,
        missing_source_session_ids,
    } = inspect_consolidated_pending_transitions(instance, destination_session_id)?;
    if !missing_source_session_ids.is_empty() {
        return None;
    }

    Some(ConsolidatedPendingTransitions {
        prompt_pendings,
        acknowledged_pendings,
    })
}

/// Inspects consolidated pending transitions.
fn inspect_consolidated_pending_transitions(
    instance: &OrchestratorInstance,
    destination_session_id: &str,
) -> Option<ConsolidatedPendingInspection> {
    let destination_template =
        orchestrator_template_session_for_instance_session(instance, destination_session_id)?;
    let live_session_ids_by_template = instance
        .session_instances
        .iter()
        .map(|session| {
            (
                session.template_session_id.as_str(),
                session.session_id.as_str(),
            )
        })
        .collect::<HashMap<_, _>>();
    let required_transitions = instance
        .template_snapshot
        .transitions
        .iter()
        .filter(|transition| {
            transition.trigger == OrchestratorTransitionTrigger::OnCompletion
                && transition.to_session_id == destination_template.id
                && live_session_ids_by_template.contains_key(transition.from_session_id.as_str())
        })
        .collect::<Vec<_>>();
    if required_transitions.is_empty() {
        return None;
    }

    let mut prompt_pendings = Vec::with_capacity(required_transitions.len());
    let mut acknowledged_pendings = Vec::new();
    let mut missing_source_session_ids = Vec::new();
    for transition in required_transitions {
        let Some(source_session_id) =
            live_session_ids_by_template.get(transition.from_session_id.as_str())
        else {
            continue;
        };
        let transition_pendings = instance
            .pending_transitions
            .iter()
            .filter(|pending| {
                pending.destination_session_id == destination_session_id
                    && pending.transition_id == transition.id
            })
            .cloned()
            .collect::<Vec<_>>();
        if transition_pendings.is_empty() {
            missing_source_session_ids.push((*source_session_id).to_owned());
            continue;
        }

        let latest_pending = transition_pendings
            .iter()
            .max_by_key(|pending| pending.completion_revision)
            .cloned()
            .expect("non-empty transition pendings should have a latest revision");
        prompt_pendings.push(latest_pending);
        acknowledged_pendings.extend(transition_pendings);
    }

    Some(ConsolidatedPendingInspection {
        prompt_pendings,
        acknowledged_pendings,
        missing_source_session_ids,
    })
}

/// Builds consolidated transition prompt.
fn build_consolidated_transition_prompt(
    instance: &OrchestratorInstance,
    pendings: &[PendingTransition],
) -> String {
    let sections = pendings
        .iter()
        .filter_map(|pending| {
            let rendered_prompt = pending.rendered_prompt.trim();
            if rendered_prompt.is_empty() {
                return None;
            }

            let source_name = orchestrator_template_session_for_instance_session(
                instance,
                &pending.source_session_id,
            )
            .map(|session| session.name)
            .unwrap_or_else(|| pending.source_session_id.clone());
            Some(format!(
                "From {} ({})\n{}",
                source_name, pending.transition_id, rendered_prompt
            ))
        })
        .collect::<Vec<_>>();

    match sections.as_slice() {
        [] => String::new(),
        [section] => section.clone(),
        _ => format!(
            "Consolidated predecessor inputs:\n\n{}",
            sections.join("\n\n---\n\n")
        ),
    }
}

/// Combines transition summary and result.
fn combine_transition_summary_and_result(summary: &str, last_response: &str) -> String {
    let summary = summary.trim();
    let last_response = last_response.trim();
    match (summary.is_empty(), last_response.is_empty()) {
        (true, true) => String::new(),
        (false, true) => summary.to_owned(),
        (true, false) => last_response.to_owned(),
        (false, false) if summary == last_response => summary.to_owned(),
        (false, false) => format!("Summary:\n{summary}\n\nLast response:\n{last_response}"),
    }
}

/// Returns the current turn transition messages.
fn current_turn_transition_messages(record: &SessionRecord) -> &[Message] {
    record
        .active_turn_start_message_count
        .and_then(|start| record.session.messages.get(start..))
        .unwrap_or(record.session.messages.as_slice())
}

/// Returns the latest transition message summary.
fn latest_transition_message_summary(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(transition_message_summary)
}

/// Handles transition message summary.
fn transition_message_summary(message: &Message) -> Option<String> {
    match message {
        Message::Text {
            author: Author::Assistant,
            text,
            attachments,
            ..
        } => Some(prompt_preview_text(text, attachments)),
        Message::Thinking { title, .. } => Some(make_preview(title)),
        Message::Command {
            command, status, ..
        } => match status {
            CommandStatus::Running => None,
            CommandStatus::Success => Some(format!("Ran {} successfully.", make_preview(command))),
            CommandStatus::Error => Some(format!("Command failed: {}.", make_preview(command))),
        },
        Message::Diff { summary, .. } => Some(make_preview(summary)),
        Message::Markdown { title, .. } => Some(make_preview(title)),
        Message::SubagentResult { title, summary, .. } => {
            let detail = summary.trim();
            if detail.is_empty() {
                Some(make_preview(title))
            } else {
                Some(make_preview(detail))
            }
        }
        Message::ParallelAgents { agents, .. } => Some(parallel_agents_preview_text(agents)),
        Message::FileChanges { .. } => None,
        Message::Approval { .. }
        | Message::UserInputRequest { .. }
        | Message::McpElicitationRequest { .. }
        | Message::CodexAppRequest { .. }
        | Message::Text {
            author: Author::You,
            ..
        } => None,
    }
}

/// Returns the latest transition message text.
fn latest_transition_message_text(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(transition_message_text)
}

/// Handles transition message text.
fn transition_message_text(message: &Message) -> Option<String> {
    match message {
        Message::Text {
            author: Author::Assistant,
            text,
            expanded_text,
            ..
        } => Some(expanded_text.as_deref().unwrap_or(text).trim().to_owned()),
        Message::Markdown {
            title, markdown, ..
        } => Some(
            format!("{}\n\n{}", title.trim(), markdown.trim())
                .trim()
                .to_owned(),
        ),
        Message::Diff { summary, diff, .. } => Some(
            format!("{}\n\n{}", summary.trim(), diff.trim())
                .trim()
                .to_owned(),
        ),
        Message::SubagentResult { title, summary, .. } => Some(
            format!("{}\n\n{}", title.trim(), summary.trim())
                .trim()
                .to_owned(),
        ),
        Message::ParallelAgents { agents, .. } => Some(parallel_agents_preview_text(agents)),
        Message::FileChanges { title, files, .. } => {
            let paths = files
                .iter()
                .map(|file| file.path.trim())
                .filter(|path| !path.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            Some(format!("{}\n\n{}", title.trim(), paths).trim().to_owned())
        }
        Message::Thinking { title, lines, .. } => {
            let mut parts = vec![title.trim().to_owned()];
            let detail = lines
                .iter()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if !detail.is_empty() {
                parts.push(detail);
            }
            Some(parts.join("\n\n"))
        }
        Message::Command {
            command,
            output,
            status,
            ..
        } => Some(
            format!(
                "Command: {}\nStatus: {}\n\n{}",
                command.trim(),
                status.label(),
                output.trim()
            )
            .trim()
            .to_owned(),
        ),
        Message::Approval { .. }
        | Message::UserInputRequest { .. }
        | Message::McpElicitationRequest { .. }
        | Message::CodexAppRequest { .. }
        | Message::Text {
            author: Author::You,
            ..
        } => None,
    }
}

/// Renders transition prompt.
fn render_transition_prompt(
    transition: &OrchestratorTemplateTransition,
    source_template: Option<&OrchestratorSessionTemplate>,
    source_session: &Session,
    result: &str,
) -> String {
    let template = transition
        .prompt_template
        .as_deref()
        .unwrap_or("{{result}}");
    let rendered = template
        .replace("{{result}}", result)
        .replace(
            "{{sourceSessionId}}",
            source_template
                .map(|session| session.id.as_str())
                .unwrap_or(source_session.id.as_str()),
        )
        .replace(
            "{{sourceSessionName}}",
            source_template
                .map(|session| session.name.as_str())
                .unwrap_or(source_session.name.as_str()),
        )
        .replace("{{transitionId}}", &transition.id);

    if rendered.trim().is_empty() {
        result.trim().to_owned()
    } else {
        rendered.trim().to_owned()
    }
}

/// Builds orchestrator destination prompt.
fn build_orchestrator_destination_prompt(
    destination_record: &SessionRecord,
    instructions: &str,
    rendered_prompt: &str,
) -> String {
    let prompt = rendered_prompt.trim();
    let instructions = instructions.trim();
    let should_prefix_instructions = destination_record.session.messages.is_empty()
        && destination_record.queued_prompts.is_empty()
        && !instructions.is_empty();

    match (should_prefix_instructions, prompt.is_empty()) {
        (false, false) => prompt.to_owned(),
        (false, true) => String::new(),
        (true, true) => instructions.to_owned(),
        (true, false) => format!(
            "Session instructions:\n{}\n\nPrompt:\n{}",
            instructions, prompt
        ),
    }
}
