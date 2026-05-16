// Codex thread discovery and import. The Codex CLI persists each conversation
// thread in a SQLite database under the user's Codex home (`state.db`, or a
// versioned `state_<n>.sqlite` fallback after migrations). TermAl's discovery
// feature scans those databases, filters threads whose `cwd` matches the
// user's workdir, and imports them as local sessions so first-time users in
// an existing workdir do not lose prior Codex history. Two home layouts
// coexist: the legacy REPL install (`repl/`) and the current default
// app-server install (`shared-app-server/`); TermAl prefers the shared-runtime
// home and skips `repl/` entirely. Scope filtering runs BEFORE the per-home
// row limit so a workdir's threads are never crowded out by unrelated rows.
// Newer Codex schemas add optional columns (model, reasoning_effort) and the
// query relies on them; legacy schemas surface a clear `no such column`
// error instead of silently empty results. Import dedups by thread id (no
// clones on refresh) and normalizes legacy `cwd` forms (Windows `\\?\`
// verbatim, `~`, mixed separators) to the canonical project workdir.
// Surfaces: `discover_codex_threads_from_home`,
// `discover_codex_threads_from_sources`, `resolve_codex_threads_database_path`,
// `StateInner::import_discovered_codex_threads`.

use super::*;

// pins that `resolve_codex_threads_database_path` picks the highest-versioned
// `state_<n>.sqlite` when `state.db` is absent/empty and that every row column
// (sandbox policy, approval mode, archived flag, model, reasoning effort)
// round-trips into a `DiscoveredCodexThread`. guards against silently reading
// a stale database or dropping optional fields during discovery.
#[test]
fn discover_codex_threads_from_home_reads_latest_database() {
    let codex_home = std::env::temp_dir().join(format!("termal-codex-home-{}", Uuid::new_v4()));
    fs::write(codex_home.join("state.db"), b"").unwrap_or_default();
    write_test_codex_threads_db(
        &codex_home,
        &[(
            "thread-1",
            "/tmp/project",
            "Review local repo",
            r#"{"type":"danger-full-access"}"#,
            "on-request",
            1,
            Some("gpt-5-codex"),
            Some("high"),
            10,
        )],
    );

    let threads = discover_codex_threads_from_home(&codex_home, &[PathBuf::from("/tmp/project")])
        .expect("threads should load");

    assert_eq!(
        threads,
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::OnRequest),
            archived: true,
            cwd: "/tmp/project".to_owned(),
            id: "thread-1".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::High),
            sandbox_mode: Some(CodexSandboxMode::DangerFullAccess),
            title: "Review local repo".to_owned(),
        }]
    );

    let _ = fs::remove_dir_all(&codex_home);
}

// pins that Codex thread discovery treats `model` and `reasoning_effort` as
// optional columns. Older Codex installs can lack them; discovery should still
// import the thread metadata it can read instead of failing startup discovery.
#[test]
fn discover_codex_threads_from_home_tolerates_missing_optional_columns() {
    let codex_home =
        std::env::temp_dir().join(format!("termal-codex-home-legacy-{}", Uuid::new_v4()));
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    let connection =
        rusqlite::Connection::open(codex_home.join("state_5.sqlite")).expect("db should open");
    connection
        .execute_batch(
            "create table threads (
                id text primary key,
                cwd text not null,
                title text not null,
                sandbox_policy text,
                approval_mode text,
                archived integer not null,
                updated_at integer not null
            );",
        )
        .expect("legacy threads table should be created");
    connection
        .execute(
            "insert into threads (
                id, cwd, title, sandbox_policy, approval_mode, archived, updated_at
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                "thread-legacy",
                "/tmp/project",
                "Legacy thread",
                r#"{"type":"workspace-write"}"#,
                "on-request",
                0,
                10,
            ],
        )
        .expect("legacy thread row should insert");

    let threads = discover_codex_threads_from_home(&codex_home, &[PathBuf::from("/tmp/project")])
        .expect("legacy threads schema should load");
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].id, "thread-legacy");
    assert_eq!(threads[0].model, None);
    assert_eq!(threads[0].reasoning_effort, None);

    let _ = fs::remove_dir_all(&codex_home);
}

