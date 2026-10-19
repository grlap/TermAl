//! Real compiled CLI regression suite. Does not execute repository helpers or
//! open a live TermAl store. Engine/API behavior is exercised without new
//! production symbols, so the absent feature is a runtime RED, not a compiler error.
#[path = "support/review_identity_fixture.rs"]
mod fixture;
#[path = "support/review_identity_questly_fixture.rs"]
mod questly_fixture;

use fixture::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;

fn capture_request() -> Value {
    json!({"operation":"capture","schemaVersion":1})
}

#[test]
fn review_identity_unborn_capture_and_exact_check_use_the_same_profile() {
    let f = Fixture::new();
    let capture = f.capture();
    assert_eq!(capture["counts"]["repositories"], 1);
    assert_eq!(capture["verified"], Value::Null);
    let checked = f.check(capture["fingerprint"].as_str().unwrap());
    assert_eq!(checked["verified"], true);
    assert_eq!(capture["contentDigest"], checked["contentDigest"]);
}

#[test]
fn review_identity_keeps_staged_bytes_separate_from_raw_unstaged_binary_bytes() {
    let f = Fixture::new();
    f.put("binary", b"base");
    f.commit();
    f.put("binary", b"abc");
    git(&f.root, &["add", "--", "binary"], None);
    f.put("binary", &[0, 255, 13, 10, 0]);
    let result = f.capture();
    // Git SHA-1 of the independent literal object bytes "blob 3\0abc".
    // Index identity is OID+size; raw worktree identity is SHA-256.
    assert_eq!(
        entry(&result, "binary", "index")["oid"],
        "f2ba8f84ab5c1bce84a7b441cb1959cfc7093b7f"
    );
    assert_eq!(entry(&result, "binary", "index")["size"], 3);
    assert!(
        entry(&result, "binary", "index")
            .get("contentSha256")
            .is_none()
    );
    assert_eq!(
        entry(&result, "binary", "worktree")["contentSha256"],
        format!("{:x}", Sha256::digest([0, 255, 13, 10, 0]))
    );
    assert_eq!(entry(&result, "binary", "worktree")["size"], 5);
}

#[test]
fn review_identity_tracks_crlf_bytes_even_when_git_normalization_hides_them() {
    let f = Fixture::new();
    f.put(".gitattributes", b"* text=auto\n");
    f.put("tracked", b"a\nb\n");
    f.commit();
    let before = f.capture();
    f.put("tracked", b"a\r\nb\r\n");
    let after = f.capture();
    assert_ne!(before["fingerprint"], after["fingerprint"]);
    assert_eq!(entry(&after, "tracked", "worktree")["size"], 6);
}

#[test]
fn review_identity_reads_assume_unchanged_files_instead_of_stat_clean_candidates() {
    let f = Fixture::new();
    f.put("tracked", b"old");
    f.commit();
    git(
        &f.root,
        &["update-index", "--assume-unchanged", "tracked"],
        None,
    );
    let before = f.capture();
    f.put("tracked", b"new");
    let after = f.capture();
    assert_ne!(before["fingerprint"], after["fingerprint"]);
    assert_eq!(
        entry(&after, "tracked", "worktree")["contentSha256"],
        format!("{:x}", Sha256::digest(b"new"))
    );
}

#[test]
fn review_identity_reads_skip_worktree_files_and_intent_to_add_content() {
    let f = Fixture::new();
    f.put("tracked", b"old");
    f.commit();
    git(
        &f.root,
        &["update-index", "--skip-worktree", "tracked"],
        None,
    );
    let before = f.capture();
    f.put("tracked", b"new");
    f.put("intent", b"not an empty indexed blob");
    git(&f.root, &["add", "--intent-to-add", "--", "intent"], None);
    let after = f.capture();
    assert_ne!(before["fingerprint"], after["fingerprint"]);
    assert_eq!(entry(&after, "intent", "worktree")["size"], 25);
    assert!(
        entries(&after)
            .iter()
            .any(|e| e["path"] == "intent" && e["layer"] == "index")
    );
}

