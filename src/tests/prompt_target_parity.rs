// Owns the parity tests for the two selectors that choose a project's
// automatic prompt target: the project digest's
// `latest_project_prompt_target_session` (api.rs), over the digest's
// project-scoped session list, and Telegram's
// `find_latest_telegram_project_prompt_session` (telegram_digest.rs), over
// the `/api/state` projection. One table of cases is projected into both
// inputs; both selectors must name the same session, and that session must
// be the case's expected one.
//
// The real differences between the two projections are kept, and each is
// tested on its own rather than smoothed over:
// - Telegram's status has an `Unknown` variant, its serde fallback for a
//   status it does not know. It counts as a prompt target and has no digest
//   counterpart.
// - Telegram filters by project itself. The digest's selector does not: its
//   input is already the project's own sessions.
//
// Status coverage: the matrix runs every `SessionStatus` variant (Active,
// Idle, Approval, Stopping, Error) on both sides as the newest session, and
// the difference test runs the one variant only Telegram has, `Unknown`.
//
// Does not own either selector, which this module does not change, or the
// rest of the digest and Telegram behaviour (project_digest.rs and the
// telegram tests). New file.
use super::*;

const PROJECT: &str = "project-parity";
const OTHER: &str = "project-other";

/// A status both projections share.
#[derive(Clone, Copy)]
enum Status {
    Active,
    Idle,
    Approval,
    Stopping,
    Error,
}

impl Status {
    fn digest(self) -> SessionStatus {
        match self {
            Status::Active => SessionStatus::Active,
            Status::Idle => SessionStatus::Idle,
            Status::Approval => SessionStatus::Approval,
            Status::Stopping => SessionStatus::Stopping,
            Status::Error => SessionStatus::Error,
        }
    }

    fn telegram(self) -> TelegramSessionStatus {
        match self {
            Status::Active => TelegramSessionStatus::Active,
            Status::Idle => TelegramSessionStatus::Idle,
            Status::Approval => TelegramSessionStatus::Approval,
            Status::Stopping => TelegramSessionStatus::Stopping,
            Status::Error => TelegramSessionStatus::Error,
        }
    }
}

/// One session, described once for both projections, oldest first.
struct Row {
    id: &'static str,
    project: &'static str,
    child: bool,
    status: Status,
}

fn row(id: &'static str, project: &'static str, child: bool, status: Status) -> Row {
    Row {
        id,
        project,
        child,
        status,
    }
}

fn digest_session(row: &Row) -> ProjectDigestSession {
    ProjectDigestSession {
        id: row.id.to_owned(),
        parent_delegation_id: row.child.then(|| "delegation-parent".to_owned()),
        status: row.status.digest(),
        preview: String::new(),
        pending_prompt_count: 0,
        has_messages: false,
        latest_progress_summary: None,
        pending_approval_message_id: None,
        pending_interaction_message_id: None,
    }
}

fn telegram_session(
    id: &str,
    project: &str,
    child: bool,
    status: TelegramSessionStatus,
) -> TelegramStateSession {
    TelegramStateSession {
        id: id.to_owned(),
        name: id.to_owned(),
        project_id: Some(project.to_owned()),
        status,
        message_count: 0,
        session_mutation_stamp: None,
        parent_delegation_id: child.then(|| "delegation-parent".to_owned()),
    }
}

/// The digest's input: the project's own sessions, in order.
fn digest_input(rows: &[Row]) -> Vec<ProjectDigestSession> {
    rows.iter()
        .filter(|row| row.project == PROJECT)
        .map(digest_session)
        .collect()
}

/// Telegram's input: every project's sessions, in order.
fn telegram_input(rows: &[Row]) -> TelegramStateSessionsResponse {
    TelegramStateSessionsResponse {
        projects: Vec::new(),
        sessions: rows
            .iter()
            .map(|row| telegram_session(row.id, row.project, row.child, row.status.telegram()))
            .collect(),
    }
}

fn digest_target(sessions: &[ProjectDigestSession]) -> Option<&str> {
    latest_project_prompt_target_session(sessions).map(|session| session.id.as_str())
}

fn telegram_target(state: &TelegramStateSessionsResponse) -> Option<&str> {
    find_latest_telegram_project_prompt_session(state, PROJECT).map(|session| session.id.as_str())
}

