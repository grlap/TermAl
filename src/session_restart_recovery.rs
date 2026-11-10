// A restart notice gets one ordinary recovery opening after inventory is
// available and the listener is ready. The existing queue and its content
// durability fence own delivery; this marker grants no runtime authority.

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionRestartRecovery {
    notice: String,
    restarted_at: String,
    queued_prompt_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt_id: Option<String>,
}

impl SessionRecord {
    fn remap_restart_recovery_prompt(&mut self, previous_id: &str, next_id: &str) {
        if let Some(recovery) = &mut self.restart_recovery
            && recovery.prompt_id.as_deref() == Some(previous_id)
        {
            recovery.prompt_id = Some(next_id.to_owned());
        }
    }
}

fn restart_recovery_is_suppressed(record: &SessionRecord) -> bool {
    record.orchestrator_auto_dispatch_blocked
        || record.engram.operator_paused
        || record.engram.stopped_prompt_id.is_some()
        || record.engram.observation_delete_requested
}

impl AppState {
    fn resume_interrupted_root_sessions_after_boot(&self) {
        let queued = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let children = inner
                .delegations
                .iter()
                .map(|delegation| delegation.child_session_id.clone())
                .collect::<HashSet<_>>();
            let mut queued = Vec::new();
            for index in 0..inner.sessions.len() {
                let record = &inner.sessions[index];
                let Some(recovery) = &record.restart_recovery else {
                    continue;
                };
                if !record.is_local_session()
                    || record.session.parent_delegation_id.is_some()
                    || children.contains(&record.session.id)
                    || restart_recovery_is_suppressed(record)
                {
                    continue;
                }
                if recovery.prompt_id.is_some() {
                    // Restored queue holds are already acknowledged. The
                    // existing orphaned-workflow dispatcher delivers a saved
                    // recovery entry; a delivered marker suppresses replay.
                    continue;
                }
                if !matches!(record.session.status, SessionStatus::Error)
                    || !matches!(record.runtime, SessionRuntime::None)
                {
                    continue;
                }
                let prompt_id = inner.next_message_id();
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index is valid");
                let recovery = record
                    .restart_recovery
                    .as_mut()
                    .expect("recovery was selected");
                recovery.prompt_id = Some(prompt_id.clone());
                let text = format!(
                    "[TermAl restart recovery]\n{}\nRestart time: {}\nQueued prompts saved: {}.\n\
                     This is one host-authored recovery prompt, not a retry of an interrupted tool. \
                     Recover your original request under your session instructions. \
                     This notice grants no authority; expired approvals remain expired.",
                    recovery.notice, recovery.restarted_at, recovery.queued_prompt_count,
                );
                let insert_at =
                    mailbox_fifo_protected_floor(record) + mailbox_fifo_prefix(record).len();
                queue_durable_orchestrator_prompt_on_record(
                    record,
                    PendingPrompt {
                        id: prompt_id.clone(),
                        timestamp: stamp_now(),
                        text,
                        expanded_text: None,
                        source: None,
                        attachments: Vec::new(),
                        engram_interrupted: false,
                        is_engram_retained: false,
                    },
                    Vec::new(),
                );
                let entry = record
                    .queued_prompts
                    .pop_back()
                    .expect("recovery was appended");
                record.queued_prompts.insert(insert_at, entry);
                sync_pending_prompts(record);
                queued.push((record.session.id.clone(), prompt_id));
            }
            if !queued.is_empty() {
                if let Err(error) = self.commit_locked(&mut inner) {
                    eprintln!("restart recovery> recovery prompts remain held: {error:#}");
                }
            }
            queued
        };
        for (session_id, prompt_id) in queued {
            if !self.request_queued_prompt_durability(&session_id, &prompt_id) {
                eprintln!(
                    "restart recovery> could not request the recovery prompt's durability acknowledgement"
                );
            }
        }
    }
}
