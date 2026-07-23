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

// A stale top-level-looking copy can be encountered before a newer home copy
// identifies the same thread as a TermAl delegation child. Classification
// from any home must win after deduplication so the stale copy is not imported.
#[test]
fn discover_codex_threads_from_homes_retains_delegation_classification_across_duplicates() {
    let root = std::env::temp_dir().join(format!(
        "termal-codex-cross-home-delegation-{}",
        Uuid::new_v4()
    ));
    let stale_home = root.join("stale");
    let classified_home = root.join("classified");
    let delegated_child_prompt = format!(
        "{DELEGATED_CHILD_SESSION_MARKER} `delegation-cross-home`.\n\n\
         Mode: Reviewer\n\
         Parent session: `session-parent`\n\
         Child session: `session-child`\n\n\
         Task:\nReview the patch"
    );
    write_test_codex_threads_db(
        &stale_home,
        &[(
            "thread-cross-home",
            "/tmp/termal",
            "Stale top-level title",
            r#"{"type":"workspace-write"}"#,
            "on-request",
            0,
            Some("gpt-5-codex"),
            Some("medium"),
            20,
        )],
    );
    write_test_codex_threads_db(
        &classified_home,
        &[(
            "thread-cross-home",
            "/tmp/termal",
            delegated_child_prompt.as_str(),
            r#"{"type":"read-only"}"#,
            "never",
            0,
            Some("gpt-5-codex"),
            Some("high"),
            10,
        )],
    );

    let discovery = discover_codex_threads_from_homes(
        &[stale_home, classified_home],
        &[PathBuf::from("/tmp/termal")],
    )
    .expect("cross-home discovery should load");

    assert!(discovery.threads.is_empty());
    assert_eq!(
        discovery.delegation_thread_ids,
        BTreeSet::from(["thread-cross-home".to_owned()])
    );

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

// Pins the current Codex schema's nested-agent markers. Subagent threads are
// real resumable Codex threads, but they belong inside their parent's agent
// tree and must never become top-level TermAl ghost sessions after a restart.
// The filter must run before the per-home limit: otherwise a busy parent with
// hundreds of newer children can crowd an older top-level conversation out of
// discovery even when every child is discarded afterward. `source` remains a
// fallback for Codex databases that gained the nested source payload before
// they gained the denormalized `thread_source` column value.
#[test]
fn discover_codex_threads_from_home_excludes_subagents_before_limit_and_reports_ids() {
    let codex_home =
        std::env::temp_dir().join(format!("termal-codex-home-subagents-{}", Uuid::new_v4()));
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    let connection =
        rusqlite::Connection::open(codex_home.join("state_8.sqlite")).expect("db should open");
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
                source text,
                thread_source text,
                updated_at integer not null
            );",
        )
        .expect("threads table should be created");

    for index in 0..MAX_DISCOVERED_CODEX_THREADS_PER_HOME {
        connection
            .execute(
                "insert into threads (
                    id, cwd, title, sandbox_policy, approval_mode, archived,
                    model, reasoning_effort, source, thread_source, updated_at
                ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    format!("thread-child-{index}"),
                    "/tmp/termal",
                    "do git pull",
                    r#"{"type":"read-only"}"#,
                    "never",
                    0,
                    "gpt-5-codex",
                    "high",
                    r#"{"subagent":{"thread_spawn":{"parent_thread_id":"thread-parent"}}}"#,
                    "subagent",
                    10_000 - index as i64,
                ],
            )
            .expect("subagent thread row should insert");
    }
    connection
        .execute(
            "insert into threads (
                id, cwd, title, sandbox_policy, approval_mode, archived,
                model, reasoning_effort, source, thread_source, updated_at
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                "thread-child-source-only",
                "/tmp/termal",
                "Inherited parent prompt",
                r#"{"type":"read-only"}"#,
                "never",
                0,
                "gpt-5-codex",
                "high",
                r#"{"subagent":{"thread_spawn":{"parent_thread_id":"thread-parent"}}}"#,
                Option::<&str>::None,
                20_000,
            ],
        )
        .expect("source-only subagent row should insert");
    let delegated_child_prompt = format!(
        "{DELEGATED_CHILD_SESSION_MARKER} `delegation-leaked`.\n\n\
         Mode: Reviewer\n\
         Parent session: `session-parent`\n\
         Child session: `session-child`\n\n\
         Task:\nReview the patch"
    );
    connection
        .execute(
            "insert into threads (
                id, cwd, title, sandbox_policy, approval_mode, archived,
                model, reasoning_effort, source, thread_source, updated_at
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                "thread-termal-delegation-child",
                "/tmp/termal",
                delegated_child_prompt,
                r#"{"type":"read-only"}"#,
                "never",
                0,
                "gpt-5-codex",
                "high",
                Option::<&str>::None,
                Option::<&str>::None,
                30_000,
            ],
        )
        .expect("TermAl delegation child row should insert");
    connection
        .execute(
            "insert into threads (
                id, cwd, title, sandbox_policy, approval_mode, archived,
                model, reasoning_effort, source, thread_source, updated_at
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                "thread-parent",
                "/tmp/termal",
                "Real top-level conversation",
                r#"{"type":"workspace-write"}"#,
                "on-request",
                0,
                "gpt-5-codex",
                "medium",
                Option::<&str>::None,
                Option::<&str>::None,
                1,
            ],
        )
        .expect("null-source top-level thread row should insert");

    let discovery = discover_codex_threads_with_subagents_from_home(
        &codex_home,
        &[PathBuf::from("/tmp/termal")],
    )
    .expect("threads should load");

    assert_eq!(
        discovery
            .threads
            .iter()
            .map(|thread| thread.id.as_str())
            .collect::<Vec<_>>(),
        vec!["thread-parent"],
        "subagents must be removed in SQL before the row cap so the older parent survives"
    );
    assert_eq!(
        discovery.subagent_thread_ids.len(),
        MAX_DISCOVERED_CODEX_THREADS_PER_HOME + 1
    );
    assert!(discovery.subagent_thread_ids.contains("thread-child-0"));
    assert!(
        discovery
            .subagent_thread_ids
            .contains("thread-child-source-only"),
        "the source JSON fallback should classify subagents when thread_source is null"
    );
    assert!(!discovery.subagent_thread_ids.contains("thread-parent"));
    assert_eq!(
        discovery.delegation_thread_ids,
        BTreeSet::from(["thread-termal-delegation-child".to_owned()]),
        "TermAl delegation bootstrap threads must be removed before the row cap and reported for cleanup"
    );

    drop(connection);
    let _ = fs::remove_dir_all(&codex_home);
}

