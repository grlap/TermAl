//! Shared delegation test helpers split out of `src/tests/delegations.rs`.
//!
//! This module owns small utilities used by focused delegation test modules.
//! It deliberately does not own assertions or scenario-specific setup; those
//! stay with the tests that describe the behavior being pinned.

use super::*;

pub(super) fn finish_delegation_child_with_assistant_text(
    state: &AppState,
    child_session_id: &str,
    text: &str,
) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let child_index = inner
        .find_session_index(child_session_id)
        .expect("child session should exist");
    let message_id = inner.next_message_id();
    let child = inner
        .session_mut_by_index(child_index)
        .expect("child session index should be valid");
    push_message_on_record(
        child,
        Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: text.to_owned(),
            expanded_text: None,
        },
    );
    child.session.status = SessionStatus::Idle;
    child.session.preview = text.lines().last().unwrap_or_default().to_owned();
    state.commit_locked(&mut inner).unwrap();
}

pub(super) fn test_app_state_with_delegation_codex_runtime(
    runtime_id: &str,
) -> (AppState, mpsc::Receiver<CodexRuntimeCommand>) {
    let state = super::test_app_state();
    let (runtime, input_rx, _process) = test_shared_codex_runtime(runtime_id);
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);
    (state, input_rx)
}

pub(super) fn test_app_state_with_drained_delegation_codex_runtime(runtime_id: &str) -> AppState {
    let (state, input_rx) = test_app_state_with_delegation_codex_runtime(runtime_id);
    std::thread::spawn(move || while input_rx.recv().is_ok() {});
    state
}

pub(super) fn temp_delegation_state_paths() -> (PathBuf, PathBuf, PathBuf) {
    let unique = Uuid::new_v4();
    let project_root = std::env::temp_dir().join(format!("termal-delegation-root-{unique}"));
    let state_root = std::env::temp_dir().join(format!("termal-delegation-state-{unique}"));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&state_root).expect("state root should exist");
    (
        project_root,
        state_root.join("termal.sqlite"),
        state_root.join("orchestrators.json"),
    )
}
pub(super) fn install_delegation_codex_runtime(state: &AppState, runtime_id: &str) {
    let (runtime, input_rx, _process) = test_shared_codex_runtime(runtime_id);
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);
    std::thread::spawn(move || while input_rx.recv().is_ok() {});
}
