// Tests for the Telegram relay adapter in `src/telegram.rs`.
//
// In production the Telegram bot is an optional remote-control surface for
// TermAl: it pushes periodic project digests (headline, status, proposed
// actions, deep link) to a linked chat and accepts slash commands like
// `/commit`, `/stop`, or `/status` that are mapped onto `ProjectActionId`
// values and dispatched against the same HTTP action API used by the desktop
// UI. These tests pin two pieces of that adapter: `parse_telegram_command`
// (slash-command routing) and `render_telegram_digest` /
// `build_telegram_digest_keyboard` (outgoing digest formatting and inline
// keyboard). Everything else in `src/telegram.rs` is thin I/O glue around
// those pure functions.

use super::*;

// Pins that the slash-command parser strips Telegram's `@botname` mention
// suffix and preserves trailing free-form args. Without this, group-chat
// messages like `/commit@termal_bot now please` would be rejected as
// unknown commands and silently dropped, breaking the bot for any chat
// where it shares room with other bots.
#[test]
fn telegram_command_parser_supports_suffixes_and_aliases() {
    let parsed =
        parse_telegram_command("/commit@termal_bot   now please").expect("command should parse");
    assert_eq!(
        parsed.command,
        TelegramIncomingCommand::Action(ProjectActionId::AskAgentToCommit)
    );
    assert_eq!(parsed.args, "now please");

    let parsed = parse_telegram_command("/status").expect("status should parse");
    assert_eq!(parsed.command, TelegramIncomingCommand::Status);
}

// Pins that unknown slash commands return `None` rather than falling back
// to a default action. This guards against a regression where a typo such
// as `/commti` could be silently coerced into an action like
// `AskAgentToCommit`; the bot must instead reply with the help text so the
// user notices the mistake.
#[test]
fn telegram_command_parser_rejects_unknown_slash_commands() {
    assert!(parse_telegram_command("/unknown").is_none());
}

// Pins the outgoing digest shape: the rendered text exposes the project
// headline, a `Next: ...` line listing proposed action labels, and an
// `Open: ...` line resolving the relative deep link against the public
// base URL, while the inline keyboard emits one button per action with
// `callback_data` matching the `ProjectActionId` kebab-case. Without this
// the phone UI would either lose tap-to-act buttons or fire callbacks the
// server cannot parse, silently breaking remote control.
#[test]
fn telegram_digest_renderer_includes_actions_and_public_link() {
    let digest = ProjectDigestResponse {
        project_id: "project-1".to_owned(),
        headline: "termal".to_owned(),
        done_summary: "Updated the digest API.".to_owned(),
        current_status: "Changes are ready for review.".to_owned(),
        primary_session_id: Some("session-1".to_owned()),
        proposed_actions: vec![
            ProjectActionId::ReviewInTermal.into_digest_action(),
            ProjectActionId::AskAgentToCommit.into_digest_action(),
        ],
        deep_link: Some("/?projectId=project-1&sessionId=session-1".to_owned()),
        source_message_ids: vec!["message-1".to_owned()],
    };

    let rendered = render_telegram_digest(&digest, Some("https://termal.local"));
    assert!(rendered.contains("Project: termal"));
    assert!(rendered.contains("Next: Review in TermAl, Ask Agent to Commit"));
    assert!(
        rendered.contains("Open: https://termal.local/?projectId=project-1&sessionId=session-1")
    );

    let keyboard = build_telegram_digest_keyboard(&digest).expect("keyboard should exist");
    assert_eq!(keyboard.inline_keyboard.len(), 1);
    assert_eq!(
        keyboard.inline_keyboard[0][0].callback_data,
        "review-in-termal"
    );
    assert_eq!(
        keyboard.inline_keyboard[0][1].callback_data,
        "ask-agent-to-commit"
    );
}