// pins that `resolve_codex_threads_database_path` only matches filenames of
// the form `state_<numeric>.sqlite`, ignoring non-versioned sqlite files like
// `state_preview.sqlite`. guards against discovery accidentally opening an
// unrelated Codex sqlite artifact and failing with a schema error.
#[test]
fn resolve_codex_threads_database_path_skips_unrelated_entries() {
    let codex_home =
        std::env::temp_dir().join(format!("termal-codex-home-scan-{}", Uuid::new_v4()));
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    fs::write(codex_home.join("state_9.sqlite"), b"sqlite").expect("valid state db should exist");
    fs::write(codex_home.join("state_preview.sqlite"), b"broken")
        .expect("unrelated sqlite file should be created");

    let path = resolve_codex_threads_database_path(&codex_home)
        .expect("database discovery should skip unrelated entries");

    assert_eq!(
        path.file_name().and_then(|value| value.to_str()),
        Some("state_9.sqlite")
    );

    let _ = fs::remove_dir_all(&codex_home);
}

// pins the candidate-home priority: the `shared-app-server` home wins over
// the legacy Codex `~/.codex` home when both hold the same thread id, and the
// `repl` home is skipped entirely. guards against TermAl importing stale REPL
// history or preferring the source home when the shared-runtime install is
// the active Codex backend.
#[test]
fn discover_codex_threads_from_sources_skips_repl_home_and_uses_shared_runtime_home() {
    let root = std::env::temp_dir().join(format!("termal-codex-discovery-{}", Uuid::new_v4()));
    let source_home = root.join(".codex");
    let termal_root = root.join(".termal").join("codex-home");
    let shared_home = termal_root.join("shared-app-server");
    let repl_home = termal_root.join("repl");

    write_test_codex_threads_db(
        &shared_home,
        &[(
            "thread-shared",
            "/tmp/project-shared",
            "Shared runtime thread",
            r#"{"type":"workspace-write"}"#,
            "on-request",
            0,
            Some("gpt-5-codex"),
            Some("medium"),
            30,
        )],
    );
    write_test_codex_threads_db(
        &repl_home,
        &[(
            "thread-repl",
            "/tmp/project-repl",
            "REPL thread",
            r#"{"type":"read-only"}"#,
            "never",
            0,
            Some("gpt-5-mini"),
            Some("low"),
            20,
        )],
    );
    write_test_codex_threads_db(
        &source_home,
        &[
            (
                "thread-shared",
                "/tmp/project-source",
                "Older source copy",
                r#"{"type":"danger-full-access"}"#,
                "never",
                1,
                Some("gpt-5"),
                Some("high"),
                10,
            ),
            (
                "thread-source",
                "/tmp/project-source-only",
                "Source-only thread",
                r#"{"type":"workspace-write"}"#,
                "on-failure",
                0,
                Some("gpt-5-codex"),
                Some("minimal"),
                5,
            ),
        ],
    );

    let threads = discover_codex_threads_from_sources(
        Some(&source_home),
        &termal_root,
        &[
            PathBuf::from("/tmp/project-shared"),
            PathBuf::from("/tmp/project-source-only"),
        ],
    )
    .expect("threads should load");

    assert_eq!(
        threads
            .iter()
            .map(|thread| thread.id.as_str())
            .collect::<Vec<_>>(),
        vec!["thread-shared", "thread-source"]
    );
    assert!(matches!(
        threads.first(),
        Some(DiscoveredCodexThread {
            title,
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            ..
        }) if title == "Shared runtime thread"
    ));
    assert!(threads.iter().all(|thread| thread.id != "thread-repl"));

    let _ = fs::remove_dir_all(&root);
}

