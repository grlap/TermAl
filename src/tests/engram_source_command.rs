use super::*;

pub(super) fn source_envelope(root: &FsPath, status: &str) -> Value {
    let mut value = json!({
        "version": 1, "status": status, "root": engram_exact_path_key(root),
        "source": format!("git-commit:{}", "a".repeat(40)), "tree": "b".repeat(40),
        "entries": [], "refusal": null,
        "source_id": {"commit":"a".repeat(40), "tree":"b".repeat(40),
            "submodules":[], "changes":[]},
    });
    if status == "not_clean" {
        value["entries"] = json!([{ "status": "??", "path": "new file.txt" }]);
        value["source_id"]["changes"] = json!([{"path":"new file.txt", "status":"??",
            "origin":null, "blob":"c".repeat(40), "mode":"100644"}]);
    } else if status == "unavailable" {
        value["source"] = Value::Null;
        value["tree"] = Value::Null;
        value["source_id"] = Value::Null;
        value["refusal"] = json!({ "code": "input_unavailable", "message": "cannot read input" });
    }
    value
}

fn decode(root: &FsPath, value: &Value, code: Option<i32>) -> Result<EngramSourceObservation> {
    parse_engram_source_observation(&serde_json::to_vec(value).unwrap(), code, root,
        std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET)
}

#[test]
fn source_owner_envelopes_preserve_clean_dirty_and_exit_one_refusals() {
    let root = TestTempRoot::create("source-envelope");
    let clean = decode(&root, &source_envelope(&root, "clean"), Some(0)).unwrap();
    assert!(clean.entries.is_empty());
    assert_eq!(clean.source_id.unwrap().commit, "a".repeat(40));
    let dirty = decode(&root, &source_envelope(&root, "not_clean"), Some(0)).unwrap();
    assert_eq!(dirty.entries.len(), 1);
    assert_eq!(dirty.entries[0].status, "??");
    assert_eq!(dirty.entries[0].path, "new file.txt");
    assert!(dirty.entries[0].origin.is_none());
    assert_eq!(dirty.source_id.unwrap().changes.len(), 1);
    for code in ["input_unavailable", "deadline", "output_limit"] {
        let mut value = source_envelope(&root, "unavailable");
        value["root"] = Value::Null; // A refused root need not have been established.
        value["refusal"]["code"] = json!(code);
        let unavailable = decode(&root, &value, Some(1)).unwrap();
        assert_eq!(unavailable.status, EngramSourceStatus::Unavailable);
        assert!(unavailable.source_id.is_none());
        assert!(unavailable.refusal.unwrap().message.contains("cannot read input"));
    }
}

#[test]
fn source_owner_envelopes_refuse_schema_root_and_exit_incoherence() {
    let root = TestTempRoot::create("source-envelope-invalid");
    let clean = source_envelope(&root, "clean");
    assert!(decode(&root, &clean, Some(0)).is_ok());
    for field in ["version", "status", "root", "source", "tree", "entries", "refusal", "source_id"] {
        let mut missing = clean.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(decode(&root, &missing, Some(0)).is_err(), "missing {field}");
    }
    for (field, value) in [
        ("version", json!(2)), ("status", json!("unknown")), ("root", Value::Null),
        ("root", json!("relative")), ("root", json!(engram_exact_path_key(&root.join("other")))),
        ("source", Value::Null), ("source", json!(format!("content-v1:{}", "a".repeat(64)))),
        ("source", json!(format!("git-commit:{}", "a".repeat(64)))),
        ("tree", json!("B".repeat(40))), ("entries", json!([{"status":"??","path":"x"}])),
        ("refusal", json!({"code":"deadline","message":"expired"})), ("extra", json!(true)),
    ] {
        let mut invalid = clean.clone();
        invalid[field] = value;
        assert!(decode(&root, &invalid, Some(0)).is_err(), "{invalid}");
    }
    for code in [None, Some(1), Some(2), Some(128)] {
        assert!(decode(&root, &clean, code).is_err(), "exit {code:?}");
    }
    let unavailable = source_envelope(&root, "unavailable");
    for (field, value) in [
        ("source", json!(format!("git-commit:{}", "a".repeat(40)))),
        ("tree", json!("b".repeat(40))), ("entries", json!([{"status":"??","path":"x"}])),
        ("refusal", Value::Null),
        ("refusal", json!({"code":"new_code","message":"unknown"})),
        ("refusal", json!({"code":"deadline","message":""})),
        ("refusal", json!({"code":"deadline","message":"x".repeat(4097)})),
        ("refusal", json!({"code":"deadline","message":"expired","extra":true})),
    ] {
        let mut invalid = unavailable.clone();
        invalid[field] = value;
        assert!(decode(&root, &invalid, Some(1)).is_err(), "field {field}");
    }
    assert!(decode(&root, &unavailable, Some(0)).is_err());
    assert!(decode(&root, &unavailable, Some(2)).is_err());
}

