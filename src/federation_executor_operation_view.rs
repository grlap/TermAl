// Owns: one read-only view of a received executor operation at its receiver,
// the satellite: the journal row, and the target root's queue and delivery
// state for that operation's queue entry. Reads the journal off the state
// lock, then takes one StateInner snapshot: the two are sampled one after the
// other, never one atomic cut across both stores. Never mutates either store.
// A storage failure is an error, never reported as an absent operation.
// Does not own any route, authentication or binding policy: each caller
// gates its own use (the central's authenticated outcome query, or a local
// diagnostic read), and keeps its own response contract. Does not own the
// journal (federation_executor_journal.rs) or delivery (federation_executor_
// delivery.rs). New file.

/// A queued prompt's identifying metadata. Never the prompt text or body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FederationExecutorPendingPromptMetadata {
    prompt_id: String,
    timestamp: String,
    /// Who queued it: a user, a mailbox wake or an orchestrator.
    source: QueuedPromptSource,
    mailbox_id: Option<String>,
    message_id: Option<String>,
    sequence: Option<u64>,
}

/// At most this many of the root's queued prompts are listed.
const FEDERATION_EXECUTOR_VIEW_QUEUE_LIMIT: usize = 100;

/// What the receiver knows of one executor operation, at one read.
#[derive(Clone, Debug)]
struct FederationExecutorOperationView {
    /// The exact identity retained by the journal.
    operation: FederationExecutorOperation,
    stage: String,
    // Read by the receiver's local diagnostic view; the outcome needs only the stage.
    #[cfg_attr(not(test), allow(dead_code))]
    effect_state: String,
    #[cfg_attr(not(test), allow(dead_code))]
    queue_entry_id: Option<String>,
    runtime_acknowledged: bool,
    /// The root's delivery receipt for this operation's queue entry, when the
    /// receipt names this exact operation.
    receipt: Option<PromptDeliveryDisposition>,
    /// Whether the queue entry is still queued, and its position if so.
    queue_position: Option<usize>,
    #[cfg_attr(not(test), allow(dead_code))]
    pending_prompt: Option<FederationExecutorPendingPromptMetadata>,
    /// The root's whole queue, as metadata only: its first
    /// FEDERATION_EXECUTOR_VIEW_QUEUE_LIMIT entries, its full count, and
    /// whether the list was cut.
    #[cfg_attr(not(test), allow(dead_code))]
    pending_prompts: Vec<FederationExecutorPendingPromptMetadata>,
    #[cfg_attr(not(test), allow(dead_code))]
    pending_prompt_count: usize,
    #[cfg_attr(not(test), allow(dead_code))]
    pending_prompts_truncated: bool,
    /// Whether the target root exists at this read; with it absent, every root
    /// field below is its empty value.
    #[cfg_attr(not(test), allow(dead_code))]
    root_present: bool,
    /// Whether this host is the operation's target executor. A row targeted
    /// at another host is central's ownership mirror, never execution evidence.
    #[cfg_attr(not(test), allow(dead_code))]
    executor_local: bool,
    root_status: Option<SessionStatus>,
    root_paused: bool,
    root_stopping: bool,
    /// Whether the operation's exact target binding is current at this read
    /// (`federation_executor_target_locked`).
    binding_current: bool,
}

impl AppState {
    /// The receiver's view of the operation `operation_key` from the central
    /// host `central_host_id`, or `None` when the journal holds no such row.
    fn read_federation_executor_operation_view(
        &self,
        central_host_id: &str,
        operation_key: &str,
    ) -> Result<Option<FederationExecutorOperationView>> {
        let record = {
            let connection = self.mailbox_store.connection()?;
            federation_executor_read(&connection, central_host_id, operation_key)?
        };
        let Some(record) = record else { return Ok(None); };
        let inner = self.inner.lock().expect("state mutex poisoned");
        let operation = &record.operation;
        let prompt_id = operation.queue_identity();
        let root = inner.find_session_index(&operation.session_id).map(|index| &inner.sessions[index]);
        let receipt = root.and_then(|root| root.prompt_deliveries.get(&prompt_id))
            .filter(|receipt| receipt.federation_operation.as_ref() == Some(operation))
            .map(|receipt| receipt.disposition);
        let queued = root.and_then(|root| root.queued_prompts.iter().enumerate()
            .find(|(_, entry)| entry.pending_prompt.id == prompt_id));
        let metadata = |entry: &QueuedPromptRecord| {
            let mailbox = entry.pending_prompt.source.as_ref().and_then(|source| source.mailbox.as_ref());
            FederationExecutorPendingPromptMetadata {
                prompt_id: entry.pending_prompt.id.clone(),
                timestamp: entry.pending_prompt.timestamp.clone(),
                source: entry.source,
                mailbox_id: mailbox.map(|mailbox| mailbox.mailbox_id.clone()),
                message_id: mailbox.map(|mailbox| mailbox.message_id.clone()),
                sequence: mailbox.map(|mailbox| mailbox.sequence),
            }
        };
        let pending_prompt = queued.map(|(_, entry)| metadata(entry));
        let pending_prompt_count = root.map_or(0, |root| root.queued_prompts.len());
        let pending_prompts = root.map(|root| root.queued_prompts.iter()
            .take(FEDERATION_EXECUTOR_VIEW_QUEUE_LIMIT).map(metadata).collect()).unwrap_or_default();
        let executor_local = inner.federation.local.as_ref()
            .is_some_and(|local| local.host_id == operation.target_host_id);
        Ok(Some(FederationExecutorOperationView {
            stage: record.stage.clone(),
            effect_state: record.effect_state.clone(),
            queue_entry_id: record.queue_entry_id.clone(),
            runtime_acknowledged: record.runtime_ack.is_some(),
            receipt,
            queue_position: queued.map(|(position, _)| position),
            pending_prompt,
            pending_prompts,
            pending_prompt_count,
            pending_prompts_truncated: pending_prompt_count > FEDERATION_EXECUTOR_VIEW_QUEUE_LIMIT,
            root_present: root.is_some(),
            executor_local,
            root_status: root.map(|root| root.session.status),
            root_paused: root.is_some_and(|root| root.orchestrator_auto_dispatch_blocked),
            root_stopping: root.is_some_and(|root| root.runtime_stop_in_progress),
            binding_current: federation_executor_target_locked(&inner, operation).is_ok(),
            operation: record.operation,
        }))
    }
}
