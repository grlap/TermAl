// Tests ordinary content-v1 spelling admission and its sole new refusal.
// The copied pre-change capture supplies a runtime compatibility comparison;
// fixtures and Git operations remain owned by the parent test module.
include!("content_revision_legacy_reference.rs");

fn unverified_spelling_witness(message: &str) {
    use std::io::Write;
    writeln!(std::io::stderr(), "UNVERIFIED {message}").unwrap();
}

fn legacy_revision(root: &FsPath) -> Result<String> {
    legacy_capture_content_revision(&ReviewFreezeGit::new(root)?)
}

fn index_base_blob(root: &FsPath, path: &str) {
    let blob = run_git_test_command_output(root, &["rev-parse", ":tracked.txt"]);
    run_git_test_command(root, &["update-index", "--add", "--cacheinfo",
        &format!("100644,{},{}", blob.trim(), path)]);
}

#[cfg(windows)]
#[test]
fn merged_directory_spellings_admit_distinct_tracked_files() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    run_git_test_command(&root, &["config", "core.ignoreCase", "true"]);
    fs::create_dir_all(root.join("CompilerOptions/Fsc")).unwrap();
    fs::write(root.join("CompilerOptions/Fsc/first.txt"), "base\n").unwrap();
    // A real second spelling must resolve on this fixture volume. Unexpected
    // case-sensitive Windows volumes do not silently count as this witness.
    assert!(root.join("CompilerOptions/fsc").is_dir(),
        "criterion witness requires an insensitive Windows fixture volume");
    fs::write(root.join("CompilerOptions/fsc/second.txt"), "base\n").unwrap();
    index_base_blob(&root, "CompilerOptions/Fsc/first.txt");
    index_base_blob(&root, "CompilerOptions/fsc/second.txt");
    let old_error = legacy_revision(&root).unwrap_err().to_string();
    assert!(old_error.contains("differs in case"), "{old_error}");
    let admitted = revision(&root);
    fs::write(root.join("CompilerOptions/Fsc/first.txt"), "first edit\n").unwrap();
    assert_ne!(revision(&root), admitted);
    fs::write(root.join("CompilerOptions/Fsc/first.txt"), "base\n").unwrap();
    fs::write(root.join("CompilerOptions/fsc/second.txt"), "second edit\n").unwrap();
    assert_ne!(revision(&root), admitted);
    fs::write(root.join("CompilerOptions/fsc/second.txt"), "base\n").unwrap();
    assert_eq!(revision(&root), admitted);
}

fn collision_fixture(root: &FsPath, first: &str, second: &str) {
    // Git must retain both raw index spellings on macOS too.
    run_git_test_command(root, &["config", "core.precomposeUnicode", "false"]);
    fs::write(root.join(first), "base\n").unwrap();
    match fs::OpenOptions::new().write(true).create_new(true).open(root.join(second)) {
        Ok(file) => {
            drop(file);
            fs::remove_file(root.join(second)).unwrap();
            fs::hard_link(root.join(first), root.join(second)).unwrap();
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("fixture second spelling could not be created: {error}"),
    }
    index_base_blob(root, first);
    index_base_blob(root, second);
    let index = run_git_test_command_output(root, &["ls-files", "-z"]);
    assert!(index.split('\0').any(|path| path == first), "missing {first:?}: {index:?}");
    assert!(index.split('\0').any(|path| path == second), "missing {second:?}: {index:?}");
    let a = fs::File::open(root.join(first)).unwrap();
    let b = fs::File::open(root.join(second)).unwrap();
    assert_eq!(content_revision_file_key(&a, &a.metadata().unwrap()).unwrap(),
        content_revision_file_key(&b, &b.metadata().unwrap()).unwrap());
}

#[test]
fn tracked_case_collision_names_both_paths_even_with_equal_index_blobs() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    run_git_test_command(&root, &["config", "core.ignoreCase", "true"]);
    collision_fixture(&root, "A.txt", "a.txt");
    assert!(legacy_revision(&root).is_ok(), "this is the old final-name false match");
    let error = content_revision(&root).unwrap_err().to_string();
    assert!(error.contains("A.txt") && error.contains("a.txt") && error.contains("same file"), "{error}");
    let other_source = root.join(".git/other-source");
    fs::write(&other_source, "different index blob\n").unwrap();
    let other_blob = run_git_test_command_output(&root,
        &["hash-object", "-w", other_source.to_str().unwrap()]);
    fs::remove_file(other_source).unwrap();
    run_git_test_command(&root, &["update-index", "--cacheinfo",
        &format!("100644,{},a.txt", other_blob.trim())]);
    let error = content_revision(&root).unwrap_err().to_string();
    assert!(error.contains("A.txt") && error.contains("a.txt") && error.contains("same file"), "{error}");
    run_git_test_command(&root, &["update-index", "--force-remove", "A.txt"]);
    assert!(content_revision(&root).is_ok(), "an untracked alias is not a new refusal");
}