#[test]
fn review_identity_hashes_raw_untracked_bytes_and_ignores_only_declared_ignored_files() {
    let f = Fixture::new();
    f.put(".gitignore", b"ignored\n");
    f.commit();
    f.put("raw", &[255, 254, 0, 13, 10]);
    let before = f.capture();
    assert_eq!(
        entry(&before, "raw", "worktree")["contentSha256"],
        format!("{:x}", Sha256::digest([255, 254, 0, 13, 10]))
    );
    f.put("ignored", b"excluded by repository rule");
    assert_eq!(before["fingerprint"], f.capture()["fingerprint"]);
    f.put("raw", &[255, 254, 0, 10]);
    assert_ne!(before["fingerprint"], f.capture()["fingerprint"]);
}

#[test]
fn review_identity_host_discovery_preserves_negated_and_all_ignored_directory_semantics() {
    let f = Fixture::new();
    f.put(".gitignore", b"visible/*\n!visible/keep\nempty/*\n");
    f.commit();
    f.put("visible/keep", b"included");
    f.put("visible/drop", b"ignored");
    let before = f.capture();
    assert_eq!(entry(&before, "visible/keep", "worktree")["size"], 8);
    assert!(
        !entries(&before)
            .iter()
            .any(|entry| entry["path"] == "visible/drop")
    );
    f.put("empty/drop", b"all contents ignored");
    f.put("visible/drop", b"a different ignored value");
    assert_eq!(before["fingerprint"], f.capture()["fingerprint"]);
}

#[test]
fn review_identity_host_discovery_keeps_children_of_a_tracked_blob_replaced_by_directory() {
    let f = Fixture::new();
    f.put("changed", b"tracked blob");
    f.commit();
    fs::remove_file(f.root.join("changed")).unwrap();
    f.put("changed/raw", b"untracked child");
    let before = f.capture();
    assert_eq!(
        entry(&before, "changed", "worktree")["kind"],
        "directory-boundary"
    );
    assert_eq!(entry(&before, "changed/raw", "worktree")["size"], 15);
    f.put("changed/raw", b"a changed child");
    assert_ne!(before["fingerprint"], f.capture()["fingerprint"]);
}

#[test]
fn review_identity_keeps_all_unmerged_index_stages() {
    let f = Fixture::new();
    let a = git(&f.root, &["hash-object", "-w", "--stdin"], Some(b"base"));
    let b = git(&f.root, &["hash-object", "-w", "--stdin"], Some(b"ours"));
    let c = git(&f.root, &["hash-object", "-w", "--stdin"], Some(b"theirs"));
    let index = format!("100644 {a} 1\tconflict\n100644 {b} 2\tconflict\n100644 {c} 3\tconflict\n");
    git(
        &f.root,
        &["update-index", "--index-info"],
        Some(index.as_bytes()),
    );
    f.put("conflict", b"unresolved");
    let result = f.capture();
    let stages: Vec<_> = entries(&result)
        .iter()
        .filter(|e| e["layer"] == "index" && e["path"] == "conflict")
        .map(|e| e["stage"].as_u64().unwrap())
        .collect();
    assert_eq!(stages, [1, 2, 3]);
}

#[test]
fn review_identity_covers_dirty_initialized_and_deeply_nested_submodules() {
    let f = Fixture::new();
    let (outer, outer_git) = child(&f.root, &f.root.join(".git"), "vendor");
    let (deep, _) = child(&outer, &outer_git, "deep");
    commit(&outer);
    let oid = git(&outer, &["rev-parse", "HEAD"], None);
    git(
        &f.root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{oid},vendor"),
        ],
        None,
    );
    f.commit();
    let before = f.capture();
    assert_eq!(before["counts"]["repositories"], 3);
    put(&deep, "tracked", b"dirty below the first submodule");
    let after = f.capture();
    assert_ne!(before["fingerprint"], after["fingerprint"]);
    assert_eq!(
        entry(&after, "vendor/deep/tracked", "worktree")["contentSha256"],
        format!("{:x}", Sha256::digest(b"dirty below the first submodule"))
    );
}