// The SQL prefix is only a candidate accelerator. Rust owns the authoritative
// marker validation, so malformed prefix matches and NULL titles remain normal
// top-level threads while a structurally valid bootstrap is classified.
#[test]
fn discover_codex_threads_from_home_retains_invalid_delegation_prefixes_and_null_titles() {
    let codex_home = std::env::temp_dir().join(format!(
        "termal-codex-home-delegation-marker-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    let connection =
        rusqlite::Connection::open(codex_home.join("state_8.sqlite")).expect("db should open");
    connection
        .execute_batch(
            "create table threads (
                id text primary key,
                cwd text not null,
                title text,
                sandbox_policy text not null,
                approval_mode text not null,
                archived integer not null,
                model text,
                reasoning_effort text,
                updated_at integer not null
            );",
        )
        .expect("threads table should be created");
    let valid_prompt = format!(
        "{DELEGATED_CHILD_SESSION_MARKER} `delegation-valid`.\n\n\
         Mode: Reviewer\n\
         Parent session: `session-parent`\n\
         Child session: `session-child`\n\n\
         Task:\nReview the patch"
    );
    let truncated_prompt = format!(
        "{DELEGATED_CHILD_SESSION_MARKER} `delegation-truncated`.\n\n\
         Mode: Reviewer\n\
         Parent session: `session-parent`"
    );
    for (id, title, updated_at) in [
        ("thread-valid-child", Some(valid_prompt.as_str()), 40),
        (
            "thread-truncated-child",
            Some(truncated_prompt.as_str()),
            35,
        ),
        (
            "thread-invalid-prefix",
            Some("You are a delegated child session for TermAl delegation but malformed"),
            30,
        ),
        ("thread-null-title", None, 20),
        ("thread-ordinary", Some("Ordinary top-level thread"), 10),
    ] {
        connection
            .execute(
                "insert into threads (
                    id, cwd, title, sandbox_policy, approval_mode, archived,
                    model, reasoning_effort, updated_at
                ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    id,
                    "/tmp/termal",
                    title,
                    r#"{"type":"workspace-write"}"#,
                    "on-request",
                    0,
                    "gpt-5-codex",
                    "medium",
                    updated_at,
                ],
            )
            .expect("thread row should insert");
    }

    let discovery = discover_codex_threads_with_subagents_from_home(
        &codex_home,
        &[PathBuf::from("/tmp/termal")],
    )
    .expect("threads should load");

    assert_eq!(
        discovery
            .threads
            .iter()
            .map(|thread| (thread.id.as_str(), thread.title.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("thread-truncated-child", truncated_prompt.as_str()),
            (
                "thread-invalid-prefix",
                "You are a delegated child session for TermAl delegation but malformed",
            ),
            ("thread-null-title", ""),
            ("thread-ordinary", "Ordinary top-level thread"),
        ]
    );
    assert_eq!(
        discovery.delegation_thread_ids,
        BTreeSet::from(["thread-valid-child".to_owned()])
    );

    drop(connection);
    let _ = fs::remove_dir_all(&codex_home);
}

// Current Codex stores the complete first user prompt as `threads.title`, but
// the discovery classifier still re-checks filesystem scope after the SQL
// prefix query. A syntactically valid delegation marker whose raw cwd starts
// inside the project and then escapes through `..` must not be classified or
// imported.
#[test]
fn discover_codex_threads_from_home_rejects_delegation_marker_outside_normalized_scope() {
    let root = std::env::temp_dir().join(format!(
        "termal-codex-home-delegation-outside-scope-{}",
        Uuid::new_v4()
    ));
    let codex_home = root.join("codex");
    let project_scope = root.join("project");
    let outside_repo = root.join("outside").join("repo");
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    fs::create_dir_all(&project_scope).expect("project scope should be created");
    fs::create_dir_all(&outside_repo).expect("outside repository should be created");
    let raw_escaping_cwd = project_scope.join("..").join("outside").join("repo");
    let connection =
        rusqlite::Connection::open(codex_home.join("state_8.sqlite")).expect("db should open");
    connection
        .execute_batch(
            "create table threads (
                id text primary key,
                cwd text not null,
                title text,
                sandbox_policy text not null,
                approval_mode text not null,
                archived integer not null,
                model text,
                reasoning_effort text,
                updated_at integer not null
            );",
        )
        .expect("threads table should be created");
    let valid_prompt = format!(
        "{DELEGATED_CHILD_SESSION_MARKER} `delegation-outside`.\n\n\
         Mode: Reviewer\n\
         Parent session: `session-parent`\n\
         Child session: `session-child`\n\n\
         Task:\nReview the patch"
    );
    connection
        .execute(
            "insert into threads (
                id, cwd, title, sandbox_policy, approval_mode, archived,
                model, reasoning_effort, updated_at
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "thread-outside-child",
                raw_escaping_cwd.to_string_lossy().as_ref(),
                valid_prompt,
                r#"{"type":"workspace-write"}"#,
                "on-request",
                0,
                "gpt-5-codex",
                "medium",
                1,
            ],
        )
        .expect("out-of-scope delegation row should insert");

    let discovery = discover_codex_threads_with_subagents_from_home(&codex_home, &[project_scope])
        .expect("threads should load");

    assert!(discovery.delegation_thread_ids.is_empty());
    assert!(discovery.threads.is_empty());

    drop(connection);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn codex_discovery_like_prefix_pattern_escapes_sql_wildcards() {
    assert_eq!(
        codex_discovery_like_prefix_pattern(r"delegated%_\child"),
        r"delegated\%\_\\child%"
    );
}

