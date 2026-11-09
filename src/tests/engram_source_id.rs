use super::engram_source_command::source_envelope;
use super::*;

fn decode(root: &FsPath, value: &Value) -> Result<EngramSourceObservation> {
    let code = if value["status"] == "unavailable" {
        1
    } else {
        0
    };
    parse_engram_source_observation(
        &serde_json::to_vec(value).unwrap(),
        Some(code),
        root,
        std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET,
    )
}

fn change(path: &str, status: &str) -> Value {
    json!({"path":path, "status":status, "origin":null,
        "blob":"c".repeat(40), "mode":"100644"})
}

#[test]
fn source_owner_identity_compares_the_whole_object_and_excludes_envelope_metadata() {
    let root = TestTempRoot::create("source-id-equality");
    let mut value = source_envelope(&root, "not_clean");
    value["source_id"]["submodules"] = json!([{"path":"child", "commit":"d".repeat(40),
        "changes":[change("file.txt", " M")]}]);
    let baseline = decode(&root, &value).unwrap().source_id.unwrap();
    assert_eq!(serde_json::to_value(&baseline).unwrap(), value["source_id"]);
    let mut metadata = value.clone();
    metadata["root"] = json!(root.join("."));
    metadata["entries"] = json!([{"status":" M", "path":"child"}]);
    assert_eq!(
        decode(&root, &metadata).unwrap().source_id.unwrap(),
        baseline
    );
    for case in [
        "root-commit",
        "root-tree",
        "blob",
        "mode",
        "path",
        "status",
        "child-commit",
        "child-path",
        "child-blob",
        "child-mode",
    ] {
        let mut changed = value.clone();
        match case {
            "root-commit" => {
                changed["source_id"]["commit"] = json!("e".repeat(40));
                changed["source"] = json!(format!("git-commit:{}", "e".repeat(40)));
            }
            "root-tree" => {
                changed["source_id"]["tree"] = json!("e".repeat(40));
                changed["tree"] = json!("e".repeat(40));
            }
            "blob" => changed["source_id"]["changes"][0]["blob"] = json!("e".repeat(40)),
            "mode" => changed["source_id"]["changes"][0]["mode"] = json!("100755"),
            "path" => changed["source_id"]["changes"][0]["path"] = json!("another.txt"),
            "status" => changed["source_id"]["changes"][0]["status"] = json!(" M"),
            "child-commit" => {
                changed["source_id"]["submodules"][0]["commit"] = json!("e".repeat(40))
            }
            "child-path" => changed["source_id"]["submodules"][0]["path"] = json!("nested/child"),
            "child-blob" => {
                changed["source_id"]["submodules"][0]["changes"][0]["blob"] = json!("e".repeat(40))
            }
            "child-mode" => {
                changed["source_id"]["submodules"][0]["changes"][0]["mode"] = json!("120000")
            }
            _ => unreachable!(),
        }
        assert_ne!(
            decode(&root, &changed).unwrap().source_id.unwrap(),
            baseline,
            "{case}"
        );
    }
}

#[test]
fn source_owner_identity_requires_every_nullable_key_and_rejects_unknown_nested_keys() {
    let root = TestTempRoot::create("source-id-closed");
    let mut value = source_envelope(&root, "not_clean");
    value["source_id"]["submodules"] = json!([{"path":"child", "commit":"d".repeat(40),
        "changes":[change("file.txt", " M")]}]);
    for pointer in [
        "/source_id",
        "/source_id/submodules/0",
        "/source_id/changes/0",
        "/source_id/submodules/0/changes/0",
    ] {
        let object = value.pointer(pointer).unwrap().as_object().unwrap();
        for key in object.keys() {
            let mut missing = value.clone();
            missing
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(key);
            assert!(decode(&root, &missing).is_err(), "{pointer}/{key}");
        }
        let mut extra = value.clone();
        extra.pointer_mut(pointer).unwrap()["extra"] = json!(true);
        assert!(decode(&root, &extra).is_err(), "{pointer}/extra");
    }
    let mut missing = value.clone();
    missing.as_object_mut().unwrap().remove("source_id");
    assert!(decode(&root, &missing).is_err());
    for source_id in [Value::Null, json!("a digest is not an object")] {
        let mut invalid = value.clone();
        invalid["source_id"] = source_id;
        assert!(decode(&root, &invalid).is_err());
    }
    let mut unavailable = source_envelope(&root, "unavailable");
    assert!(decode(&root, &unavailable).unwrap().source_id.is_none());
    unavailable["source_id"] = value["source_id"].clone();
    assert!(decode(&root, &unavailable).is_err());
}