#[test]
fn review_identity_records_gitlink_oid_separately_from_the_checked_out_child() {
    let f = Fixture::new();
    let (worktree, _) = child(&f.root, &f.root.join(".git"), "vendor");
    let old = git(&worktree, &["rev-parse", "HEAD"], None);
    f.commit();
    let before = f.capture();
    put(&worktree, "new", b"second child commit");
    commit(&worktree);
    let new = git(&worktree, &["rev-parse", "HEAD"], None);
    git(
        &f.root,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{new},vendor"),
        ],
        None,
    );
    let after = f.capture();
    assert_ne!(before["fingerprint"], after["fingerprint"]);
    assert_eq!(entry(&after, "vendor", "head")["oid"], old);
    assert_eq!(entry(&after, "vendor", "index")["oid"], new);
    assert_eq!(entry(&after, "vendor", "index")["mode"], "160000");
}

#[test]
fn review_identity_retains_a_removed_gitlinks_dirty_checkout_as_embedded_input() {
    let f = Fixture::new();
    let (worktree, _) = child(&f.root, &f.root.join(".git"), "vendor");
    f.commit();
    git(&f.root, &["rm", "--cached", "--", "vendor"], None);
    let before = f.capture();
    put(&worktree, "tracked", b"retained and dirty");
    let after = f.capture();
    assert_eq!(after["counts"]["repositories"], 2);
    assert_ne!(before["fingerprint"], after["fingerprint"]);
    assert_eq!(entry(&after, "vendor", "head")["mode"], "160000");
}

#[test]
fn review_identity_uninitialized_gitlinks_have_an_explicit_node_without_fetching() {
    let f = Fixture::new();
    let (worktree, gitdir) = child(&f.root, &f.root.join(".git"), "vendor");
    f.commit();
    fs::remove_file(worktree.join("tracked")).unwrap();
    fs::remove_file(worktree.join(".git")).unwrap();
    fs::remove_dir(&worktree).unwrap();
    let absent = f.capture();
    assert_eq!(
        entry(&absent, "vendor", "worktree")["kind"],
        "gitlink-uninitialized"
    );
    fs::create_dir(&worktree).unwrap();
    let empty = f.capture();
    assert_eq!(
        entry(&empty, "vendor", "worktree")["kind"],
        "gitlink-uninitialized"
    );
    assert!(gitdir.is_dir(), "no initialization or metadata mutation");
}

#[test]
fn review_identity_nonempty_uninitialized_checkout_refuses_instead_of_omitting_bytes() {
    let f = Fixture::new();
    let (worktree, _) = child(&f.root, &f.root.join(".git"), "vendor");
    fs::remove_file(worktree.join(".git")).unwrap();
    refused(
        &f.root,
        capture_request(),
        "uninitialized_submodule_contents",
    );
}

#[test]
fn review_identity_recurses_into_nonignored_untracked_embedded_git_directories() {
    let f = Fixture::new();
    let nested = f.root.join("embedded");
    fs::create_dir(&nested).unwrap();
    git(&nested, &["init", "--object-format=sha1"], None);
    put(&nested, "file", b"one");
    commit(&nested);
    let before = f.capture();
    assert_eq!(before["counts"]["repositories"], 2);
    put(&nested, "untracked", b"raw inner bytes");
    assert_ne!(before["fingerprint"], f.capture()["fingerprint"]);
}

#[test]
fn review_identity_refuses_object_alternates_even_when_the_alternate_is_local() {
    let f = Fixture::new();
    let alternate = f.home.join("other-objects");
    fs::create_dir(&alternate).unwrap();
    f.put(
        ".git/objects/info/alternates",
        alternate.to_str().unwrap().as_bytes(),
    );
    refused(&f.root, capture_request(), "alternates_unsupported");
}