// Pins the source-only Codex schema too. SQL `LIKE` returns NULL for a NULL
// operand, so negating a raw `source like ...` expression would otherwise
// discard a legitimate top-level thread instead of retaining it. The sibling
// source-marked subagent must still be classified and excluded.
#[test]
fn discover_codex_threads_from_home_retains_null_source_without_thread_source_column() {
    let codex_home =
        std::env::temp_dir().join(format!("termal-codex-home-null-source-{}", Uuid::new_v4()));
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    let connection =
        rusqlite::Connection::open(codex_home.join("state_8.sqlite")).expect("db should open");
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
                source text,
                updated_at integer not null
            );",
        )
        .expect("source-only threads table should be created");
    connection
        .execute(
            "insert into threads (
                id, cwd, title, sandbox_policy, approval_mode, archived,
                model, reasoning_effort, source, updated_at
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                "thread-parent-null-source",
                "/tmp/termal",
                "Top-level conversation",
                r#"{"type":"workspace-write"}"#,
                "on-request",
                0,
                "gpt-5-codex",
                "medium",
                Option::<&str>::None,
                1,
            ],
        )
        .expect("null-source top-level row should insert");
    connection
        .execute(
            "insert into threads (
                id, cwd, title, sandbox_policy, approval_mode, archived,
                model, reasoning_effort, source, updated_at
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                "thread-child-source-only-schema",
                "/tmp/termal",
                "Nested conversation",
                r#"{"type":"read-only"}"#,
                "never",
                0,
                "gpt-5-codex",
                "high",
                r#"{"subagent":{"thread_spawn":{"parent_thread_id":"thread-parent-null-source"}}}"#,
                2,
            ],
        )
        .expect("source-marked subagent row should insert");

    let discovery = discover_codex_threads_with_subagents_from_home(
        &codex_home,
        &[PathBuf::from("/tmp/termal")],
    )
    .expect("threads should load");

    assert_eq!(
        discovery
            .threads
            .iter()
            .map(|thread| thread.id.as_str())
            .collect::<Vec<_>>(),
        vec!["thread-parent-null-source"]
    );
    assert_eq!(
        discovery.subagent_thread_ids,
        BTreeSet::from(["thread-child-source-only-schema".to_owned()])
    );

    drop(connection);
    let _ = fs::remove_dir_all(&codex_home);
}

