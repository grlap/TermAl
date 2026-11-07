use super::*;

pub(super) fn source_owner_output(code: i32, bytes: Vec<u8>) -> std::process::Output {
    #[cfg(windows)]
    let status = { use std::os::windows::process::ExitStatusExt; std::process::ExitStatus::from_raw(code as u32) };
    #[cfg(not(windows))]
    let status = { use std::os::unix::process::ExitStatusExt; std::process::ExitStatus::from_raw(code << 8) };
    std::process::Output { status, stdout: bytes, stderr: vec![] }
}

pub(super) fn source_owner_reply(args: &[String]) -> std::process::Output {
    if args == ["--version"] {
        // This is intentionally not a source-schema declaration.
        source_owner_output(0, b"engram fixture (schema durable-store)\n".to_vec())
    } else {
        assert_eq!(args[0], "source");
        assert_eq!(args[1], "--root");
        assert_eq!(args[3], "--json");
        assert_eq!(args[4], "--timeout-ms");
        assert!((1..=30000).contains(&args[5].parse::<u64>().unwrap()));
        source_owner_output(1, serde_json::to_vec(&json!({
            "version":1, "status":"unavailable", "root":args[2], "source":null, "tree":null,
            "entries":[], "refusal":{"code":"input_unavailable","message":"fixture is not a repository"},
            "source_id":null,
        })).unwrap())
    }
}

pub(super) fn source_owner_fixture_binary(root: &FsPath) -> PathBuf {
    let binary = root.join("source-owner-fixture.exe");
    fs::write(&binary, source_owner_fixture_bytes()).unwrap();
    binary
}

fn source_owner_fixture_bytes() -> Vec<u8> {
    // Native header shape only; the explicit scripted transport never executes
    // this fixture. Real executable loading remains the bounded process owner.
    let mut bytes = if cfg!(target_os = "macos") { vec![0xcf, 0xfa, 0xed, 0xfe] }
        else { vec![0x7f, b'E', b'L', b'F'] };
    bytes.extend_from_slice(b"fixture bytes; never executed");
    bytes
}

#[test]
fn source_owner_native_headers_exclude_interpreter_scripts() {
    for header in [*b"\x7fELF", [0xcf, 0xfa, 0xed, 0xfe], [0xca, 0xfe, 0xba, 0xbe]] {
        assert!(engram_source_native_header(&header));
    }
    for header in [*b"#!/b", *b"#!/u", *b"echo", *b"text", [0; 4]] {
        assert!(!engram_source_native_header(&header));
    }
}

#[cfg(unix)]
#[test]
fn source_owner_unix_scripts_are_refused_before_probing_explicit_or_searched_paths() {
    use std::os::unix::fs::PermissionsExt;
    let root = TestTempRoot::create("source-owner-script");
    let project = root.join("project");
    let programs = root.join("programs");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&programs).unwrap();
    let script = programs.join("engram");
    fs::write(&script, b"#!/usr/bin/env sh\nprintf 'engram fixture'\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let deadline = || std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
    let error = engram_source_owner_metadata_with(&script, &project, &root, deadline(),
        |_, _, _, _, _| panic!("an interpreter script must not run")).unwrap_err();
    assert!(error.to_string().contains("interpreter scripts"), "{error:#}");
    let error = resolve_engram_source_program_in(FsPath::new("engram"), &project, &[programs], deadline()).unwrap_err();
    assert!(error.to_string().contains("interpreter scripts"), "{error:#}");
}

#[test]
fn source_owner_metadata_pins_bytes_and_actual_source_schema_without_claiming_a_verdict() {
    let root = TestTempRoot::create("source-owner-meta");
    let binary = source_owner_fixture_binary(&root);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut calls = 0;
    let pin = engram_source_owner_metadata_with(&binary, &root, &root, deadline,
        |actual, cwd, args, until, limit| {
            assert_eq!(actual, fs::canonicalize(&binary).unwrap());
            assert_eq!(cwd, root.as_ref());
            assert_eq!(until, deadline, "both calls share the shorter caller deadline");
            assert_eq!(limit, if calls == 0 { 4096 } else { ENGRAM_SOURCE_JSON_LIMIT + 1 });
            calls += 1;
            Ok(source_owner_reply(args))
        }).unwrap();
    assert_eq!(calls, 2);
    assert_eq!(pin.source_version, 1);
    assert!(pin.binary_version.contains("durable-store"));
    assert_eq!(pin.sha256, sha256_hex(&fs::read(&binary).unwrap()));
    let value = serde_json::to_value(&pin).unwrap();
    let keys: Vec<_> = value.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["binaryPath", "binaryVersion", "sha256", "sourceVersion"]);
    assert!(!value.to_string().contains("input_unavailable"));
}

