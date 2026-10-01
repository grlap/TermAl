// Owns the proofs of the host architecture's first extraction
// (docs/features/host-architecture.md, section 9.5) that hold from its first
// commits on.
//
// The end-to-end proof: a recognised test driven through the recorder closes
// its turn with the checkpoint request the extraction's base commit sent for
// the same input. The request is compared with a fixture captured at that
// base, before any code moved: its observations and its evidence in the byte
// order the idempotency key hashes, the key itself, and the whole request.
// The base is commit cac212ce8d710ee2a7aee087b7f2b42fe463e123, the tip of
// `master` when the extraction began; the correctness fixes that landed after
// the brief was written are part of it. The execution record does not exist
// at the base, so its assertion joins this test in the commit that introduces
// it.
//
// The boundary of the facade: no source file outside the Engram host
// component names a function of the check machinery; the rest of the host
// reaches it through `EngramHost` (src/engram_host.rs).
//
// Does not own what a check is or when it is credited
// (src/tests/engram_turn_checks.rs, whose `CheckedTurn` fixture this child
// module uses). New module; nothing was split out of another file.
use super::*;

/// The request at the extraction's base, with the values that are not the
/// same wherever and whenever the test runs replaced by the placeholders this
/// test writes.
const BASE_CHECKPOINT: &str = include_str!("fixtures/engram-first-extraction-checkpoint.txt");

/// When the check completed, in place of the time the host stamped.
const CHECK_COMPLETED_AT: &str = "<CHECK-COMPLETED-AT>";
/// When the turn's own observation was taken, likewise.
const TURN_OBSERVED_AT: &str = "<TURN-OBSERVED-AT>";

/// `text` with three values replaced by placeholders: the worktree root the
/// run's repository happened to be created at; the environment fingerprint,
/// which hashes that root; and the content revision, which is the same on
/// every run of this repository on one machine but has not been shown equal
/// on every platform a checkout may run the test on. Each is checked on its
/// own before it is replaced. The stamped times are not replaced here: they
/// are relabelled by the record that carries them, so that two equal times
/// cannot be confused.
fn canonical(text: &str, workspace: &str, revision: &str, environment_fingerprint: &str) -> String {
    // A path appears in JSON text with its backslashes escaped.
    let workspace = serde_json::to_string(workspace).expect("a path should serialize");
    text.replace(workspace.trim_matches('"'), "<WORKSPACE>")
        .replace(revision, "<REVISION>")
        .replace(environment_fingerprint, "<ENVIRONMENT-FINGERPRINT>")
}