// Pins restart cleanup for the already-persisted bad rows. Only an empty,
// visible, idle, top-level local Codex ghost may be removed. A real TermAl
// delegation child and a non-idle session are retained even when their Codex
// thread IDs are classified as subagents, and an unrelated top-level ghost is
// untouched. The removed id must become a persistence tombstone so the card
// does not return on the following restart.
#[test]
fn prune_auto_imported_codex_child_sessions_removes_only_empty_top_level_ghosts() {
    let mut inner = StateInner::new();
    let project = inner.create_project(
        Some("TermAl".to_owned()),
        "/tmp/termal".to_owned(),
        default_local_remote_id(),
    );

    let ghost = inner.create_session(
        Agent::Codex,
        Some("do git pull".to_owned()),
        "/tmp/termal".to_owned(),
        Some(project.id.clone()),
        None,
    );
    let ghost_session_id = ghost.session.id.clone();
    let ghost_index = inner
        .find_session_index(&ghost_session_id)
        .expect("ghost session should exist");
    set_record_external_session_id(
        inner
            .session_mut_by_index(ghost_index)
            .expect("ghost session should be mutable"),
        Some("thread-subagent-ghost".to_owned()),
    );

    let delegation_ghost = inner.create_session(
        Agent::Codex,
        Some("delegation bootstrap prompt".to_owned()),
        "/tmp/termal".to_owned(),
        Some(project.id.clone()),
        None,
    );
    let delegation_ghost_session_id = delegation_ghost.session.id.clone();
    let delegation_ghost_index = inner
        .find_session_index(&delegation_ghost_session_id)
        .expect("delegation ghost session should exist");
    set_record_external_session_id(
        inner
            .session_mut_by_index(delegation_ghost_index)
            .expect("delegation ghost session should be mutable"),
        Some("thread-delegation-ghost".to_owned()),
    );

    let child = inner.create_session(
        Agent::Codex,
        Some("Live delegation child".to_owned()),
        "/tmp/termal".to_owned(),
        Some(project.id.clone()),
        None,
    );
    let child_session_id = child.session.id.clone();
    let child_index = inner
        .find_session_index(&child_session_id)
        .expect("child session should exist");
    let child_record = inner
        .session_mut_by_index(child_index)
        .expect("child session should be mutable");
    child_record.session.parent_delegation_id = Some("delegation-live".to_owned());
    set_record_external_session_id(child_record, Some("thread-subagent-child".to_owned()));

    let active = inner.create_session(
        Agent::Codex,
        Some("Active nested thread".to_owned()),
        "/tmp/termal".to_owned(),
        Some(project.id.clone()),
        None,
    );
    let active_session_id = active.session.id.clone();
    let active_index = inner
        .find_session_index(&active_session_id)
        .expect("active session should exist");
    let active_record = inner
        .session_mut_by_index(active_index)
        .expect("active session should be mutable");
    active_record.session.status = SessionStatus::Active;
    set_record_external_session_id(active_record, Some("thread-subagent-active".to_owned()));

    let unrelated = inner.create_session(
        Agent::Codex,
        Some("Unrelated ghost".to_owned()),
        "/tmp/termal".to_owned(),
        Some(project.id),
        None,
    );
    let unrelated_session_id = unrelated.session.id.clone();
    let unrelated_index = inner
        .find_session_index(&unrelated_session_id)
        .expect("unrelated session should exist");
    set_record_external_session_id(
        inner
            .session_mut_by_index(unrelated_index)
            .expect("unrelated session should be mutable"),
        Some("thread-top-level".to_owned()),
    );

    let child_thread_ids = BTreeSet::from([
        "thread-subagent-ghost".to_owned(),
        "thread-delegation-ghost".to_owned(),
        "thread-subagent-child".to_owned(),
        "thread-subagent-active".to_owned(),
    ]);
    let removed = inner.prune_auto_imported_codex_child_sessions(&child_thread_ids);

    assert_eq!(removed, 2);
    assert!(inner.find_session_index(&ghost_session_id).is_none());
    assert!(
        inner
            .find_session_index(&delegation_ghost_session_id)
            .is_none()
    );
    assert!(inner.find_session_index(&child_session_id).is_some());
    assert!(inner.find_session_index(&active_session_id).is_some());
    assert!(inner.find_session_index(&unrelated_session_id).is_some());
    assert!(
        inner.removed_session_ids.contains(&ghost_session_id),
        "the pruned ghost must be deleted from SQLite on the startup persist"
    );
    assert!(
        inner
            .removed_session_ids
            .contains(&delegation_ghost_session_id),
        "the orphaned delegation ghost must also be deleted from SQLite"
    );
}

