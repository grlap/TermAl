// Process-local command identity in the recorder's existing command map.
// Producers establish creation; the opaque key only retains that decision.
// Command recognition, authority, snapshots and results stay in EngramHost.

// Concurrent retention follows the 256-entry outstanding-work precedent in
// claude_outstanding_work.rs. Completed history never consumes active quota.
const COMMAND_INVOCATION_LIMIT: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
enum CommandProducer {
    Acp,
    Claude,
    Codex,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommandProducerScope {
    producer: CommandProducer,
    runtime: RuntimeToken,
    session: String,
    turn: Option<String>,
}

/// One actual coverage gap in the current producer owner, not identity history.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CommandCreationLoss {
    producer: CommandProducer,
    native_session_identity: String,
}

impl CommandProducerScope {
    fn loss_identity(&self) -> CommandCreationLoss {
        CommandCreationLoss { producer: self.producer.clone(), native_session_identity: sha256_hex(self.session.as_bytes()) }
    }
}

#[derive(Clone, Debug)]
struct CommandInvocation {
    key: String,
    provenance: EngramObservationProvenance,
}

#[derive(Clone, Debug)]
struct RecordedInvocation {
    scope: CommandProducerScope,
    native_id: Option<String>,
    reference: CommandInvocation,
    dispatched: bool,
    terminal: bool,
    turnless_safe: bool,
}

impl RecordedInvocation {
    fn shares_native_identity(&self, other: &Self) -> bool {
        self.scope.producer == other.scope.producer
            && self.scope.runtime == other.scope.runtime
            && self.scope.session == other.scope.session
            && self.native_id == other.native_id
    }
}

#[derive(Clone, Debug)]
struct RecordedCommand {
    message_id: String,
    invocation: Option<RecordedInvocation>,
}

impl From<String> for RecordedCommand {
    fn from(message_id: String) -> Self {
        Self { message_id, invocation: None }
    }
}

enum CommandInvocationEvent<'a> {
    Dispatch { command: &'a str, ran: Option<&'a str>, cwd: Option<&'a str> },
    Describe { ran: Option<&'a str>, cwd: Option<&'a str> },
    Finish {
        command: &'a str,
        reported_command: Option<&'a str>,
        output: &'a str,
        status: CommandStatus,
        exit: EngramCommandExit,
        runtime_output_cut: bool,
    },
    Abandon,
    /// A background task's actual terminal notification releases its slot;
    /// it supplies no command result or verification outcome.
    Release,
}

impl CommandInvocation {
    // Presentation-only recorders have no authority sink or retained proof.
    fn presentation(key: &str) -> Self {
        Self { key: key.to_owned(), provenance: EngramObservationProvenance::Ambient }
    }
}

fn recorder_invocation_slot<R: SessionRecorderAccess>(recorder: &mut R) -> bool {
    let count = recorder.recorder_state_mut().command_messages.values()
        .filter(|entry| entry.invocation.is_some()).count();
    if count < COMMAND_INVOCATION_LIMIT { return true; }
    let retired = recorder.recorder_state_mut().command_messages.iter()
        .find(|(_, entry)| entry.invocation.as_ref().is_some_and(|entry| entry.terminal))
        .map(|(key, _)| key.clone());
    let Some(retired) = retired else { return false; };
    if let Some(retired) = recorder.recorder_state_mut().command_messages.remove(&retired)
        .and_then(|entry| entry.invocation)
    {
        for entry in recorder.recorder_state_mut().command_messages.values_mut().filter_map(|entry| entry.invocation.as_mut()) {
            if entry.shares_native_identity(&retired) { entry.turnless_safe = false; }
        }
    }
    true
}