#[test]
fn a_test_driven_through_the_recorder_checkpoints_as_it_did_at_the_extractions_base() {
    let turn = CheckedTurn::start("first-extraction", true);
    let revision = turn.revision();
    turn.record_mut(|record| {
        record.active_codex_sandbox_mode = Some(CodexSandboxMode::WorkspaceWrite);
    });
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let checkpoint = turn.finish();

    // The records as typed, so that they serialize in the field order the
    // host sends and hashes, which a `Value` does not keep.
    let observations: Vec<EngramExecutionObservationInput> =
        serde_json::from_value(checkpoint["observations"].clone()).expect("observations");
    let verification: Vec<EngramVerificationEvidenceInput> =
        serde_json::from_value(checkpoint["verification_evidence"].clone())
            .expect("verification evidence");
    let environment: Vec<EngramEnvironmentEvidenceInput> =
        serde_json::from_value(checkpoint["environment_evidence"].clone())
            .expect("environment evidence");
    assert_eq!(observations.len(), 2, "the check, then the turn's own");
    assert_eq!(environment.len(), 1, "one check, one environment");

    // The key is its base, then the hash of the observations, then the hash
    // of the evidence, each over exactly the bytes the host sent.
    let key = checkpoint["idempotency_key"]
        .as_str()
        .expect("a checkpoint has a key");
    let observations_hash =
        sha256_hex(&serde_json::to_vec(&observations).expect("observations serialize"));
    let evidence_hash = sha256_hex(
        &serde_json::to_vec(&(&verification, &environment)).expect("evidence serializes"),
    );
    let key_base = key
        .strip_suffix(&format!(
            ":observations:{observations_hash}:evidence:{evidence_hash}"
        ))
        .unwrap_or_else(|| panic!("{key} should end with the hashes of what it reports"));

    // One worktree at one revision throughout, and that worktree is this
    // run's repository, named as a measurement of it names it: the root is
    // checked here, on its own, before the comparison replaces it.
    let workspace = observations[0]
        .source_basis
        .as_ref()
        .expect("the check has a basis")
        .workspace_id
        .clone();
    let (measured_root, _) = content_revision(&turn.root).expect("the root should be measured");
    assert_eq!(
        workspace,
        measured_root.to_string_lossy(),
        "the root in the form a measurement of the repository gives it"
    );
    assert_eq!(
        fs::canonicalize(&workspace).expect("the reported root exists"),
        fs::canonicalize(&turn.root).expect("the repository exists"),
        "the reported root is the repository the test ran in"
    );
    for basis in observations
        .iter()
        .filter_map(|observation| observation.source_basis.as_ref())
        .chain(environment.iter().map(|environment| &environment.source_basis))
    {
        assert_eq!(basis.workspace_id, workspace, "one worktree");
        assert_eq!(basis.source_revision, revision, "nothing changed");
    }
    // The fingerprint hashes the components, the root among them, so it is
    // checked against them here and compared as a placeholder.
    let environment_fingerprint = environment[0].environment_fingerprint.clone();
    assert_eq!(
        environment_fingerprint,
        engram_environment_fingerprint(
            environment[0]
                .components
                .as_ref()
                .expect("the environment names its components")
        )
    );
    // The times, by the record that carries each. The environment is timed
    // with its check. No order is asserted between the check's time and the
    // turn's: both are wall-clock stamps, and the host promises none.
    let check_completed_at = observations[0]
        .observed_at
        .clone()
        .expect("the check is timed");
    assert!(
        observations[1].observed_at.is_some(),
        "the turn's observation is timed"
    );
    assert_eq!(environment[0].observed_at, check_completed_at);
    let mut labelled_observations = observations.clone();
    labelled_observations[0].observed_at = Some(CHECK_COMPLETED_AT.to_owned());
    labelled_observations[1].observed_at = Some(TURN_OBSERVED_AT.to_owned());
    let mut labelled_environment = environment.clone();
    labelled_environment[0].observed_at = CHECK_COMPLETED_AT.to_owned();
    let mut labelled_request = checkpoint.clone();
    labelled_request["observations"][0]["observed_at"] = json!(CHECK_COMPLETED_AT);
    labelled_request["observations"][1]["observed_at"] = json!(TURN_OBSERVED_AT);
    labelled_request["environment_evidence"][0]["observed_at"] = json!(CHECK_COMPLETED_AT);

    // A `Value` serializes its maps in key order (serde_json without
    // `preserve_order`), so the request block below is alphabetical; the two
    // blocks above it carry the order the host sends.
    let request = serde_json::to_string_pretty(&labelled_request)
        .expect("the request serializes")
        .replace(&observations_hash, "<OBSERVATIONS-SHA256>")
        .replace(&evidence_hash, "<EVIDENCE-SHA256>");
    let actual = canonical(
        &format!(
            "key base: {key_base}\n\nobservations, as hashed:\n{}\n\nevidence, as hashed:\n{}\n\nrequest:\n{request}\n",
            serde_json::to_string(&labelled_observations).expect("observations serialize"),
            serde_json::to_string(&(&verification, &labelled_environment))
                .expect("evidence serializes"),
        ),
        &workspace,
        &revision,
        &environment_fingerprint,
    );
    assert!(
        !actual.contains("first-extraction-project"),
        "a path of this run is left in the comparison:\n{actual}"
    );
    // The fixture is text in the repository, so a checkout may give it CRLF.
    let base = BASE_CHECKPOINT.replace("\r\n", "\n");
    assert!(
        actual.trim_end() == base.trim_end(),
        "the checkpoint request differs from the one the extraction's base sent.\n\
         --- now ---\n{actual}\n--- at the base ---\n{base}"
    );
}