// pins the scope-before-limit ordering: with 101 unrelated threads plus one
// in-scope thread at the end of the table, the single in-scope thread is
// still returned. guards against a SQL regression that limits rows before
// filtering by `cwd` and would silently drop the user's real history when
// a Codex DB has accumulated many out-of-scope conversations.
#[test]
fn discover_codex_threads_from_home_filters_scopes_before_limiting_results() {
    let codex_home =
        std::env::temp_dir().join(format!("termal-codex-home-large-{}", Uuid::new_v4()));
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    let connection =
        rusqlite::Connection::open(codex_home.join("state_5.sqlite")).expect("db should open");
    connection
        .execute_batch(
            "create table threads (
                id text primary key,
                cwd text not null,
                title text not null,
                sandbox_policy text not null,
                approval_mode text not null,
                archived integer not null,
                model text,
                reasoning_effort text,
                updated_at integer not null
            );",
        )
        .expect("threads table should be created");

    for index in 0..101 {
        connection
            .execute(
                "insert into threads (
                    id, cwd, title, sandbox_policy, approval_mode, archived, model, reasoning_effort, updated_at
                ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    format!("thread-other-{index}"),
                    "/tmp/out-of-scope",
                    format!("Out-of-scope thread {index}"),
                    r#"{"type":"workspace-write"}"#,
                    "never",
                    0,
                    "gpt-5-codex",
                    "low",
                    1_000 - index,
                ],
            )
            .expect("thread row should insert");
    }
    connection
        .execute(
            "insert into threads (
                id, cwd, title, sandbox_policy, approval_mode, archived, model, reasoning_effort, updated_at
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "thread-target",
                "/tmp/termal",
                "Older in-scope thread",
                r#"{"type":"danger-full-access"}"#,
                "on-request",
                0,
                "gpt-5-codex",
                "medium",
                1,
            ],
        )
        .expect("target row should insert");

    let threads = discover_codex_threads_from_home(&codex_home, &[PathBuf::from("/tmp/termal")])
        .expect("threads should load");

    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].id, "thread-target");

    let _ = fs::remove_dir_all(&codex_home);
}

// pins the per-home cap at `MAX_DISCOVERED_CODEX_THREADS_PER_HOME` and the
// `updated_at desc` ordering: with `cap + 25` in-scope rows, discovery
// returns exactly `cap` items starting at the newest. guards against
// unbounded memory use when a long-lived Codex install has thousands of
// threads in a single workdir.
#[test]
fn discover_codex_threads_from_home_limits_in_scope_results_per_home() {
    let codex_home =
        std::env::temp_dir().join(format!("termal-codex-home-limited-{}", Uuid::new_v4()));
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    let connection =
        rusqlite::Connection::open(codex_home.join("state_7.sqlite")).expect("db should open");
    connection
        .execute_batch(
            "create table threads (
                id text primary key,
                cwd text not null,
                title text not null,
                sandbox_policy text not null,
                approval_mode text not null,
                archived integer not null,
                model text,
                reasoning_effort text,
                updated_at integer not null
            );",
        )
        .expect("threads table should be created");

    for index in 0..(MAX_DISCOVERED_CODEX_THREADS_PER_HOME + 25) {
        connection
            .execute(
                "insert into threads (
                    id, cwd, title, sandbox_policy, approval_mode, archived, model, reasoning_effort, updated_at
                ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    format!("thread-in-scope-{index}"),
                    "/tmp/termal/subdir",
                    format!("In-scope thread {index}"),
                    r#"{"type":"workspace-write"}"#,
                    "on-request",
                    0,
                    "gpt-5-codex",
                    "medium",
                    10_000 - index as i64,
                ],
            )
            .expect("thread row should insert");
    }

    let threads = discover_codex_threads_from_home(&codex_home, &[PathBuf::from("/tmp/termal")])
        .expect("threads should load");
    let last_expected_id = format!(
        "thread-in-scope-{}",
        MAX_DISCOVERED_CODEX_THREADS_PER_HOME - 1
    );

    assert_eq!(threads.len(), MAX_DISCOVERED_CODEX_THREADS_PER_HOME);
    assert_eq!(
        threads.first().map(|thread| thread.id.as_str()),
        Some("thread-in-scope-0")
    );
    assert_eq!(
        threads.last().map(|thread| thread.id.as_str()),
        Some(last_expected_id.as_str()),
    );

    let _ = fs::remove_dir_all(&codex_home);
}