#[test]
fn review_identity_refuses_a_submodule_gitdir_outside_the_owners_modules_tree() {
    let f = Fixture::new();
    let (worktree, _) = child(&f.root, &f.root.join(".git"), "vendor");
    let foreign = f.home.join("foreign-git");
    fs::create_dir(&foreign).unwrap();
    put(
        &worktree,
        ".git",
        format!("gitdir: {}\n", foreign.display()).as_bytes(),
    );
    refused(&f.root, capture_request(), "git_metadata_escape");
}

#[test]
fn review_identity_refuses_configured_root_filters_without_executing_them() {
    let f = Fixture::new();
    f.put("tracked", b"data");
    f.commit();
    let marker = f.home.join("filter-ran");
    git(
        &f.root,
        &[
            "config",
            "filter.identity.clean",
            &format!("echo invoked > {}", marker.display()),
        ],
        None,
    );
    f.put(".gitattributes", b"* filter=identity\n");
    refused(&f.root, capture_request(), "configured_filter_unsupported");
    assert!(!marker.exists());
}

#[test]
fn review_identity_checks_filters_again_inside_each_nested_repository() {
    let f = Fixture::new();
    let (worktree, _) = child(&f.root, &f.root.join(".git"), "vendor");
    let marker = f.home.join("nested-filter-ran");
    git(
        &worktree,
        &[
            "config",
            "filter.identity.process",
            &format!("echo invoked > {}", marker.display()),
        ],
        None,
    );
    put(&worktree, ".gitattributes", b"* filter=identity\n");
    refused(&f.root, capture_request(), "configured_filter_unsupported");
    assert!(!marker.exists());
}

#[test]
fn review_identity_missing_promisor_objects_refuse_without_transport() {
    let f = Fixture::new();
    git(
        &f.root,
        &[
            "update-index",
            "--add",
            "--info-only",
            "--cacheinfo",
            "100644,ffffffffffffffffffffffffffffffffffffffff,missing",
        ],
        None,
    );
    git(&f.root, &["config", "remote.origin.promisor", "true"], None);
    git(
        &f.root,
        &["config", "remote.origin.url", "ext::must-never-run"],
        None,
    );
    refused(&f.root, capture_request(), "missing_local_object");
}

#[test]
fn review_identity_checks_refuse_another_platform_and_never_downgrade() {
    let f = Fixture::new();
    let capture = f.capture();
    refused(
        &f.root,
        json!({
            "operation":"check", "schemaVersion":1,
            "expectedFingerprint":capture["fingerprint"],
            "platformProfile":if cfg!(windows) { "unix" } else { "windows" }
        }),
        "platform_profile_mismatch",
    );
}

#[test]
fn review_identity_unknown_schema_and_expected_mismatch_have_named_refusals() {
    let f = Fixture::new();
    refused(
        &f.root,
        json!({"operation":"capture","schemaVersion":2}),
        "unsupported_schema",
    );
    refused(
        &f.root,
        json!({
            "operation":"check","schemaVersion":1,
            "expectedFingerprint":"0".repeat(64), "platformProfile":platform()
        }),
        "fingerprint_mismatch",
    );
}

#[test]
fn review_identity_pages_are_complete_and_bound_to_the_checked_fingerprint() {
    let f = Fixture::new();
    for i in 0..4 {
        f.put(&format!("untracked-{i}"), &[i]);
    }
    let first = successful(
        &f.root,
        json!({"operation":"capture","schemaVersion":1,"pageLimit":1}),
    );
    let total = first["inventory"]["totalRecords"].as_u64().unwrap() as usize;
    let mut seen = entries(&first).to_vec();
    let mut cursor = first["inventory"]["nextCursor"].as_str().map(str::to_owned);
    while let Some(next) = cursor {
        let page = successful(
            &f.root,
            json!({
                "operation":"check","schemaVersion":1,"platformProfile":platform(),
                "expectedFingerprint":first["fingerprint"],"pageLimit":1,"cursor":next
            }),
        );
        assert_eq!(page["verified"], true);
        assert_eq!(page["inventoryDigest"], first["inventoryDigest"]);
        seen.extend_from_slice(entries(&page));
        assert!(seen.len() <= total, "pagination must advance");
        cursor = page["inventory"]["nextCursor"].as_str().map(str::to_owned);
    }
    assert_eq!(seen.len(), total);
    assert_eq!(seen.iter().filter(|e| e["layer"] == "worktree").count(), 4);
    let stale_cursor = first["inventory"]["nextCursor"].clone();
    f.put("untracked-0", b"changed after page one");
    refused(
        &f.root,
        json!({
            "operation":"check","schemaVersion":1,"platformProfile":platform(),
            "expectedFingerprint":first["fingerprint"],"cursor":stale_cursor
        }),
        "fingerprint_mismatch",
    );
}

