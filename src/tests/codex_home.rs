use super::*;

#[test]
fn mailbox_wake_codex_instructions_teach_read_first_once() {
    let section = termal_codex_agents_section();
    assert_eq!(section.matches(TERMAL_MAILBOX_GUIDANCE).count(), 1);
    assert!(section.contains("`afterSequence` omitted"));
    assert!(section.contains("mailbox read --mailbox-id <id> --json"));
    assert!(section.contains("senderProcessedThrough"));
    assert!(section.contains("expectedProcessedThrough"));
    assert!(!section.contains("1. Run `mailbox list"));
    assert!(!section.contains("--after <processedThrough>"));
}

#[test]
fn codex_home_appends_one_managed_coordination_section_after_user_instructions() {
    let root = TestTempRoot::create("termal-codex-home-agents");
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir_all(&source).expect("source Codex home should be created");
    fs::write(
        source.join("AGENTS.md"),
        "# User instructions\n\nKeep this user-owned rule.\n",
    )
    .expect("source AGENTS.md should write");

    seed_termal_codex_home_from(&source, &target).expect("Codex home should seed");
    let first =
        fs::read_to_string(target.join("AGENTS.md")).expect("managed AGENTS.md should be readable");
    assert!(first.starts_with("# User instructions\n\nKeep this user-owned rule.\n\n"));
    assert!(first.contains("TERMAL_SESSION_ID"));
    assert!(first.contains("mailbox acknowledge"));
    assert_eq!(first.matches(TERMAL_CODEX_AGENTS_SECTION_START).count(), 1);

    seed_termal_codex_home_from(&target, &target)
        .expect("refreshing an already managed Codex home should succeed");
    let refreshed = fs::read_to_string(target.join("AGENTS.md"))
        .expect("refreshed AGENTS.md should be readable");
    assert_eq!(refreshed, first);
    assert_eq!(
        refreshed.matches(TERMAL_CODEX_AGENTS_SECTION_START).count(),
        1,
        "refresh must replace the managed section instead of duplicating it"
    );
}

#[test]
fn codex_home_writes_coordination_instructions_without_a_source_home() {
    let root = TestTempRoot::create("termal-codex-home-agents-missing-source");
    let source = root.path().join("missing-source");
    let target = root.path().join("target");

    seed_termal_codex_home_from(&source, &target)
        .expect("a missing source home must not suppress managed instructions");
    let agents =
        fs::read_to_string(target.join("AGENTS.md")).expect("managed AGENTS.md should be created");
    assert!(agents.starts_with(TERMAL_CODEX_AGENTS_SECTION_START));
    assert!(agents.contains("TERMAL_CLI"));
    assert!(agents.contains("stable idempotencyKey"));
}

#[test]
fn missing_source_preserves_target_user_instructions_and_deduplicates_managed_sections() {
    let root = TestTempRoot::create("termal-codex-home-agents-preserve-target");
    let source = root.path().join("missing-source");
    let target = root.path().join("target");
    fs::create_dir_all(&target).expect("target Codex home should be created");
    let section = termal_codex_agents_section();
    fs::write(
        target.join("AGENTS.md"),
        format!("# Existing user instructions\n\n{section}\n\n{section}\n"),
    )
    .expect("existing target AGENTS.md should write");

    seed_termal_codex_home_from(&source, &target)
        .expect("a missing source should preserve the existing managed home");
    let agents =
        fs::read_to_string(target.join("AGENTS.md")).expect("managed AGENTS.md should be readable");
    assert!(agents.starts_with("# Existing user instructions\n\n"));
    assert_eq!(agents.matches(TERMAL_CODEX_AGENTS_SECTION_START).count(), 1);
    assert_eq!(agents.matches(TERMAL_CODEX_AGENTS_SECTION_END).count(), 1);
}

#[test]
fn unterminated_managed_marker_remains_user_text_across_reseeds() {
    let root = TestTempRoot::create("termal-codex-home-agents-unterminated-marker");
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir_all(&source).expect("source Codex home should be created");
    let user_contents = format!(
        "# User instructions\n\n{TERMAL_CODEX_AGENTS_SECTION_START}\nThis unterminated remainder is user text.\n"
    );
    fs::write(source.join("AGENTS.md"), &user_contents)
        .expect("unterminated source AGENTS.md should write");

    seed_termal_codex_home_from(&source, &target).expect("Codex home should seed");
    let first =
        fs::read_to_string(target.join("AGENTS.md")).expect("managed AGENTS.md should be readable");
    assert!(first.starts_with(&format!("{}\n", user_contents.trim_end())));
    assert!(first.contains("This unterminated remainder is user text."));
    assert_eq!(first.matches(TERMAL_CODEX_AGENTS_SECTION_START).count(), 2);
    assert_eq!(first.matches(TERMAL_CODEX_AGENTS_SECTION_END).count(), 1);

    seed_termal_codex_home_from(&target, &target)
        .expect("refreshing the managed home should preserve the unterminated user marker");
    let refreshed = fs::read_to_string(target.join("AGENTS.md"))
        .expect("refreshed AGENTS.md should be readable");
    assert_eq!(refreshed, first);
}