// pins that import creates a Codex session attached to the correct project
// with model, sandbox, approval, reasoning effort, and archived thread-state
// all copied from the discovered row, skips threads whose `cwd` is outside
// the workdir, and is idempotent when called twice with the same input.
// guards against duplicate sessions on repeated discovery refreshes.
#[test]
fn import_discovered_codex_threads_adds_project_scoped_sessions_without_duplicates() {
    let mut inner = StateInner::new();
    let project = inner.create_project(
        Some("TermAl".to_owned()),
        "/tmp/termal".to_owned(),
        default_local_remote_id(),
    );
    inner.create_session(
        Agent::Codex,
        Some("Codex Live".to_owned()),
        "/tmp/termal".to_owned(),
        Some(project.id.clone()),
        None,
    );

    let discovered = vec![
        DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: true,
            cwd: "/tmp/termal".to_owned(),
            id: "thread-local".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::Low),
            sandbox_mode: Some(CodexSandboxMode::DangerFullAccess),
            title: "Read bugs".to_owned(),
        },
        DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: false,
            cwd: "/tmp/elsewhere".to_owned(),
            id: "thread-other".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: None,
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            title: "Ignore me".to_owned(),
        },
    ];

    inner.import_discovered_codex_threads("/tmp/termal", discovered.clone());
    inner.import_discovered_codex_threads("/tmp/termal", discovered);

    let discovered_session = inner
        .sessions
        .iter()
        .find(|record| record.external_session_id.as_deref() == Some("thread-local"))
        .expect("project-scoped discovered thread should be imported");
    assert_eq!(discovered_session.session.agent, Agent::Codex);
    assert_eq!(discovered_session.session.workdir, "/tmp/termal");
    assert_eq!(
        discovered_session.session.project_id.as_deref(),
        Some(project.id.as_str())
    );
    assert_eq!(discovered_session.session.model, "gpt-5-codex");
    assert_eq!(
        discovered_session.session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );
    assert_eq!(
        discovered_session.session.preview,
        "Archived Codex thread ready to reopen."
    );
    assert_eq!(
        discovered_session.session.reasoning_effort,
        Some(CodexReasoningEffort::Low)
    );
    assert_eq!(
        discovered_session.session.sandbox_mode,
        Some(CodexSandboxMode::DangerFullAccess)
    );
    assert_eq!(
        discovered_session.session.approval_policy,
        Some(CodexApprovalPolicy::Never)
    );
    assert_eq!(
        inner
            .sessions
            .iter()
            .filter(|record| record.external_session_id.as_deref() == Some("thread-local"))
            .count(),
        1
    );
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| record.external_session_id.as_deref() != Some("thread-other"))
    );
}

// pins the Windows path-normalization step: a Codex `cwd` stored as a
// verbatim `\\?\` prefix is collapsed to the canonical project workdir and
// attached to the existing project rather than creating a second one.
// guards against duplicate projects and orphaned sessions when Codex has
// persisted a legacy verbatim path form.
#[cfg(windows)]
#[test]
fn import_discovered_codex_threads_normalizes_legacy_local_verbatim_paths() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-discovered-verbatim-path-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let legacy_root = format!(r"\\?\{normalized_root}");

    let mut inner = StateInner::new();
    let project = inner.create_project(
        Some("TermAl".to_owned()),
        normalized_root.clone(),
        default_local_remote_id(),
    );
    inner.import_discovered_codex_threads(
        &normalized_root,
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: false,
            cwd: legacy_root,
            id: "thread-legacy".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: None,
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            title: "Legacy thread".to_owned(),
        }],
    );

    assert_eq!(inner.projects.len(), 1);
    assert_eq!(inner.projects[0].root_path, normalized_root);
    let record = inner
        .sessions
        .iter()
        .find(|entry| entry.external_session_id.as_deref() == Some("thread-legacy"))
        .expect("legacy discovered thread should be imported");
    assert_eq!(record.session.workdir, normalized_root);
    assert_eq!(
        record.session.project_id.as_deref(),
        Some(project.id.as_str())
    );

    let _ = fs::remove_dir_all(project_root);
}

