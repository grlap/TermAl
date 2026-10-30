// Producer fixtures name native scopes explicitly. These are synthetic
// protocol envelopes, not claims that a host turn is a provider identity.
use super::*;

pub(super) fn test_private_command_key(recorder: &SessionRecorder, native_id: &str) -> String {
    let mut matches = recorder.recorder_state.command_messages.values()
        .filter_map(|entry| entry.invocation.as_ref())
        .filter(|entry| entry.native_id.as_deref() == Some(native_id));
    let key = matches.next().expect("the native creation has a retained reference").reference.key.clone();
    assert!(matches.next().is_none(), "the fixture must select an unambiguous native invocation");
    key
}

pub(super) fn test_native_command_key(recorder: &SessionRecorder, key: &str) -> String {
    recorder.recorder_state.command_messages.get(key)
        .and_then(|entry| entry.invocation.as_ref()).and_then(|entry| entry.native_id.as_ref())
        .cloned().unwrap_or_else(|| key.to_owned())
}

pub(super) fn test_command_scope(recorder: &mut SessionRecorder, producer: CommandProducer) {
    let runtime = {
        let inner = recorder.state.inner.lock().unwrap();
        inner.sessions.iter().find(|record| record.session.id == recorder.session_id)
            .and_then(|record| record.runtime.runtime_token())
    }.unwrap_or_else(|| match producer {
        CommandProducer::Acp => RuntimeToken::Acp("fixture-reader".to_owned()),
        CommandProducer::Claude => RuntimeToken::Claude("fixture-reader".to_owned()),
        CommandProducer::Codex => RuntimeToken::Codex("fixture-reader".to_owned()),
    });
    let turn = (producer == CommandProducer::Codex).then(|| "fixture-native-turn".to_owned());
    recorder.command_scope(CommandProducerScope {
        producer, runtime, session: "fixture-native-session".to_owned(),
        turn,
    });
}

pub(super) trait TestCommandScope: TurnRecorder {
    fn set_test_scope(&mut self);
}

impl TestCommandScope for SessionRecorder {
    fn set_test_scope(&mut self) { test_command_scope(self, CommandProducer::Codex); }
}

impl TestCommandScope for TestRecorder {
    fn set_test_scope(&mut self) {}
}

pub(super) fn test_acp_session_update(update: &Value, state: &AppState, session_id: &str,
    input_tx: &Sender<AcpRuntimeCommand>, turn: &mut AcpTurnState,
    recorder: &mut SessionRecorder, agent: AcpAgent) -> Result<()> {
    test_command_scope(recorder, CommandProducer::Acp);
    handle_acp_session_update(update, state, session_id, input_tx, turn, recorder, agent)
}

pub(super) fn test_codex_item_started(item: &Value, recorder: &mut impl TestCommandScope) -> Result<()> {
    recorder.set_test_scope();
    handle_codex_app_server_item_started(item, recorder)
}

pub(super) fn test_codex_item_completed(item: &Value, state: &AppState, session_id: &str,
    turn: &mut CodexTurnState, recorder: &mut impl TestCommandScope) -> Result<()> {
    recorder.set_test_scope();
    handle_codex_app_server_item_completed(item, state, session_id, turn, recorder)
}