#[test]
fn source_owner_identity_preserves_renames_tombstones_and_recreated_staged_deletions() {
    let root = TestTempRoot::create("source-id-changes");
    let mut value = source_envelope(&root, "not_clean");
    let mut deleted = change("gone.txt", " D");
    deleted["blob"] = Value::Null;
    deleted["mode"] = Value::Null;
    let mut renamed = change("new.txt", "R ");
    renamed["origin"] = json!("old.txt");
    let mut link = change("link", " M");
    link["mode"] = json!("120000");
    value["source_id"]["changes"] = json!([deleted, link, renamed]);
    let decoded = decode(&root, &value).unwrap().source_id.unwrap();
    assert!(decoded.changes[0].blob.is_none());
    assert_eq!(decoded.changes[1].mode, Some(EngramSourceIdMode::Symlink));
    assert_eq!(decoded.changes[2].origin.as_deref(), Some("old.txt"));
    let mut origin_changed = value.clone();
    origin_changed["source_id"]["changes"][2]["origin"] = json!("other.txt");
    assert_ne!(
        decode(&root, &origin_changed).unwrap().source_id.unwrap(),
        decoded
    );

    value["entries"] =
        json!([{"status":"D ","path":"file.txt"}, {"status":"??","path":"file.txt"}]);
    value["source_id"]["changes"] = json!([change("file.txt", "??"), change("file.txt", "D ")]);
    let recreated = decode(&root, &value).unwrap().source_id.unwrap();
    assert_eq!(recreated.changes.len(), 2);
    assert!(recreated.changes.iter().all(|change| change.blob.is_some()));
    value["source_id"]["changes"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert!(
        decode(&root, &value).is_err(),
        "identity order is validated, never repaired"
    );
}

#[test]
fn source_owner_identity_orders_literal_paths_by_utf8_bytes_including_nested_children() {
    let root = TestTempRoot::create("source-id-order");
    let mut value = source_envelope(&root, "not_clean");
    value["source_id"]["changes"] = json!([
        change("\"literal name", "??"),
        change("\u{e000}", "??"),
        change("\u{10000}", "??")
    ]);
    value["source_id"]["submodules"] = json!([
        {"path":"child", "commit":"c".repeat(40), "changes":[]},
        {"path":"child/nested", "commit":"d".repeat(40), "changes":[]}]);
    assert!(decode(&root, &value).is_ok());
    for pointer in ["/source_id/changes", "/source_id/submodules"] {
        let mut reversed = value.clone();
        reversed
            .pointer_mut(pointer)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .reverse();
        assert!(decode(&root, &reversed).is_err(), "{pointer}");
    }
}

#[test]
fn source_owner_identity_refuses_invalid_ids_paths_modes_and_partial_blob_pairs() {
    let root = TestTempRoot::create("source-id-invalid");
    let value = source_envelope(&root, "not_clean");
    for (key, invalid_value) in [
        ("blob", json!("C".repeat(40))),
        ("blob", json!("c".repeat(39))),
        ("blob", Value::Null),
        ("mode", Value::Null),
        ("mode", json!(100644)),
        ("mode", json!("160000")),
        ("origin", json!("unexpected")),
        ("path", json!("../escape")),
        ("path", json!("/absolute")),
        ("path", json!("bad\nname")),
        ("path", json!("bad\0name")),
        ("status", json!("!!")),
        ("status", json!("  ")),
    ] {
        let mut invalid = value.clone();
        invalid["source_id"]["changes"][0][key] = invalid_value;
        assert!(decode(&root, &invalid).is_err(), "{invalid}");
    }
    for key in ["commit", "tree"] {
        for id in ["c".repeat(40), "A".repeat(40), "a".repeat(64)] {
            let mut invalid = value.clone();
            invalid["source_id"][key] = json!(id);
            assert!(
                decode(&root, &invalid).is_err(),
                "provenance must agree: {key}"
            );
        }
    }
}