// pins that `disable_socket_inheritance` clears `HANDLE_FLAG_INHERIT` on the
// listener's raw socket after explicit test setup marks it inheritable, while
// leaving the handle queryable. guards against the TermAl listener leaking
// into spawned Codex/Claude child processes, which would hold the port open
// after shutdown. (Stray from the cluster; not a discovery test.)
#[cfg(windows)]
#[tokio::test]
async fn disable_socket_inheritance_clears_windows_inherit_flag() {
    use std::os::windows::io::AsRawSocket as _;

    unsafe extern "system" {
        fn GetHandleInformation(handle: *mut std::ffi::c_void, flags: *mut u32) -> i32;
        fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
    }

    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let raw = listener.as_raw_socket() as *mut std::ffi::c_void;

    let inherited = unsafe { SetHandleInformation(raw, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    assert_ne!(
        inherited,
        0,
        "test setup should make the socket inheritable: {}",
        io::Error::last_os_error()
    );

    let mut flags = 0u32;
    let queried = unsafe { GetHandleInformation(raw, &mut flags) };
    assert_ne!(
        queried,
        0,
        "test setup should read socket handle flags: {}",
        io::Error::last_os_error()
    );
    assert_ne!(
        flags & HANDLE_FLAG_INHERIT,
        0,
        "test setup should confirm the inherit bit is set"
    );

    disable_socket_inheritance(&listener);

    flags = 0;
    let queried = unsafe { GetHandleInformation(raw, &mut flags) };
    assert_ne!(
        queried,
        0,
        "socket handle flags should remain queryable after inheritance is disabled: {}",
        io::Error::last_os_error()
    );
    assert_eq!(
        flags & HANDLE_FLAG_INHERIT,
        0,
        "disable_socket_inheritance should clear HANDLE_FLAG_INHERIT"
    );
}

// pins that importing a discovered thread whose id already matches a local
// session leaves the session's user-chosen model, sandbox mode, approval
// policy, and reasoning effort untouched while still updating the archived
// thread-state from the Codex row. guards against discovery clobbering
// prompt settings the user has deliberately overridden.
#[test]
fn import_discovered_codex_threads_preserves_existing_prompt_settings() {
    let mut inner = StateInner::new();
    let project = inner.create_project(
        Some("TermAl".to_owned()),
        "/tmp/termal".to_owned(),
        default_local_remote_id(),
    );
    let mut record = inner.create_session(
        Agent::Codex,
        Some("Codex Live".to_owned()),
        "/tmp/termal".to_owned(),
        Some(project.id.clone()),
        Some("gpt-5-mini".to_owned()),
    );
    record.codex_sandbox_mode = CodexSandboxMode::ReadOnly;
    record.session.sandbox_mode = Some(CodexSandboxMode::ReadOnly);
    record.codex_approval_policy = CodexApprovalPolicy::OnFailure;
    record.session.approval_policy = Some(CodexApprovalPolicy::OnFailure);
    record.codex_reasoning_effort = CodexReasoningEffort::Minimal;
    record.session.reasoning_effort = Some(CodexReasoningEffort::Minimal);
    set_record_external_session_id(&mut record, Some("thread-existing".to_owned()));
    if let Some(slot) = inner
        .find_session_index(&record.session.id)
        .and_then(|index| inner.sessions.get_mut(index))
    {
        *slot = record;
    }

    inner.import_discovered_codex_threads(
        "/tmp/termal",
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: true,
            cwd: "/tmp/termal".to_owned(),
            id: "thread-existing".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::High),
            sandbox_mode: Some(CodexSandboxMode::DangerFullAccess),
            title: "Existing thread".to_owned(),
        }],
    );

    let record = inner
        .sessions
        .iter()
        .find(|entry| entry.external_session_id.as_deref() == Some("thread-existing"))
        .expect("existing discovered thread should still be present");
    assert_eq!(record.session.model, "gpt-5-mini");
    assert_eq!(
        record.session.sandbox_mode,
        Some(CodexSandboxMode::ReadOnly)
    );
    assert_eq!(
        record.session.approval_policy,
        Some(CodexApprovalPolicy::OnFailure)
    );
    assert_eq!(
        record.session.reasoning_effort,
        Some(CodexReasoningEffort::Minimal)
    );
    assert_eq!(
        record.session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );
}