#[test]
fn review_identity_windows_and_unix_mode_coverage_are_explicit() {
    let f = Fixture::new();
    f.put("file", b"abc");
    git(&f.root, &["add", "--", "file"], None);
    let result = f.capture();
    assert_eq!(entry(&result, "file", "index")["mode"], "100644");
    let mode = &entry(&result, "file", "worktree")["fsMode"];
    #[cfg(windows)]
    {
        assert_eq!(mode, &json!({"windows":null}));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            mode,
            &json!({"unix":fs::metadata(f.root.join("file")).unwrap().permissions().mode() & 0o7777})
        );
    }
}

#[test]
fn review_identity_bounds_nested_repository_depth_without_silently_stopping() {
    let f = Fixture::new();
    let mut root = f.root.clone();
    for _ in 0..9 {
        root = root.join("nested");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "--object-format=sha1"], None);
    }
    refused(&f.root, capture_request(), "input_limit_exceeded");
}

#[test]
fn review_identity_composite_uses_independently_framed_root_and_platform_fields() {
    let f = Fixture::new();
    f.put("nonempty", b"abc");
    let actual = f.capture();
    let mut reference = Sha256::new();
    // This is an independent protocol oracle, not a production hash helper.
    // Each field is u64-BE label length, u64-BE payload length, label, payload.
    for (label, payload) in [
        ("domain", "termal-review-identity-v1"),
        ("schema", "1"),
        ("root", actual["canonicalRoot"].as_str().unwrap()),
        ("content", actual["contentDigest"].as_str().unwrap()),
        ("platform", platform()),
    ] {
        reference.update((label.len() as u64).to_be_bytes());
        reference.update((payload.len() as u64).to_be_bytes());
        reference.update(label.as_bytes());
        reference.update(payload.as_bytes());
    }
    assert_eq!(actual["fingerprint"], format!("{:x}", reference.finalize()));
    assert_eq!(
        entry(&actual, "nonempty", "worktree")["contentSha256"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[cfg(windows)]
#[test]
fn review_identity_windows_junctions_refuse_without_traversing_them() {
    use std::os::windows::process::CommandExt;
    let f = Fixture::new();
    let outside = f.home.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"outside reviewed root").unwrap();
    let junction = f.root.join("junction");
    let created = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .creation_flags(0x08000000)
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let output = invoke(&f.root, capture_request());
    fs::remove_dir(&junction).unwrap();
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["reason"]["code"], "link_kind_unsupported");
    assert_eq!(result["complete"], false);
    assert_eq!(
        fs::read(outside.join("sentinel")).unwrap(),
        b"outside reviewed root"
    );
}

#[cfg(windows)]
#[test]
fn review_identity_ignored_junctions_are_pruned_but_tracked_descendants_still_refuse() {
    use std::os::windows::process::CommandExt;
    let f = Fixture::new();
    f.put(".gitignore", b"ignored/\n");
    f.commit();
    let before = f.capture();
    let outside = f.home.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join(".git"), b"must not be probed").unwrap();
    fs::write(outside.join("sentinel"), b"outside reviewed root").unwrap();
    let junction = f.root.join("ignored");
    let created = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .creation_flags(0x08000000)
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    assert_eq!(before["fingerprint"], f.capture()["fingerprint"]);
    let oid = git(&f.root, &["hash-object", "-w", "--stdin"], Some(b"tracked"));
    git(
        &f.root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{oid},ignored/sentinel"),
        ],
        None,
    );
    let output = invoke(&f.root, capture_request());
    fs::remove_dir(&junction).unwrap();
    assert!(!output.status.success());
    let refused: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(refused["reason"]["code"], "ancestor_link");
    assert_eq!(
        fs::read(outside.join("sentinel")).unwrap(),
        b"outside reviewed root"
    );
}