#[test]
fn source_owner_metadata_refuses_missing_relative_replaced_and_expired_programs() {
    let root = TestTempRoot::create("source-owner-paths");
    let binary = source_owner_fixture_binary(&root);
    let future = || std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
    for path in [PathBuf::from("relative/engram"), root.join("missing.exe"), root.to_path_buf()] {
        let error = engram_source_owner_metadata_with(&path, &root, &root, future(),
            |_, _, _, _, _| panic!("an untrusted or absent path must not launch")).unwrap_err();
        assert!(!error.to_string().is_empty());
    }
    assert!(engram_source_owner_metadata_with(&binary, &root, &root, std::time::Instant::now(),
        |_, _, _, _, _| panic!("expired work must not launch")).is_err());
    let error = engram_source_owner_metadata_with(&binary, &root, &root, future(),
        |_, _, args, _, _| {
            if args[0] == "source" { fs::write(&binary, b"changed program").unwrap(); }
            Ok(source_owner_reply(args))
        }).unwrap_err();
    assert!(error.to_string().contains("changed during probing"), "{error:#}");
}

#[test]
fn source_owner_host_resolution_is_unique_native_and_never_uses_cwd_or_relative_path_entries() {
    let root = TestTempRoot::create("source-owner-search");
    let project = root.join("project");
    let first = root.join("first");
    let second = root.join("second");
    for dir in [&project, &first, &second] { fs::create_dir_all(dir).unwrap(); }
    let name = if cfg!(windows) { "engram.exe" } else { "engram" };
    fs::write(project.join(name), source_owner_fixture_bytes()).unwrap();
    let deadline = || std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
    let command = FsPath::new("engram");
    assert!(resolve_engram_source_program_in(command, &project, &[PathBuf::from("relative")], deadline()).is_err());
    fs::write(first.join(name), source_owner_fixture_bytes()).unwrap();
    let one = fs::canonicalize(first.join(name)).unwrap();
    assert_eq!(resolve_engram_source_program_in(command, &project, &[first.clone()], deadline()).unwrap(), one);
    assert_eq!(resolve_engram_source_program_in(command, &project, &[first.clone(), first.clone()], deadline()).unwrap(), one);
    fs::write(second.join(name), source_owner_fixture_bytes()).unwrap();
    let ambiguous = resolve_engram_source_program_in(command, &project, &[first, second], deadline()).unwrap_err();
    assert!(ambiguous.to_string().contains("ambiguous"), "{ambiguous:#}");
    let unsafe_project = resolve_engram_source_program_in(command, &project, &[project.clone()], deadline()).unwrap_err();
    assert!(unsafe_project.to_string().contains("inside the project"), "{unsafe_project:#}");
    // An explicit configured absolute pin is supported even on a runner where
    // the trusted native binary lives under that runner's repository.
    assert_eq!(resolve_engram_source_program_in(&one, &project, &[], deadline()).unwrap(), one);
}

#[test]
fn source_owner_metadata_refuses_process_diagnostics_bad_versions_and_bad_source_contracts() {
    let root = TestTempRoot::create("source-owner-output");
    let binary = source_owner_fixture_binary(&root);
    for case in ["version-exit", "version-stderr", "version-utf8", "version-limit", "version-text",
        "source-stderr", "source-exit", "source-version", "source-root", "source-limit", "timeout"] {
        let error = engram_source_owner_metadata_with(&binary, &root, &root,
            std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET,
            |_, _, args, _, _| {
                if case == "timeout" { return Err(io::Error::new(io::ErrorKind::TimedOut, "fixture deadline").into()); }
                let mut output = source_owner_reply(args);
                let version = args[0] == "--version";
                match (version, case) {
                    (true, "version-exit") => output.status = source_owner_output(1, vec![]).status,
                    (true, "version-stderr") | (false, "source-stderr") => output.stderr = b"warning".to_vec(),
                    (true, "version-utf8") => output.stdout = vec![0xff],
                    (true, "version-limit") => output.stdout = vec![b'x'; 4097],
                    (true, "version-text") => output.stdout = b"some other program".to_vec(),
                    (false, "source-exit") => output.status = source_owner_output(2, vec![]).status,
                    (false, "source-limit") => output.stdout = vec![b' '; ENGRAM_SOURCE_JSON_LIMIT + 2],
                    (false, "source-version") | (false, "source-root") => {
                        let mut value: Value = serde_json::from_slice(&output.stdout).unwrap();
                        if case == "source-version" { value["version"] = json!(2); }
                        else { value["root"] = json!(root.join("other")); }
                        output.stdout = serde_json::to_vec(&value).unwrap();
                    }
                    _ => {}
                }
                Ok(output)
            }).unwrap_err();
        assert!(!error.to_string().is_empty(), "{case}");
    }
}