#[test]
fn both_prompt_target_selectors_agree_with_the_expected_target() {
    let cases: Vec<(&str, Vec<Row>, Option<&str>)> = vec![
        ("empty input", vec![], None),
        // Each shared status as the newest session.
        (
            "newest active",
            vec![
                row("older", PROJECT, false, Status::Idle),
                row("newest", PROJECT, false, Status::Active),
            ],
            Some("newest"),
        ),
        (
            "newest idle",
            vec![
                row("older", PROJECT, false, Status::Active),
                row("newest", PROJECT, false, Status::Idle),
            ],
            Some("newest"),
        ),
        (
            "newest approval",
            vec![
                row("older", PROJECT, false, Status::Idle),
                row("newest", PROJECT, false, Status::Approval),
            ],
            Some("newest"),
        ),
        (
            "newest stopping",
            vec![
                row("older", PROJECT, false, Status::Idle),
                row("newest", PROJECT, false, Status::Stopping),
            ],
            Some("newest"),
        ),
        (
            "newest errored is skipped",
            vec![
                row("older", PROJECT, false, Status::Idle),
                row("newest", PROJECT, false, Status::Error),
            ],
            Some("older"),
        ),
        // Delegated children are never a target.
        (
            "delegated child newer than an eligible parent",
            vec![
                row("parent", PROJECT, false, Status::Idle),
                row("child", PROJECT, true, Status::Active),
            ],
            Some("parent"),
        ),
        (
            "only delegated children",
            vec![
                row("child-1", PROJECT, true, Status::Active),
                row("child-2", PROJECT, true, Status::Idle),
            ],
            None,
        ),
        (
            "only errored sessions",
            vec![
                row("error-1", PROJECT, false, Status::Error),
                row("error-2", PROJECT, false, Status::Error),
            ],
            None,
        ),
        // Ordering: the newest eligible session wins.
        (
            "newest eligible wins over older eligible and newer ineligible",
            vec![
                row("oldest", PROJECT, false, Status::Active),
                row("eligible", PROJECT, false, Status::Idle),
                row("child", PROJECT, true, Status::Active),
                row("errored", PROJECT, false, Status::Error),
            ],
            Some("eligible"),
        ),
        // Project filtering: another project's sessions are never chosen.
        (
            "another project interleaved and newest overall",
            vec![
                row("other-old", OTHER, false, Status::Active),
                row("mine", PROJECT, false, Status::Idle),
                row("other-new", OTHER, false, Status::Active),
            ],
            Some("mine"),
        ),
        (
            "only another project's sessions",
            vec![row("other", OTHER, false, Status::Active)],
            None,
        ),
    ];
    for (name, rows, expected) in &cases {
        let digest = digest_input(rows);
        let telegram = telegram_input(rows);
        assert_eq!(
            digest_target(&digest),
            telegram_target(&telegram),
            "{name}: the selectors disagree"
        );
        assert_eq!(digest_target(&digest), *expected, "{name}");
    }
}

#[test]
fn the_projection_differences_between_the_selectors_are_kept() {
    // Telegram's `Unknown` status, which the digest has no counterpart for,
    // is a prompt target.
    let state = TelegramStateSessionsResponse {
        projects: Vec::new(),
        sessions: vec![
            telegram_session("older", PROJECT, false, TelegramSessionStatus::Idle),
            telegram_session("unknown", PROJECT, false, TelegramSessionStatus::Unknown),
        ],
    };
    assert_eq!(telegram_target(&state), Some("unknown"));

    // Telegram filters by project itself; the digest's selector does not, so
    // given every project's sessions it would pick another project's. Its
    // real input is already scoped to the project.
    let rows = [
        row("mine", PROJECT, false, Status::Idle),
        row("theirs", OTHER, false, Status::Active),
    ];
    assert_eq!(telegram_target(&telegram_input(&rows)), Some("mine"));
    let unscoped = rows.iter().map(digest_session).collect::<Vec<_>>();
    assert_eq!(digest_target(&unscoped), Some("theirs"));
    assert_eq!(digest_target(&digest_input(&rows)), Some("mine"));
}