#[test]
fn auto_imported_codex_empty_ghost_predicate_rejects_owned_or_user_visible_sessions() {
    let mut inner = StateInner::new();
    let project = inner.create_project(
        Some("TermAl".to_owned()),
        "/tmp/termal".to_owned(),
        default_local_remote_id(),
    );
    let ghost = inner.create_session(
        Agent::Codex,
        Some("legacy raw prompt".to_owned()),
        "/tmp/termal".to_owned(),
        Some(project.id),
        None,
    );
    let ghost_index = inner
        .find_session_index(&ghost.session.id)
        .expect("ghost session should exist");
    set_record_external_session_id(
        inner
            .session_mut_by_index(ghost_index)
            .expect("ghost session should be mutable"),
        Some("thread-ghost-predicate".to_owned()),
    );
    let baseline = inner.sessions[ghost_index].clone();

    assert!(is_empty_top_level_auto_imported_codex_ghost(&baseline));

    let mut hidden = baseline.clone();
    hidden.hidden = true;
    assert!(!is_empty_top_level_auto_imported_codex_ghost(&hidden));

    let mut remote = baseline.clone();
    remote.remote_id = Some("remote-1".to_owned());
    remote.remote_session_id = Some("remote-session-1".to_owned());
    assert!(!is_empty_top_level_auto_imported_codex_ghost(&remote));

    let mut delegated = baseline.clone();
    delegated.session.parent_delegation_id = Some("delegation-1".to_owned());
    assert!(!is_empty_top_level_auto_imported_codex_ghost(&delegated));

    let mut active = baseline.clone();
    active.session.status = SessionStatus::Active;
    assert!(!is_empty_top_level_auto_imported_codex_ghost(&active));

    let mut nonempty = baseline;
    nonempty.session.pending_prompts.push(PendingPrompt {
        attachments: Vec::new(),
        id: "message-pending".to_owned(),
        timestamp: stamp_now(),
        text: "queued user work".to_owned(),
        expanded_text: None,
        source: None,
    });
    assert!(!is_empty_top_level_auto_imported_codex_ghost(&nonempty));
}