#[cfg(unix)]
#[test]
fn review_identity_literal_colon_names_cannot_hide_behind_pathspec_magic() {
    let f = Fixture::new();
    f.put(".gitignore", b"foo\n:ignored\n");
    f.commit();
    f.put(":ignored", b"ignored literal name");
    let before = f.capture();
    f.put(":ignored", b"different ignored bytes");
    assert_eq!(before["fingerprint"], f.capture()["fingerprint"]);
    for name in [":foo", "::foo", ":(top)foo", ":!x", ":^x"] {
        f.put(name, b"nonignored raw input");
        refused(&f.root, capture_request(), "unsafe_path");
        fs::remove_file(f.root.join(name)).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn review_identity_hashes_symlink_targets_and_never_follows_outside_targets() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let outside = f.home.join("outside");
    fs::write(&outside, b"secret").unwrap();
    symlink(&outside, f.root.join("link")).unwrap();
    let before = f.capture();
    assert_eq!(entry(&before, "link", "worktree")["kind"], "symlink");
    assert_eq!(
        entry(&before, "link", "worktree")["target"],
        outside.to_str().unwrap()
    );
    fs::write(&outside, b"changed outside input").unwrap();
    assert_eq!(before["fingerprint"], f.capture()["fingerprint"]);
}

#[test]
fn review_identity_oversized_raw_payload_is_a_named_refusal() {
    let f = Fixture::new();
    let file = fs::File::create(f.root.join("oversized")).unwrap();
    file.set_len(128 * 1024 * 1024 + 1).unwrap();
    drop(file);
    refused(&f.root, capture_request(), "input_limit_exceeded");
}

#[test]
fn review_identity_exclusion_policy_bytes_cannot_hide_themselves() {
    let f = Fixture::new();
    f.put(".gitignore", b".gitignore\nignored\n");
    let before = f.capture();
    f.put(".gitignore", b".gitignore\nignored\n# policy changed\n");
    assert_ne!(before["fingerprint"], f.capture()["fingerprint"]);
    fs::create_dir_all(f.root.join(".git/info")).unwrap();
    fs::write(f.root.join(".git/info/exclude"), b"elsewhere\n").unwrap();
    let before = f.capture();
    fs::write(
        f.root.join(".git/info/exclude"),
        b"elsewhere\n# metadata exclusion changed\n",
    )
    .unwrap();
    assert_ne!(before["fingerprint"], f.capture()["fingerprint"]);
}

#[test]
fn review_identity_cursor_for_another_root_is_refused_without_a_partial_inventory() {
    let first = Fixture::new();
    first.put("one", b"one");
    let second = Fixture::new();
    second.put("two", b"two");
    let page = successful(
        &first.root,
        json!({"operation":"capture","schemaVersion":1,"pageLimit":1}),
    );
    let capture = second.capture();
    refused(
        &second.root,
        json!({"operation":"check","schemaVersion":1,"platformProfile":platform(),
        "expectedFingerprint":capture["fingerprint"],"cursor":page["inventory"]["nextCursor"]}),
        "stale_cursor",
    );
}

#[test]
fn review_identity_repository_count_is_bounded_even_without_gitlinks() {
    let f = Fixture::new();
    for index in 0..64 {
        let nested = f.root.join(format!("nested-{index}"));
        fs::create_dir(&nested).unwrap();
        git(&nested, &["init", "--object-format=sha1"], None);
        put(&nested, "file", b"one");
    }
    refused(&f.root, capture_request(), "input_limit_exceeded");
}

#[test]
fn review_identity_repeated_physical_repository_is_a_cycle_refusal() {
    let f = Fixture::new();
    let (first, gitdir) = child(&f.root, &f.root.join(".git"), "first");
    let oid = git(&first, &["rev-parse", "HEAD"], None);
    let second = f.root.join("second");
    fs::create_dir(&second).unwrap();
    fs::write(
        second.join(".git"),
        format!("gitdir: {}\n", gitdir.display()),
    )
    .unwrap();
    put(&second, "tracked", b"second worktree");
    git(
        &f.root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{oid},second"),
        ],
        None,
    );
    refused(&f.root, capture_request(), "repository_cycle");
}