/// The fragments that are the check machinery: what a check is, how it is
/// recognised, marked, carried and reported.
const CHECK_MACHINERY_FILES: [&str; 5] = [
    "engram_turn_checks.rs",
    "engram_carried_checks.rs",
    "engram_check_recognition.rs",
    "engram_launcher_stages.rs",
    "engram_one_call.rs",
];

/// The name of the function a line defines, if it defines one.
fn defined_function(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    // A definition, not prose: only qualifiers may precede `fn`.
    let name = loop {
        if let Some(name) = rest.strip_prefix("fn ") {
            break name;
        }
        let (word, after) = rest.split_once(' ')?;
        if !(word.starts_with("pub") || matches!(word, "async" | "const" | "unsafe")) {
            return None;
        }
        rest = after;
    };
    let end = name
        .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .unwrap_or(name.len());
    (end > 0).then(|| &name[..end])
}

/// Each use in `text` of a name in `machinery`, with its line number.
fn machinery_names_in(text: &str, machinery: &BTreeSet<String>) -> Vec<(usize, String)> {
    let mut named = Vec::new();
    for (index, line) in text.lines().enumerate() {
        for word in
            line.split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        {
            if machinery.contains(word) {
                named.push((index + 1, word.to_owned()));
            }
        }
    }
    named
}

#[test]
fn no_file_outside_the_component_names_a_function_of_the_check_machinery() {
    let source = FsPath::new(env!("CARGO_MANIFEST_DIR")).join("src");
    // Every function the machinery defines whose name says whose it is. A
    // method of one of its types with a general name (`refusal_line`) cannot
    // be told from another type's by its name; reaching those types is the
    // matter of the state they sit in, not of this boundary.
    let mut machinery = BTreeSet::new();
    for file in CHECK_MACHINERY_FILES {
        let text = fs::read_to_string(source.join(file))
            .unwrap_or_else(|error| panic!("{file} should be readable: {error}"));
        machinery.extend(
            text.lines()
                .filter_map(defined_function)
                .filter(|name| name.contains("engram"))
                .map(str::to_owned),
        );
    }
    assert!(
        machinery.contains("note_engram_command_started")
            && machinery.contains("engram_note_turn_started")
            && machinery.contains("poll_engram_carried_runs")
            && machinery.len() > 100,
        "the machinery's functions should be found: {} were",
        machinery.len()
    );
    // The scan finds a call and a mention alike, and nothing in a line that
    // names none.
    assert_eq!(
        machinery_names_in(
            "let x = 1;\n    state.note_engram_host_write(&path);\n// see `engram_note_turn_started`\n",
            &machinery
        ),
        [
            (2, "note_engram_host_write".to_owned()),
            (3, "engram_note_turn_started".to_owned())
        ]
    );

    // The component is the Engram fragments and the facade; its tests live
    // under src/tests and are not source files of the host.
    let mut named = Vec::new();
    for entry in fs::read_dir(&source).expect("the source directory should be readable") {
        let path = entry.expect("a source entry").path();
        let Some(file) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !path.is_file() || !file.ends_with(".rs") || file.starts_with("engram_") {
            continue;
        }
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{file} should be readable: {error}"));
        named.extend(
            machinery_names_in(&text, &machinery)
                .into_iter()
                .map(|(line, name)| format!("{file}:{line}: {name}")),
        );
    }
    assert!(
        named.is_empty(),
        "these lines name a function of the check machinery; reach it through \
         `EngramHost` (src/engram_host.rs):\n{}",
        named.join("\n")
    );
}