#[test]
fn tracked_normalization_collision_names_both_paths() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let (first, second) = ("caf\u{e9}.txt", "cafe\u{301}.txt");
    collision_fixture(&root, first, second);
    let error = content_revision(&root).unwrap_err().to_string();
    assert!(error.contains(&format!("{first:?}")) && error.contains(&format!("{second:?}"))
        && error.contains("same file"), "{error}");
}

#[test]
fn distinct_name_hard_links_keep_the_legacy_revision() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    fs::hard_link(root.join("tracked.txt"), root.join("another.txt")).unwrap();
    index_base_blob(&root, "another.txt");
    assert_eq!(revision(&root), legacy_revision(&root).unwrap());
    fs::hard_link(root.join("tracked.txt"), root.join("Stra\u{df}e.txt")).unwrap();
    assert_ne!(content_revision_spelling_key("Stra\u{df}e.txt"),
        content_revision_spelling_key("strasse.txt"));
    match fs::hard_link(root.join("tracked.txt"), root.join("strasse.txt")) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            unverified_spelling_witness("sharp-s distinct-name hard links: full-folding volume");
            return;
        }
        Err(error) => panic!("hard-link fixture failed: {error}"),
    }
    index_base_blob(&root, "Stra\u{df}e.txt");
    index_base_blob(&root, "strasse.txt");
    assert_eq!(revision(&root), legacy_revision(&root).unwrap(),
        "full-fold expansion must not create a new hard-link refusal");
    assert_eq!(content_revision_spelling_key("\u{fb05}.txt"),
        content_revision_spelling_key("\u{fb06}.txt"), "simple fold differs from lowercase");
}

#[test]
fn decomposed_simple_case_pairs_reach_the_real_collision_refusal() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let (first, second) = ("j\u{30c}.txt", "J\u{30c}.txt");
    assert_eq!(content_revision_spelling_key(first), content_revision_spelling_key(second));
    assert_eq!(content_revision_spelling_key("\u{1f0}.txt"),
        content_revision_spelling_key(second));
    assert_ne!(content_revision_spelling_key("q\u{3b9}\u{323}.txt"),
        content_revision_spelling_key("q\u{323}\u{3b9}.txt"),
        "folding must not turn a spacing iota into a reorderable combining mark");
    collision_fixture(&root, first, second);
    let error = content_revision(&root).unwrap_err().to_string();
    assert!(error.contains(&format!("{first:?}")) && error.contains(&format!("{second:?}"))
        && error.contains("same file"), "{error}");
}

#[cfg(unix)]
#[test]
fn case_variants_on_distinct_files_are_admitted_and_terminal_links_stay_links() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    fs::write(root.join("A.txt"), "base\n").unwrap();
    match fs::OpenOptions::new().write(true).create_new(true).open(root.join("a.txt")) {
        Ok(mut file) => { use std::io::Write; file.write_all(b"other\n").unwrap(); }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            unverified_spelling_witness("distinct-case-file witness: insensitive fixture volume");
            return;
        }
        Err(error) => panic!("fixture case variant failed: {error}"),
    }
    index_base_blob(&root, "A.txt");
    index_base_blob(&root, "a.txt");
    assert_eq!(revision(&root), legacy_revision(&root).unwrap());
    fs::remove_file(root.join("a.txt")).unwrap();
    std::os::unix::fs::symlink("A.txt", root.join("a.txt")).unwrap();
    assert_eq!(revision(&root), legacy_revision(&root).unwrap());
}