fn metadata_state() -> (AppState, PathBuf, String, String) {
    let state = test_app_state();
    let root = state.test_temp_root.as_ref().unwrap().path().join("source-project");
    fs::create_dir_all(&root).unwrap();
    let binary = source_owner_fixture_binary(&root);
    let project = create_test_project(&state, &root, "source-owner");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    state.inner.lock().unwrap().projects.iter_mut().find(|p| p.id == project).unwrap().engram
        = Some(EngramProjectSettings { binary_path: Some(binary.to_str().unwrap().into()),
            ..EngramProjectSettings::default() });
    (state, root, project, session)
}

#[test]
fn source_owner_metadata_rechecks_the_host_derived_project_after_off_lock_probes() {
    let (state, _, project, session) = metadata_state();
    let deadline = || std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
    assert!(state.source_owner_metadata_with(&session, deadline(), |_, _, args, _, _| {
        assert!(state.inner.is_not_held_by_current_thread_for_test(), "process IO must not hold StateInner");
        Ok(source_owner_reply(args))
    }).is_ok());
    let error = state.source_owner_metadata_with(&session, deadline(), |_, _, args, _, _| {
        if args[0] == "source" {
            state.inner.lock().unwrap().projects.iter_mut().find(|p| p.id == project)
                .unwrap().engram.as_mut().unwrap().binary_path = Some("another-binary".into());
        }
        Ok(source_owner_reply(args))
    }).unwrap_err();
    assert!(error.message.contains("configuration changed"), "{error:?}");
    state.inner.lock().unwrap().sessions.iter_mut().find(|r| r.session.id == session).unwrap().hidden = true;
    assert!(state.source_owner_metadata_with(&session, deadline(),
        |_, _, _, _, _| panic!("hidden sessions cannot probe")).is_err());
}

#[test]
fn source_owner_metadata_excludes_children_remote_proxies_and_unconfigured_projects() {
    let (state, _, project, session) = metadata_state();
    let (_, child) = super::delegation_support::install_required_review_delegation(&state, &session);
    let until = || std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
    let error = state.source_owner_metadata_with(&child, until(),
        |_, _, _, _, _| panic!("a child cannot discover an owner")).unwrap_err();
    assert!(error.message.contains("native local root"), "{error:?}");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].remote_id = Some("remote".into());
        inner.sessions[index].remote_session_id = Some("remote-session".into());
    }
    let error = state.source_owner_metadata_with(&session, until(),
        |_, _, _, _, _| panic!("a proxy cannot discover an owner")).unwrap_err();
    assert!(error.message.contains("native local root"), "{error:?}");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].remote_id = None;
        inner.sessions[index].remote_session_id = None;
        inner.projects.iter_mut().find(|p| p.id == project).unwrap().engram = None;
    }
    let error = state.source_owner_metadata_with(&session, until(),
        |_, _, _, _, _| panic!("unconfigured projects cannot probe")).unwrap_err();
    assert!(error.message.contains("not configured"), "{error:?}");
}

#[tokio::test]
async fn source_owner_endpoint_rejects_caller_parameters_and_unknown_sessions() {
    let (state, _, _, session) = metadata_state();
    let app = Router::new().route("/api/sessions/{id}/source-owner", get(get_session_source_owner)).with_state(state);
    for (suffix, body) in [("?root=elsewhere", ""), ("", "{}"), ("?project=other", "")] {
        let response = request_response(&app, Request::builder()
            .uri(format!("/api/sessions/{session}/source-owner{suffix}"))
            .body(Body::from(body)).unwrap()).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let response = request_response(&app, Request::builder()
        .uri("/api/sessions/session-not-present/source-owner").body(Body::empty()).unwrap()).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
