//! Native Engram-command fixture for Stop-ordering tests: readiness and
//! work-context reads. It no longer emulates `authority revoke`, which TermAl
//! does not send. Real PowerShell and shell CLI conformance remains in the
//! Engram host-adapter fixture tests.

use super::*;


#[test]
fn authority_command() {
    let Some(output) = std::env::var_os("TERMAL_TEST_AUTHORITY_FIXTURE") else {
        return;
    };
    let args: Vec<String> = std::env::args()
        .skip_while(|arg| arg != "--")
        .skip(1)
        .collect();
    assert!(args.len() >= 6, "Engram fixture argv: {args:?}");
    assert_eq!(args[0], "--project-file");
    assert_eq!(args[2], "--home");
    let root = FsPath::new(&args[3]);
    assert_eq!(
        fs::canonicalize(&args[1]).expect("fixture project file should exist"),
        fs::canonicalize(root.join(".engram-project")).expect("home's project file should exist")
    );
    let mode = fs::read_to_string(&args[1]).expect("fixture project declaration");
    assert_eq!(mode.trim(), "fixture-ready");
    if args[4..] == ["readiness", "--json"] {
        let database = work_database_path(root, mode.trim());
        fs::create_dir_all(database.parent().unwrap()).unwrap();
        if !database.exists() {
            fs::write(&database, "fixture database").unwrap();
        }
        let response = serde_json::json!({
            "schema_version": 1, "scope": "readiness", "ready": true,
            "full_audit": "not_run", "mutation_enabled": false, "work_schema_version": 1,
            "host_path_policy": {"stored":"fixture", "resolved":"fixture", "status":"matched"},
            "control": { "required_assurance": "turn_gated" },
            "database": database,
            "project_id": mode.trim(),
        });
        fs::write(output, serde_json::to_vec(&response).unwrap()).unwrap();
        return;
    }
    if args[4] == "work" {
        assert_eq!(args.len(), 15, "work-context argv: {args:?}");
        assert_eq!(args[5], "--actor-id");
        assert_eq!(args[7], "--session-id");
        assert_eq!(args[9], "--actor-context");
        assert_eq!(&args[11..14], ["next", "--peek", "--context-generation"]);
        assert!(
            args[14]
                .strip_prefix("termal-")
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|generation| generation > 0),
            "work-context generation: {:?}",
            args[14]
        );
        for (name, value) in [
            ("ENGRAM_HOME", &args[3]),
            ("ENGRAM_ACTOR_ID", &args[6]),
            ("ENGRAM_SESSION_ID", &args[8]),
            ("ENGRAM_ACTOR_CONTEXT", &args[10]),
        ] {
            assert_eq!(std::env::var(name).as_ref(), Ok(value));
        }
        fs::write(
            output,
            format!("Engram work context for {} as {}\n", args[8], args[6]),
        )
        .unwrap();
        return;
    }
    // TermAl sends no `authority revoke` (Engram removed work-authority
    // grants); that or any other command reaching this fixture is a test error.
    panic!("unexpected Engram fixture argv: {args:?}");
}