#[cfg(any(windows, unix))]
#[test]
fn a_redirecting_parent_still_refuses_a_tracked_file() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let target = repository(&mut cleanup);
    let link = root.join("redirect");
    #[cfg(windows)]
    {
        let output = Command::new("cmd").args(["/d", "/c", "mklink", "/J"])
            .arg(&link).arg(&target).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    index_base_blob(&root, "redirect/tracked.txt");
    let error = content_revision(&root).unwrap_err().to_string();
    #[cfg(windows)]
    fs::remove_dir(&link).unwrap();
    #[cfg(unix)]
    fs::remove_file(&link).unwrap();
    assert!(error.contains("leads elsewhere"), "{error}");
}

#[cfg(any(windows, unix))]
#[test]
fn spelling_equivalent_parent_redirects_refuse_at_every_ancestor() {
    for nested in [false, true] {
        let mut cleanup = RemoveDirsOnDrop(Vec::new());
        let root = repository(&mut cleanup);
        run_git_test_command(&root, &["config", "core.precomposeUnicode", "false"]);
        #[cfg(windows)]
        let (target_name, link_name) = ("caf\u{e9}", "cafe\u{301}");
        #[cfg(unix)]
        let (target_name, link_name) = ("foo", "Foo");
        let target = root.join(target_name);
        let link = root.join(link_name);
        fs::create_dir(&target).unwrap();
        if link.exists() {
            unverified_spelling_witness("case-equivalent redirect: insensitive Unix fixture volume");
            return;
        }
        let suffix = if nested { "nested/tracked.txt" } else { "tracked.txt" };
        if nested { fs::create_dir(target.join("nested")).unwrap(); }
        fs::write(target.join(suffix), "base\n").unwrap();
        crate::tests::engram_child_temp::link_directory(&link, &target);
        let listed = format!("{link_name}/{suffix}");
        index_base_blob(&root, &listed);
        let canonical_root = fs::canonicalize(&root).unwrap();
        let parent = canonical_root.join(&listed).parent().unwrap().to_path_buf();
        let canonical_parent = fs::canonicalize(&parent).unwrap();
        assert_ne!(parent, canonical_parent, "must enter the widened branch");
        assert_eq!(content_revision_spelling_key(parent.to_str().unwrap()),
            content_revision_spelling_key(canonical_parent.to_str().unwrap()));
        let error = content_revision_path(&canonical_root, &listed,
            &mut std::collections::HashSet::new()).unwrap_err().to_string();
        let capture_error = content_revision(&root).unwrap_err().to_string();
        #[cfg(windows)]
        fs::remove_dir(&link).unwrap();
        #[cfg(unix)]
        fs::remove_file(&link).unwrap();
        assert!(error.contains("through a link or junction"), "{error}");
        assert!(capture_error.contains("through a link or junction"), "{capture_error}");
    }
}

#[test]
fn ordinary_values_match_the_legacy_algorithm_across_index_and_content_states() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    // Independently framed schema + .gitignore + tracked.txt fixture bytes;
    // neither a runtime baseline generation nor an index/commit identity.
    assert_eq!(revision(&root),
        "content-v1:35df390d4d41e4dbdc7e204ae9c950f73390bf48437b0486a2033ec865b84e1c");
    assert_eq!(revision(&root), legacy_revision(&root).unwrap());
    fs::write(root.join("tracked.txt"), "edited\r\n").unwrap();
    fs::write(root.join("untracked.bin"), b"\0binary\r\n").unwrap();
    assert_eq!(revision(&root), legacy_revision(&root).unwrap());
    run_git_test_command(&root, &["update-index", "--assume-unchanged", "tracked.txt"]);
    assert_eq!(revision(&root), legacy_revision(&root).unwrap());
    run_git_test_command(&root, &["update-index", "--no-assume-unchanged", "tracked.txt"]);
    run_git_test_command(&root, &["update-index", "--skip-worktree", "tracked.txt"]);
    assert_eq!(revision(&root), legacy_revision(&root).unwrap());
    run_git_test_command(&root, &["update-index", "--no-skip-worktree", "tracked.txt"]);
    fs::remove_file(root.join("tracked.txt")).unwrap();
    assert_eq!(revision(&root), legacy_revision(&root).unwrap());
}