fn recorder_invocation_create<R: SessionRecorderAccess>(
    recorder: &mut R,
    native_id: Option<&str>,
    display_key: &str,
) -> CommandInvocation {
    let mut scope = recorder.recorder_state_mut().command_scope.clone();
    // Claude's parser may learn the native session in this very frame, after
    // the router fixed its runtime/turn provenance but before tool creation.
    if let Some(current) = scope.as_mut().filter(|scope| scope.producer == CommandProducer::Claude) {
        let native = recorder.state().inner.lock().expect("state mutex poisoned")
            .sessions.iter().find(|record| record.session.id == recorder.session_id()
                && record.runtime.matches_runtime_token(&current.runtime))
            .and_then(|record| record.external_session_id.clone());
        if let Some(native) = native && current.session != native {
            current.session = native;
            recorder_command_scope(recorder, current.clone());
        }
    }
    let Some(scope) = scope else {
        return recorder_unproven_invocation(recorder, display_key, "the producer creation scope is unavailable");
    };
    let existing = recorder.recorder_state_mut().command_messages.values()
        .filter_map(|entry| entry.invocation.as_ref())
        .find(|entry| entry.scope == scope && entry.native_id.as_deref() == native_id)
        .cloned();
    if let Some(existing) = existing {
        if native_id.is_none() {
            recorder.state().engram_host().command_identity_unavailable(recorder.session_id(),
                "an id-less creation in this native turn cannot distinguish another invocation from a repeated frame; no additional qualifying start is admitted");
        }
        if matches!(existing.reference.provenance, EngramObservationProvenance::Unproven) {
            recorder.state().engram_host().command_identity_unavailable(recorder.session_id(),
                "this retained identity was first observed without its creation; a later frame cannot promote that original to a qualifying invocation");
        }
        return existing.reference;
    }
    if recorder.recorder_state_mut().command_creation_coverage_lost {
        return recorder_unproven_invocation(recorder, display_key,
            "this producer's creation coverage was lost at concurrent capacity; existing references remain valid; a genuinely new native session is the recovery boundary; same-session resume does not repair this known loss");
    }
    // Only terminal producer boundaries release slots. A turn reset,
    // checkpoint Unknown or artifact settlement cannot evict active proof.
    if !recorder_invocation_slot(recorder) {
        recorder.recorder_state_mut().command_creation_coverage_lost = true;
        recorder.state().engram_host().command_creation_coverage_lost(recorder.session_id(), &scope);
        return recorder_unproven_invocation(recorder, display_key,
            "the producer's unresolved command identity capacity is full; this invocation cannot qualify; existing references remain valid; creation coverage recovers with a genuinely new native session; same-session resume does not repair this known loss");
    }
    let session_available = !scope.session.is_empty();
    let qualifying = session_available && match scope.producer {
        CommandProducer::Acp | CommandProducer::Claude => native_id.is_some(),
        CommandProducer::Codex => native_id.is_some() || scope.turn.is_some(),
    };
    let provenance = recorder_invocation_provenance(recorder, &scope, qualifying);
    let prefix = if scope.producer == CommandProducer::Codex && native_id.is_none() {
        "invocation-foreground"
    } else { "invocation" };
    let reference = CommandInvocation { key: format!("{prefix}-{}", Uuid::new_v4()), provenance };
    let mut turnless_safe = true;
    for entry in recorder.recorder_state_mut().command_messages.values_mut().filter_map(|entry| entry.invocation.as_mut()) {
        if entry.scope.producer == scope.producer && entry.scope.runtime == scope.runtime && entry.scope.session == scope.session
            && entry.native_id.as_deref() == native_id
        {
            entry.turnless_safe = false;
            turnless_safe = false;
        }
    }
    let message_id = recorder.state().allocate_message_id();
    recorder.recorder_state_mut().command_messages.insert(reference.key.clone(), RecordedCommand {
        message_id,
        invocation: Some(RecordedInvocation {
            scope, native_id: native_id.map(str::to_owned),
            reference: reference.clone(), dispatched: false, terminal: false, turnless_safe,
        }),
    });
    if let Some(native_id) = native_id {
        recorder.state().engram_host().bind_claude_command_invocation(recorder.session_id(), native_id, &reference);
    }
    if !qualifying {
        recorder.state().engram_host().command_identity_unavailable(recorder.session_id(),
            if session_available { "command creation has neither a native invocation identity nor a native event turn; display and writer tracking continue without verification eligibility" }
            else { "the native producer session scope is unavailable; display and writer tracking continue without verification eligibility" });
    }
    reference
}

fn recorder_invocation_provenance<R: SessionRecorderAccess>(
    recorder: &mut R,
    scope: &CommandProducerScope,
    qualifying: bool,
) -> EngramObservationProvenance {
    let current = recorder.recorder_state_mut().observation_provenance.clone();
    if matches!(current, EngramObservationProvenance::Claude(_)) {
        return current;
    }
    let facts = recorder.state().inner.lock().expect("state mutex poisoned")
        .sessions.iter().find(|record| record.session.id == recorder.session_id())
        .map(|record| (record.active_turn_generation, record.engram.work_binding.clone(),
            record.engram.active_grant_id.clone(), record.engram.active_turn_source_root.as_ref().map(|root| root.generation)));
    let (generation, binding, grant, source_generation) = match facts {
        Some((generation, binding, grant, source_generation)) => (Some(generation), binding, grant, source_generation),
        None => (None, None, None, None),
    };
    EngramObservationProvenance::Producer {
        runtime: scope.runtime.clone(), turn_generation: generation, qualifying, binding, grant, source_generation,
    }
}

