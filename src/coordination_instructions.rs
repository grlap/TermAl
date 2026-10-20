// Shared host coordination teaching, extracted from the Codex managed section.
// Owns protocol text and local-root eligibility, not mailbox operations or wakes.
// docs/features/agent-mailboxes.md is the prose contract mirrored here.
const TERMAL_MAILBOX_GUIDANCE: &str = r#"TermAl root coordination; not for delegation children.
Read: termal_read_mailbox; omit afterSequence, save receipt. Reading never acknowledges.
Process bodies in order; reply: termal_send_to_session, stable idempotencyKey; retry identical intent/key.
Ack: termal_acknowledge_mailbox with mailboxId and unchanged receipt after processing the whole page, even after sending. Gap: re-read.
Pages: hasMore/nextAfterSequence. Own sends are returned too. Receipt proves issuance only.
CLI: TERMAL_CLI (PowerShell: & $env:TERMAL_CLI; POSIX: "$TERMAL_CLI").
TERMAL_SESSION_ID / TERMAL_BASE_URL supply identity/URL; never impersonate.
mailbox read --mailbox-id <id> --json (omit --after)
mailbox send --to <id> --message <text> --idempotency-key <key> --json
mailbox acknowledge --mailbox-id <id> --receipt <receipt> --json
mailbox list is discovery only."#;

// A separately bounded addition keeps the original receipt protocol unchanged.
const TERMAL_MAILBOX_COVERAGE_GUIDANCE: &str = "ACK alone never suppresses a queued continuation. The host may cover it with an exact read and receipt ACK in the same later active turn, through its boundary, when nothing is unread. Set noContinuation (CLI --no-continuation) on a receipt acknowledgement only when you have fully handled every message through that receipt, sent every reply they need, and owe no work in a later turn. Never set it when you promised a follow-up, are waiting for a delegation, evaluator or gate result that will not arrive as mail, or have not read the whole page. It covers only that mailbox and receipt boundary; a later message, including a reply you await, re-arms the wake. Numeric ACK cannot carry it.";

fn render_termal_host_guidance() -> String {
    format!("TermAl host guidance\n{TERMAL_MAILBOX_GUIDANCE}\n{TERMAL_MAILBOX_COVERAGE_GUIDANCE}")
}

/// Check live ownership at the injection boundary, not a cached agent label.
/// Shared Codex homes also carry this text, whose first line excludes children.
fn termal_root_mailbox_guidance(state: &AppState, session_id: &str) -> Option<String> {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(session_id)?;
    let record = &inner.sessions[index];
    (!record.hidden && record.is_local_session() && record.session.parent_delegation_id.is_none())
        .then(render_termal_host_guidance)
}