#[test]
fn review_identity_case_collision_uses_the_platform_coverage_profile() {
    let f = Fixture::new();
    git(&f.root, &["config", "core.ignorecase", "false"], None);
    let oid = git(&f.root, &["hash-object", "-w", "--stdin"], Some(b"one"));
    git(
        &f.root,
        &["update-index", "--index-info"],
        Some(format!("100644 {oid}\tname\n100644 {oid}\tNAME\n").as_bytes()),
    );
    f.put("name", b"one");
    f.put("NAME", b"one");
    if cfg!(windows) {
        refused(&f.root, capture_request(), "path_collision");
    } else {
        let names = fs::read_dir(&f.root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name() == "name" || e.file_name() == "NAME")
            .count();
        if names != 2 {
            refused(&f.root, capture_request(), "path_collision");
        } else {
            let result = f.capture();
            assert!(entries(&result).iter().any(|e| e["path"] == "name"));
            assert!(entries(&result).iter().any(|e| e["path"] == "NAME"));
        }
    }
}

#[test]
fn review_identity_questly_fixture_keeps_all_input_layers_and_nested_bytes() {
    let fixture = questly_fixture::QuestlyFixture::new(3);
    let capture = fixture.repo.capture();
    assert_eq!(capture["counts"]["repositories"], 3);
    assert_eq!(entry(&capture, "lib/main.dart", "index")["size"], 29);
    assert_eq!(entry(&capture, "lib/main.dart", "worktree")["size"], 32);
    assert_eq!(
        entry(&capture, "lib/main.dart", "worktree")["contentSha256"],
        format!(
            "{:x}",
            Sha256::digest(b"void main() { /* unstaged */ }\r\n")
        )
    );
    assert_eq!(
        entry(&capture, "docs/compliance/evidence/export.bin", "worktree")["size"],
        6
    );
    assert_eq!(entry(&capture, "rincon-common", "index")["mode"], "160000");
    assert_eq!(
        entry(&capture, "rincon-common/packages/domain-core", "index")["mode"],
        "160000"
    );
    assert!(entries(&capture).iter().all(|entry| {
        !entry["path"].as_str().unwrap().starts_with("build/")
            && !entry["path"].as_str().unwrap().starts_with("node_modules/")
    }));
    let checked = fixture.repo.check(capture["fingerprint"].as_str().unwrap());
    assert_eq!(checked["verified"], true);
    put(&fixture.deep, "tracked", b"a second nested revision\n");
    assert_ne!(
        capture["fingerprint"],
        fixture.repo.capture()["fingerprint"]
    );
}

#[test]
fn review_identity_questly_capacity_covers_ten_thousand_files_without_relaxing_limits() {
    let fixture = questly_fixture::QuestlyFixture::new(10_000);
    let started = std::time::Instant::now();
    let capture = fixture.repo.capture();
    let elapsed = started.elapsed();
    assert_eq!(capture["complete"], true);
    assert_eq!(capture["counts"]["repositories"], 3);
    assert!(capture["counts"]["files"].as_u64().unwrap() >= 10_010);
    let records = capture["inventory"]["totalRecords"].as_u64().unwrap();
    assert!((30_000..35_000).contains(&records));
    assert!(serde_json::to_vec(&capture["inventory"]).unwrap().len() <= 64 * 1024);
    // A successful actual compiled worker is itself bounded by its unchanged
    // 40-second deadline; this timer is a diagnostic, never a looser timeout.
    eprintln!(
        "Questly-shaped capacity: platform={} files={} records={} capture_ms={}",
        platform(),
        capture["counts"]["files"],
        records,
        elapsed.as_millis()
    );
}