// Pins that newly discovered Codex threads land in `inner.sessions`
// with a `mutation_stamp` > 0, so the SQLite delta-persist watermark
// actually writes them on its next tick. `import_discovered_codex_threads`
// calls `create_session` (which stamps via `push_session`) but then
// replaces the stamped slot with the returned owned record whose stamp
// is the construction-time default — without a re-stamp after the
// slot replace, the row is invisible to `collect_persist_delta` until
// something else happens to re-stamp it (or is never persisted at all
// on a clean shutdown with no subsequent edits).
#[test]
fn import_discovered_codex_threads_stamps_newly_discovered_sessions() {
    let mut inner = StateInner::new();
    let project = inner.create_project(
        Some("TermAl".to_owned()),
        "/tmp/termal".to_owned(),
        default_local_remote_id(),
    );

    // Capture the baseline watermark. The import below must advance
    // the row's `mutation_stamp` strictly past this value.
    let watermark_before_import = inner.last_mutation_stamp;

    inner.import_discovered_codex_threads(
        "/tmp/termal",
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: false,
            cwd: "/tmp/termal".to_owned(),
            id: "thread-fresh".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            title: "Freshly discovered thread".to_owned(),
        }],
    );

    // Capture the imported session's id and stamp into locals so we
    // can drop the immutable borrow of `inner.sessions` before calling
    // `inner.collect_persist_delta(...)` below (which needs `&mut inner`).
    let (imported_session_id, imported_mutation_stamp) = {
        let record = inner
            .sessions
            .iter()
            .find(|entry| entry.external_session_id.as_deref() == Some("thread-fresh"))
            .expect("freshly discovered thread should be imported");
        assert_eq!(
            record.session.project_id.as_deref(),
            Some(project.id.as_str())
        );
        (record.session.id.clone(), record.mutation_stamp)
    };
    assert!(
        imported_mutation_stamp > watermark_before_import,
        "newly imported discovered thread must have mutation_stamp > \
         pre-import watermark so collect_persist_delta picks it up on \
         the next tick; got stamp {imported_mutation_stamp} vs \
         watermark {watermark_before_import}"
    );

    // Beyond the stamp-invariant check above, pin the delta-persist
    // end-to-end pickup. The stamp assertion catches the specific
    // "whole-struct slot replace erases the stamp" bug that lived in
    // `import_discovered_codex_threads` before the re-stamp fix, but
    // it would still pass if a future refactor stamped the record
    // correctly yet broke the watermark wiring inside
    // `collect_persist_delta` (e.g., flipping `<=` to `<` on the
    // gate, or switching the comparison to the old `last_persisted`
    // instead of the `watermark` argument). Calling the real
    // `collect_persist_delta` with the pre-import watermark and
    // asserting the imported session shows up in `changed_sessions`
    // is what catches those regressions.
    let delta = inner.collect_persist_delta(watermark_before_import);
    assert!(
        delta
            .changed_sessions
            .iter()
            .any(|persisted| persisted.session.id == imported_session_id),
        "collect_persist_delta(pre-import watermark) must include the \
         imported Codex thread in `changed_sessions`; got {} changed \
         sessions: {:?}",
        delta.changed_sessions.len(),
        delta
            .changed_sessions
            .iter()
            .map(|persisted| &persisted.session.id)
            .collect::<Vec<_>>()
    );
    assert!(
        delta.watermark >= imported_mutation_stamp,
        "returned watermark must be >= the imported session's \
         mutation stamp so the persist thread advances past it on \
         the next tick; got watermark {} vs stamp {imported_mutation_stamp}",
        delta.watermark
    );
    assert!(
        !delta.removed_session_ids.contains(&imported_session_id),
        "a freshly imported thread is not hidden and must not appear \
         in `removed_session_ids`"
    );

    // Idempotency: a second collection at the freshly-advanced
    // watermark returns no changes (no session was re-stamped). This
    // pins that the watermark contract is "strictly past this stamp"
    // and catches a regression that double-counts imported rows.
    let next_watermark = delta.watermark;
    let second_delta = inner.collect_persist_delta(next_watermark);
    assert!(
        !second_delta
            .changed_sessions
            .iter()
            .any(|persisted| persisted.session.id == imported_session_id),
        "second collection at the returned watermark must not re-emit \
         the same imported session"
    );
}