// Exercises the production restart wiring rather than only the discovery and
// cleanup helpers in isolation. The initial SQLite state represents the bad
// state produced by an older build: an empty top-level TermAl session points at
// a Codex subagent thread. Boot must classify that native thread, remove the
// persisted ghost, and still import its real top-level parent. A reload after
// the persist worker shuts down proves the cleanup tombstone reached disk.
#[test]
fn app_state_restart_prunes_persisted_codex_subagent_ghost() {
    let _env_lock = TEST_HOME_ENV_MUTEX
        .lock()
        .expect("test home env mutex poisoned");
    let root =
        std::env::temp_dir().join(format!("termal-codex-subagent-restart-{}", Uuid::new_v4()));
    let project_root = root.join("project");
    let test_home = root.join("home");
    let shared_codex_home = test_home
        .join(".termal")
        .join("codex-home")
        .join("shared-app-server");
    fs::create_dir_all(&project_root).expect("project root should exist");
    let project_root =
        fs::canonicalize(project_root).expect("project root should canonicalize for discovery");
    fs::create_dir_all(&shared_codex_home).expect("shared Codex home should exist");
    let _home = ScopedEnvVar::set_home_dir(&test_home);
    let source_codex_home = test_home.join(".codex");
    let _codex_home = ScopedEnvVar::set_path("CODEX_HOME", &source_codex_home);

    let project_workdir = project_root.to_string_lossy().into_owned();
    let codex_connection = rusqlite::Connection::open(shared_codex_home.join("state_5.sqlite"))
        .expect("Codex state db should open");
    codex_connection
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
                source text,
                thread_source text,
                updated_at integer not null
            );",
        )
        .expect("threads table should be created");
    codex_connection
        .execute(
            "insert into threads (
                id, cwd, title, sandbox_policy, approval_mode, archived,
                model, reasoning_effort, source, thread_source, updated_at
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                "thread-child",
                project_workdir,
                "do git pull",
                r#"{"type":"read-only"}"#,
                "never",
                0,
                "gpt-5-codex",
                "high",
                r#"{"subagent":{"thread_spawn":{"parent_thread_id":"thread-parent"}}}"#,
                "subagent",
                2,
            ],
        )
        .expect("subagent thread should insert");
    let delegated_child_prompt = format!(
        "{DELEGATED_CHILD_SESSION_MARKER} `delegation-orphaned`.\n\n\
         Mode: Reviewer\n\
         Parent session: `session-old-parent`\n\
         Child session: `session-old-child`\n\n\
         Task:\nReview the patch"
    );
    codex_connection
        .execute(
            "insert into threads (
                id, cwd, title, sandbox_policy, approval_mode, archived,
                model, reasoning_effort, source, thread_source, updated_at
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                "thread-delegation-child",
                project_workdir,
                delegated_child_prompt,
                r#"{"type":"read-only"}"#,
                "never",
                0,
                "gpt-5-codex",
                "high",
                Option::<&str>::None,
                Option::<&str>::None,
                3,
            ],
        )
        .expect("TermAl delegation thread should insert");
    codex_connection
        .execute(
            "insert into threads (
                id, cwd, title, sandbox_policy, approval_mode, archived,
                model, reasoning_effort, source, thread_source, updated_at
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                "thread-parent",
                project_workdir,
                "Real parent conversation",
                r#"{"type":"workspace-write"}"#,
                "on-request",
                0,
                "gpt-5-codex",
                "medium",
                "vscode",
                Option::<&str>::None,
                1,
            ],
        )
        .expect("parent thread should insert");
    drop(codex_connection);

    assert_eq!(
        resolve_termal_codex_discovery_root(&project_workdir),
        test_home.join(".termal").join("codex-home")
    );
    let direct_discovery = discover_codex_threads_with_subagents_from_home(
        &shared_codex_home,
        &[project_root.clone()],
    )
    .expect("direct shared-home discovery should load");
    assert!(
        direct_discovery
            .subagent_thread_ids
            .contains("thread-child")
    );
    assert!(
        direct_discovery
            .delegation_thread_ids
            .contains("thread-delegation-child")
    );
    assert!(
        direct_discovery
            .threads
            .iter()
            .any(|thread| thread.id == "thread-parent")
    );

    let persistence_path = test_home.join("termal.sqlite");
    let templates_path = test_home.join("orchestrators.json");
    let mut initial_inner = StateInner::new();
    let project = initial_inner.create_project(
        Some("TermAl".to_owned()),
        project_workdir.clone(),
        default_local_remote_id(),
    );
    let ghost = initial_inner.create_session(
        Agent::Codex,
        Some("do git pull".to_owned()),
        project_workdir.clone(),
        Some(project.id.clone()),
        None,
    );
    let ghost_session_id = ghost.session.id.clone();
    let ghost_index = initial_inner
        .find_session_index(&ghost_session_id)
        .expect("persisted ghost should exist");
    set_record_external_session_id(
        initial_inner
            .session_mut_by_index(ghost_index)
            .expect("persisted ghost should be mutable"),
        Some("thread-child".to_owned()),
    );
    let delegation_ghost = initial_inner.create_session(
        Agent::Codex,
        Some("delegation bootstrap prompt".to_owned()),
        project_workdir.clone(),
        Some(project.id),
        None,
    );
    let delegation_ghost_session_id = delegation_ghost.session.id.clone();
    let delegation_ghost_index = initial_inner
        .find_session_index(&delegation_ghost_session_id)
        .expect("persisted delegation ghost should exist");
    set_record_external_session_id(
        initial_inner
            .session_mut_by_index(delegation_ghost_index)
            .expect("persisted delegation ghost should be mutable"),
        Some("thread-delegation-child".to_owned()),
    );
    persist_state(&persistence_path, &initial_inner).expect("bad pre-restart state should persist");

    let pre_restart = load_state(&persistence_path)
        .expect("bad pre-restart state should reload")
        .expect("bad pre-restart state should exist");
    let persisted_ghost = pre_restart
        .sessions
        .iter()
        .find(|record| record.session.id == ghost_session_id)
        .expect("the bad ghost should exist before restart cleanup");
    assert_eq!(
        persisted_ghost.external_session_id.as_deref(),
        Some("thread-child")
    );
    assert!(!persisted_ghost.hidden);
    assert!(persisted_ghost.session.parent_delegation_id.is_none());
    assert!(persisted_ghost.session.messages.is_empty());
    assert!(persisted_ghost.session.pending_prompts.is_empty());
    assert!(persisted_ghost.queued_prompts.is_empty());
    assert!(matches!(
        persisted_ghost.session.status,
        SessionStatus::Idle
    ));

    let discovery_scopes = collect_codex_discovery_scopes(&project_workdir, &pre_restart.projects);
    assert!(!discovery_scopes.is_empty());
    assert!(discovery_scopes.iter().any(|scope| scope == &project_root));
    let discovery_homes = discover_codex_home_candidates(
        Some(&source_codex_home),
        &resolve_termal_codex_discovery_root(&project_workdir),
    );
    assert!(
        discovery_homes
            .iter()
            .any(|home| home == &shared_codex_home)
    );
    let explicit_discovery = discover_codex_threads_with_subagents_from_sources(
        Some(&source_codex_home),
        &resolve_termal_codex_discovery_root(&project_workdir),
        &discovery_scopes,
    )
    .expect("explicit production discovery should load");
    assert!(
        explicit_discovery
            .subagent_thread_ids
            .contains("thread-child"),
        "explicit production discovery should retain classified child ids"
    );
    assert!(
        explicit_discovery
            .delegation_thread_ids
            .contains("thread-delegation-child"),
        "explicit production discovery should retain classified TermAl delegation ids"
    );
    let pre_restart_discovery = discover_codex_threads(&project_workdir, &discovery_scopes)
        .expect("pre-restart Codex discovery should load");
    assert!(
        pre_restart_discovery
            .subagent_thread_ids
            .contains("thread-child"),
        "production discovery must classify the child before AppState boot cleanup; \
         got subagents {:?} and top-level threads {:?}",
        pre_restart_discovery.subagent_thread_ids,
        pre_restart_discovery
            .threads
            .iter()
            .map(|thread| thread.id.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        pre_restart_discovery
            .delegation_thread_ids
            .contains("thread-delegation-child"),
        "production discovery must classify orphaned TermAl delegation threads"
    );

    let state = AppState::new_with_paths(project_workdir, persistence_path.clone(), templates_path)
        .expect("state should restart");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(
            inner.sessions.iter().all(|record| !matches!(
                record.external_session_id.as_deref(),
                Some("thread-child" | "thread-delegation-child")
            )),
            "previously imported child ghosts should disappear during boot"
        );
        assert!(
            inner
                .sessions
                .iter()
                .any(|record| record.external_session_id.as_deref() == Some("thread-parent")),
            "the real top-level parent must remain discoverable and resumable"
        );
    }
    state.shutdown_persist_blocking();
    drop(state);

    let persisted = load_state(&persistence_path)
        .expect("cleaned state should reload")
        .expect("cleaned state should exist");
    assert!(persisted.find_session_index(&ghost_session_id).is_none());
    assert!(
        persisted
            .find_session_index(&delegation_ghost_session_id)
            .is_none()
    );
    assert!(
        persisted.sessions.iter().all(|record| !matches!(
            record.external_session_id.as_deref(),
            Some("thread-child" | "thread-delegation-child")
        )),
        "the cleanup must persist so child dummy cards cannot return next restart"
    );
    assert!(
        persisted
            .sessions
            .iter()
            .any(|record| record.external_session_id.as_deref() == Some("thread-parent"))
    );

    drop(_codex_home);
    drop(_home);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn import_discovered_codex_threads_reclaims_a_suppressed_thread_a_session_still_owns() {
    // Pins the invariant that makes orphan suppression safe at all.
    //
    // The thread-setup failure paths (`codex.rs`) suppress a thread they could not
    // bind — but they may be suppressing one the session record STILL CLAIMS:
    // `set_external_session_id_if_runtime_matches` writes `external_session_id`
    // BEFORE the `commit_locked` that is the only thing able to fail, and
    // `thread/started` writes it unconditionally. So "suppressed" and "claimed"
    // genuinely overlap.
    //
    // That is only safe because discovery looks for an owning record FIRST and
    // un-ignores the thread when it finds one. If this ordering is ever inverted,
    // a live conversation lands on the never-rediscover list — which is the exact
    // failure that bit repeatedly while this fix was being developed, and it is
    // silent. It gets a test rather than a comment.
    let mut inner = StateInner::new();
    let project = inner.create_project(
        Some("TermAl".to_owned()),
        "/tmp/termal".to_owned(),
        default_local_remote_id(),
    );
    let record = inner.create_session(
        Agent::Codex,
        Some("Live conversation".to_owned()),
        "/tmp/termal".to_owned(),
        Some(project.id.clone()),
        None,
    );
    let session_id = record.session.id.clone();
    let index = inner
        .find_session_index(&session_id)
        .expect("created session should exist");
    inner.sessions[index].external_session_id = Some("thread-owned".to_owned());
    inner.sessions[index].session.external_session_id = Some("thread-owned".to_owned());

    // A failed setup disowned the thread even though the record still claims it.
    inner.ignore_discovered_codex_thread(Some("thread-owned"));
    assert!(
        inner
            .ignored_discovered_codex_thread_ids
            .contains("thread-owned"),
        "precondition: the thread is on the never-rediscover list"
    );

    inner.import_discovered_codex_threads(
        "/tmp/termal",
        vec![DiscoveredCodexThread {
            approval_policy: None,
            archived: false,
            cwd: "/tmp/termal".to_owned(),
            id: "thread-owned".to_owned(),
            model: None,
            reasoning_effort: None,
            sandbox_mode: None,
            title: "Live conversation".to_owned(),
        }],
    );

    assert!(
        !inner
            .ignored_discovered_codex_thread_ids
            .contains("thread-owned"),
        "a thread a session record still claims must be RECLAIMED, not left stranded on \
         the never-rediscover list — suppression on the setup-failure paths is only safe \
         because discovery checks for an owning record before it consults the ignore set"
    );
    assert_eq!(
        inner
            .sessions
            .iter()
            .filter(|record| record.session.agent == Agent::Codex)
            .count(),
        1,
        "the owning session must be reused, not duplicated by a re-import"
    );
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
    let existing_session = inner.create_session(
        Agent::Codex,
        None,
        "/tmp/termal".to_owned(),
        Some(project.id.clone()),
        None,
    );
    assert_eq!(existing_session.session.name, "Codex 1");
    assert_eq!(
        existing_session.session.name,
        generated_session_name(Agent::Codex, &existing_session.session.id)
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
    assert_eq!(
        discovered_session.session.name, "Codex 2",
        "auto-imported Codex sessions must not expose the raw first prompt as their label"
    );
    assert_eq!(
        discovered_session.session.name,
        generated_session_name(Agent::Codex, &discovered_session.session.id),
        "normal creation and Codex import must share one generated-name contract"
    );
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

#[test]
fn import_discovered_codex_threads_relabels_legacy_raw_prompt_names_but_preserves_user_names() {
    let mut inner = StateInner::new();
    let project = inner.create_project(
        Some("TermAl".to_owned()),
        "/tmp/termal".to_owned(),
        default_local_remote_id(),
    );
    let legacy = inner.create_session(
        Agent::Codex,
        Some("can you fix compilation".to_owned()),
        "/tmp/termal".to_owned(),
        Some(project.id.clone()),
        None,
    );
    let legacy_id = legacy.session.id.clone();
    let legacy_index = inner
        .find_session_index(&legacy_id)
        .expect("legacy imported session should exist");
    set_record_external_session_id(
        inner
            .session_mut_by_index(legacy_index)
            .expect("legacy imported session should be mutable"),
        Some("thread-legacy-name".to_owned()),
    );

    let renamed = inner.create_session(
        Agent::Codex,
        Some("My investigation".to_owned()),
        "/tmp/termal".to_owned(),
        Some(project.id),
        None,
    );
    let renamed_id = renamed.session.id.clone();
    let renamed_index = inner
        .find_session_index(&renamed_id)
        .expect("renamed imported session should exist");
    set_record_external_session_id(
        inner
            .session_mut_by_index(renamed_index)
            .expect("renamed imported session should be mutable"),
        Some("thread-user-name".to_owned()),
    );

    inner.import_discovered_codex_threads(
        "/tmp/termal",
        vec![
            DiscoveredCodexThread {
                approval_policy: None,
                archived: false,
                cwd: "/tmp/termal".to_owned(),
                id: "thread-legacy-name".to_owned(),
                model: None,
                reasoning_effort: None,
                sandbox_mode: None,
                title: "can you fix compilation".to_owned(),
            },
            DiscoveredCodexThread {
                approval_policy: None,
                archived: false,
                cwd: "/tmp/termal".to_owned(),
                id: "thread-user-name".to_owned(),
                model: None,
                reasoning_effort: None,
                sandbox_mode: None,
                title: "the original first prompt".to_owned(),
            },
        ],
    );

    assert_eq!(
        inner
            .sessions
            .iter()
            .find(|record| record.session.id == legacy_id)
            .expect("legacy session should remain")
            .session
            .name,
        "Codex 1"
    );
    assert_eq!(
        inner
            .sessions
            .iter()
            .find(|record| record.session.id == renamed_id)
            .expect("renamed session should remain")
            .session
            .name,
        "My investigation",
        "a user-selected session name must survive discovery refresh"
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