fn recorder_unproven_invocation<R: SessionRecorderAccess>(recorder: &mut R, display_key: &str, reason: &str) -> CommandInvocation {
    recorder.state().engram_host().command_identity_unavailable(recorder.session_id(), reason);
    let scope = recorder.recorder_state_mut().command_scope.as_ref();
    let presentation = sha256_hex(format!("{scope:?}|{display_key}").as_bytes());
    // This deterministic presentation/writer key groups uncertain frames but
    // never names a proven invocation or supplies creation/authority evidence.
    CommandInvocation {
        key: format!("unproven-{presentation}"),
        provenance: EngramObservationProvenance::Unproven,
    }
}

fn recorder_invocation_find<R: SessionRecorderAccess>(
    recorder: &mut R,
    native_id: Option<&str>,
    _display_key: &str,
) -> Option<CommandInvocation> {
    let scope = recorder.recorder_state_mut().command_scope.clone()?;
    let candidates: Vec<_> = recorder.recorder_state_mut().command_messages.values()
        .filter_map(|entry| entry.invocation.as_ref())
        .filter(|entry| entry.scope.producer == scope.producer
            && entry.scope.runtime == scope.runtime && entry.scope.session == scope.session
            && entry.native_id.as_deref() == native_id).cloned().collect();
    if scope.turn.is_some() || scope.producer != CommandProducer::Codex || native_id.is_none() {
        let mut exact = candidates.iter().filter(|entry| entry.scope.turn == scope.turn);
        if let Some(candidate) = exact.next() {
            return exact.next().is_none().then(|| candidate.reference.clone());
        }
    }
    // Codex may tag an old item's end with the visible turn or omit the turn.
    // This fallback requires complete retained coverage of its explicit id,
    // never merely one remaining live row after a known reuse/loss.
    if scope.producer == CommandProducer::Codex && native_id.is_some()
        && candidates.len() == 1 && candidates[0].turnless_safe
    { Some(candidates[0].reference.clone()) } else { None }
}

fn recorder_invocation_missing<R: SessionRecorderAccess>(recorder: &mut R,
    native_id: Option<&str>, display_key: &str, terminal: bool, reason: &str) -> CommandInvocation {
    let reference = recorder_unproven_invocation(recorder, display_key, reason);
    if let Some(scope) = recorder.recorder_state_mut().command_scope.clone() {
        if recorder_invocation_slot(recorder) {
            let message_id = recorder.state().allocate_message_id();
            recorder.recorder_state_mut().command_messages.entry(reference.key.clone()).or_insert(RecordedCommand {
                message_id,
                invocation: Some(RecordedInvocation { scope, native_id: native_id.map(str::to_owned),
                    reference: reference.clone(), dispatched: false, terminal, turnless_safe: false }),
            });
        } else if !terminal {
            recorder.recorder_state_mut().command_creation_coverage_lost = true;
            recorder.state().engram_host().command_creation_coverage_lost(recorder.session_id(), &scope);
        }
    }
    reference
}