#[test]
fn source_owner_entries_require_a_valid_rename_origin_and_closed_fields() {
    let root = TestTempRoot::create("source-entry");
    let mut dirty = source_envelope(&root, "not_clean");
    dirty["entries"] = json!([{ "status": "R ", "path": "new.txt", "origin": "old.txt" }]);
    let observation = decode(&root, &dirty, Some(0)).unwrap();
    assert_eq!(observation.entries[0].origin.as_deref(), Some("old.txt"));
    for entry in [
        json!({"status":"R ","path":"new.txt"}),
        json!({"status":"??","path":"x","origin":"old.txt"}),
        json!({"status":"R ","path":"new.txt","origin":""}),
        json!({"status":"??","path":"x","extra":true}),
        json!({"status":"??","path":""}), json!({"status":"??","path":"bad\u{0}name"}),
        json!({"status":"  ","path":"x"}), json!({"status":"Z?","path":"x"}),
    ] {
        let mut invalid = dirty.clone();
        invalid["entries"] = json!([entry]);
        assert!(decode(&root, &invalid, Some(0)).is_err(), "{invalid}");
    }
}

#[test]
fn source_owner_parser_is_bounded_utf8_strict_and_deadline_checked() {
    let root = TestTempRoot::create("source-envelope-bounds");
    let clean = serde_json::to_vec(&source_envelope(&root, "clean")).unwrap();
    let future = || std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
    assert!(parse_engram_source_observation(&[clean.clone(), vec![b'\n']].concat(), Some(0), &root, future()).is_ok());
    for bytes in [vec![b' '; ENGRAM_SOURCE_JSON_LIMIT + 2], b"not JSON".to_vec(), vec![0xff]] {
        assert!(parse_engram_source_observation(&bytes, Some(0), &root, future()).is_err());
    }
    let duplicate = String::from_utf8(clean.clone()).unwrap().replace("\"version\":1", "\"version\":1,\"version\":1");
    assert!(parse_engram_source_observation(duplicate.as_bytes(), Some(0), &root, future()).is_err());
    assert!(parse_engram_source_observation(&clean, Some(0), &root, std::time::Instant::now()).is_err());
}

#[test]
fn source_owner_reply_root_is_the_same_directory_despite_a_different_spelling() {
    let root = TestTempRoot::create("source-root-spelling");
    let mut clean = source_envelope(&root, "clean");
    clean["root"] = json!(root.join(".").to_str().unwrap());
    assert!(decode(&root, &clean, Some(0)).is_ok());
    let other = root.join("other");
    fs::create_dir(&other).unwrap();
    clean["root"] = json!(other);
    assert!(decode(&root, &clean, Some(0)).is_err());
    #[cfg(target_os = "macos")]
    {
        // Read existing OS aliases; create nothing outside the repository.
        let canonical = fs::canonicalize("/var").unwrap();
        let mut value = source_envelope(&canonical, "clean");
        value["root"] = json!(normalize_user_facing_path(&canonical));
        assert!(decode(&canonical, &value, Some(0)).is_ok());
    }
}