#[test]
fn conflict_stages_keep_the_legacy_content_value() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let before = revision(&root);
    let blob = run_git_test_command_output(&root, &["rev-parse", ":tracked.txt"]);
    run_git_test_command(&root, &["update-index", "--force-remove", "tracked.txt"]);
    let input = root.join(".git/conflict-input");
    fs::write(&input, format!("100644 {} 1\ttracked.txt\n100644 {} 2\ttracked.txt\n100644 {} 3\ttracked.txt\n",
        blob.trim(), blob.trim(), blob.trim())).unwrap();
    let output = Command::new("git").current_dir(&root)
        .args(["update-index", "--index-info"])
        .stdin(std::process::Stdio::from(fs::File::open(&input).unwrap()))
        .output().unwrap();
    fs::remove_file(input).unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let stages = run_git_test_command_output(&root, &["ls-files", "--unmerged"]);
    assert_eq!(stages.lines().count(), 3, "{stages}");
    assert_eq!(revision(&root), before);
    assert_eq!(revision(&root), legacy_revision(&root).unwrap());
}

#[test]
fn sparse_checkout_keeps_legacy_absence_and_index_flag_treatment() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    for directory in ["kept", "dropped"] {
        fs::create_dir(root.join(directory)).unwrap();
        fs::write(root.join(directory).join("source.txt"), "base\n").unwrap();
    }
    run_git_test_command(&root, &["add", "kept/source.txt", "dropped/source.txt"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "sparse fixture"]);
    run_git_test_command(&root, &["sparse-checkout", "init", "--cone"]);
    run_git_test_command(&root, &["sparse-checkout", "set", "kept"]);
    assert!(!root.join("dropped/source.txt").exists());
    let flags = run_git_test_command_output(&root, &["ls-files", "-v"]);
    assert!(flags.contains("S dropped/source.txt"), "{flags}");
    assert_eq!(revision(&root), legacy_revision(&root).unwrap());
}

#[cfg(windows)]
#[test]
fn distinct_normalization_spelled_files_keep_the_legacy_value() {
    use std::io::Write;
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    run_git_test_command(&root, &["config", "core.precomposeUnicode", "false"]);
    let (first, second) = ("caf\u{e9}.txt", "cafe\u{301}.txt");
    fs::write(root.join(first), "base\n").unwrap();
    match fs::OpenOptions::new().write(true).create_new(true).open(root.join(second)) {
        Ok(mut file) => file.write_all(b"other\n").unwrap(),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            unverified_spelling_witness("distinct normalization names: normalizing volume");
            return;
        }
        Err(error) => panic!("normalization fixture failed: {error}"),
    }
    index_base_blob(&root, first);
    index_base_blob(&root, second);
    let a = fs::File::open(root.join(first)).unwrap();
    let b = fs::File::open(root.join(second)).unwrap();
    assert_ne!(content_revision_file_key(&a, &a.metadata().unwrap()).unwrap(),
        content_revision_file_key(&b, &b.metadata().unwrap()).unwrap());
    assert_eq!(revision(&root), legacy_revision(&root).unwrap());
}

#[cfg(unix)]
#[test]
fn a_candidate_renamed_after_its_read_is_rechecked_by_file_identity() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    fs::write(root.join("A.txt"), "base\n").unwrap();
    match fs::OpenOptions::new().write(true).create_new(true).open(root.join("a.txt")) {
        Ok(mut file) => { use std::io::Write; file.write_all(b"base\n").unwrap(); }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            unverified_spelling_witness("identity-swap witness: insensitive fixture volume");
            return;
        }
        Err(error) => panic!("fixture case variant failed: {error}"),
    }
    index_base_blob(&root, "A.txt");
    index_base_blob(&root, "a.txt");
    let original = revision(&root);
    let hook_root = root.clone();
    install_test_review_freeze_before_open(fs::canonicalize(&root).unwrap().join("a.txt"), move || {
        fs::rename(hook_root.join("A.txt"), hook_root.join("old-file")).unwrap();
        fs::write(hook_root.join("A.txt"), "base\n").unwrap();
    });
    let error = content_revision(&root).unwrap_err().to_string();
    assert!(error.contains("A.txt") && error.contains("changed during capture"), "{error}");
    fs::remove_file(root.join("A.txt")).unwrap();
    fs::rename(root.join("old-file"), root.join("A.txt")).unwrap();
    assert_eq!(revision(&root), original);
}