fn recorder_invocation_event<R: SessionRecorderAccess>(
    recorder: &mut R,
    reference: &CommandInvocation,
    event: CommandInvocationEvent<'_>,
) -> Result<()> {
    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    let previous = recorder.recorder_state_mut().observation_provenance.clone();
    let retained = recorder.recorder_state_mut().command_messages.get(&reference.key)
        .and_then(|entry| entry.invocation.as_ref()).cloned();
    let mut provenance = retained.as_ref().map(|entry| entry.reference.provenance.clone())
        .unwrap_or_else(|| reference.provenance.clone());
    if matches!(event, CommandInvocationEvent::Dispatch { .. })
        && let Some(entry) = retained.as_ref().filter(|entry| !entry.dispatched && !entry.terminal
            && !matches!(entry.reference.provenance, EngramObservationProvenance::Unproven))
    {
        provenance = recorder_invocation_provenance(recorder, &entry.scope,
            !matches!(entry.reference.provenance, EngramObservationProvenance::Producer { qualifying: false, .. }));
        if let Some(retained) = recorder.recorder_state_mut().command_messages.get_mut(&reference.key)
            .and_then(|entry| entry.invocation.as_mut()) { retained.reference.provenance = provenance.clone(); }
    }
    recorder.recorder_state_mut().observation_provenance = provenance.clone();
    let result = match event {
        CommandInvocationEvent::Dispatch { command, ran, cwd } => {
            let existing = recorder.recorder_state_mut().command_messages.get_mut(&reference.key)
                .and_then(|entry| entry.invocation.as_mut());
            let begin = existing.is_some_and(|entry| {
                if entry.dispatched || entry.terminal { false } else { entry.dispatched = true; true }
            });
            if begin || retained.as_ref().is_some_and(|entry| entry.dispatched && !entry.terminal)
                || matches!(reference.provenance, EngramObservationProvenance::Unproven)
            {
                recorder_command_started(recorder, &reference.key, command, ran, cwd)
            } else {
                // Re-observation may invalidate a description, but never starts
                // a new check under later binding, grant or host turn.
                recorder.observe(EngramRecorderObservation::CommandDescribed { key: &reference.key, ran, cwd });
                Ok(())
            }
        }
        CommandInvocationEvent::Describe { ran, cwd } => {
            recorder.observe(EngramRecorderObservation::CommandDescribed { key: &reference.key, ran, cwd });
            Ok(())
        }
        CommandInvocationEvent::Finish { command, reported_command, output, status, exit, runtime_output_cut } => {
            if status != CommandStatus::Running && exit != EngramCommandExit::NotFinished
                && let Some(entry) = recorder.recorder_state_mut().command_messages.get_mut(&reference.key)
                    .and_then(|entry| entry.invocation.as_mut())
            {
                entry.terminal = true;
            }
            recorder_command_completed_reported(recorder, &reference.key, command, reported_command,
                output, status, Some(exit), runtime_output_cut)
        }
        CommandInvocationEvent::Abandon => {
            if let Some(entry) = recorder.recorder_state_mut().command_messages.get_mut(&reference.key)
                .and_then(|entry| entry.invocation.as_mut())
            {
                entry.terminal = true;
            }
            state.engram_host().observe(&session_id, &provenance,
                EngramRecorderObservation::CommandAbandoned { key: &reference.key });
            Ok(())
        }
        CommandInvocationEvent::Release => {
            if let Some(entry) = recorder.recorder_state_mut().command_messages.get_mut(&reference.key)
                .and_then(|entry| entry.invocation.as_mut()) { entry.terminal = true; }
            Ok(())
        }
    };
    recorder.recorder_state_mut().observation_provenance = previous;
    result
}

fn recorder_command_scope<R: SessionRecorderAccess>(recorder: &mut R, scope: CommandProducerScope) {
    let previous = recorder.recorder_state_mut().command_scope.as_ref();
    let replaced = previous.is_some_and(|previous| previous.runtime != scope.runtime
        || previous.producer != scope.producer || previous.session != scope.session);
    if replaced {
        // Old handles remain immutable with their callers. Dropping the map
        // cannot make one of those handles address this runtime's new start.
        recorder.recorder_state_mut().command_messages.retain(|_, entry| entry.invocation.is_none());
    }
    let known_loss = recorder.state().engram_host().command_scope_has_creation_loss(recorder.session_id(), &scope);
    let retained_loss = !replaced && recorder.recorder_state_mut().command_creation_coverage_lost;
    recorder.recorder_state_mut().command_creation_coverage_lost = known_loss || retained_loss;
    recorder.recorder_state_mut().command_scope = Some(scope);
}

fn recorder_invocation_carried_end<R: SessionRecorderAccess>(recorder: &mut R, reference: &CommandInvocation,
    command: &str, reported_command: Option<&str>, output: &str, exit: EngramCommandExit) -> bool {
    let consumed = recorder.state().engram_host().codex_invocation_reported_end(recorder.session_id(),
        reference, command, reported_command, output, exit);
    if exit != EngramCommandExit::NotFinished
        && let Some(entry) = recorder.recorder_state_mut().command_messages.get_mut(&reference.key)
            .and_then(|entry| entry.invocation.as_mut())
    {
        entry.terminal = true;
    }
    consumed
}