#[test]
#[ignore = "Required explicit solo capacity measurement; no concurrent gate or warm build"]
fn review_identity_questly_census_payload_fits_the_unchanged_double_capture_deadline() {
    use questly_fixture::QuestlyCensusFixture;
    let fixture = QuestlyCensusFixture::new();
    let project = fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
    let scratch = fs::canonicalize(project.join(".tmp")).unwrap();
    assert_eq!(scratch, project.join(".tmp"));
    assert!(
        fs::canonicalize(&fixture.repo.home)
            .unwrap()
            .starts_with(&scratch)
    );
    let started = std::time::Instant::now();
    let output = invoke(&fixture.repo.root, capture_request());
    let elapsed = started.elapsed();
    let result: Value =
        serde_json::from_slice(&output.stdout).expect("structured census observation");
    eprintln!(
        "QUESTLY_CAPACITY_OBSERVATION {}",
        json!({
            "fixture":"historical-census-sized synthetic payload",
            "platform":platform(),"wallMs":elapsed.as_millis(),"nativeExit":output.status.code(),
            "complete":result["complete"],"reason":result["reason"],
            "counts":result["counts"],"rootFiles":QuestlyCensusFixture::ROOT_FILES,
            "rootBytes":QuestlyCensusFixture::ROOT_BYTES,
            "childFiles":QuestlyCensusFixture::CHILD_FILES,"childBytes":QuestlyCensusFixture::CHILD_BYTES,
            "largestRootBlob":6065957,"largestChildBlob":1366771,
        })
    );
    assert!(
        output.status.success(),
        "census capture refused: {}",
        result["reason"]
    );
    assert!(
        elapsed < std::time::Duration::from_secs(40),
        "census wall time: {elapsed:?}"
    );
    assert_eq!(result["complete"], true);
    assert_eq!(result["counts"]["repositories"], 2);
    assert!(result["counts"]["files"].as_u64().unwrap() >= 3770);
    assert!(result["counts"]["rawBytes"].as_u64().unwrap() >= 728_619_336);
}

#[test]
fn review_identity_sha256_object_format_identifies_index_content_without_rehashing_it() {
    let fixture = Fixture::new();
    let root = fixture.home.join("sha256");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "--object-format=sha256"], None);
    put(&root, "file", b"abc");
    git(&root, &["add", "--", "file"], None);
    let result = successful(&root, capture_request());
    assert_eq!(result["objectFormat"], "sha256");
    assert_eq!(
        entry(&result, "file", "index")["oid"],
        format!("{:x}", Sha256::digest(b"blob 3\0abc"))
    );
    assert_eq!(entry(&result, "file", "index")["size"], 3);
    assert!(
        entry(&result, "file", "index")
            .get("contentSha256")
            .is_none()
    );
}

#[test]
fn review_identity_index_flags_are_independent_of_worktree_content() {
    let fixture = Fixture::new();
    fixture.put("tracked", b"abc");
    fixture.commit();
    let before = fixture.capture();
    git(
        &fixture.root,
        &["update-index", "--assume-unchanged", "tracked"],
        None,
    );
    let after = fixture.capture();
    assert_ne!(
        entry(&before, "tracked", "index")["flags"],
        entry(&after, "tracked", "index")["flags"]
    );
    assert_ne!(before["fingerprint"], after["fingerprint"]);
    assert_eq!(
        entry(&before, "tracked", "worktree")["contentSha256"],
        entry(&after, "tracked", "worktree")["contentSha256"]
    );
}
