// Owns the end-to-end proof of the host architecture's first extraction
// (docs/features/host-architecture.md, section 9.5, proof 1): a recognised
// test driven through the recorder closes its turn with the checkpoint
// request the extraction's base commit sent for the same input. The request
// is compared with a fixture captured at that base, before any code moved:
// its observations and its evidence in the byte order the idempotency key
// hashes, the key itself, and the whole request. The base is commit
// cac212ce8d710ee2a7aee087b7f2b42fe463e123, the tip of `master` when the
// extraction began; the correctness fixes that landed after the brief was
// written are part of it. The execution record does not exist at the base,
// so its assertion joins this test in the commit that introduces it. Does
// not own what a check is or when it is credited
// (src/tests/engram_turn_checks.rs, whose `CheckedTurn` fixture this child
// module uses), nor the other proofs of section 9.5. New module; nothing was
// split out of another file.
use super::*;

/// The request at the extraction's base, with the values that differ between
/// machines and runs replaced by the placeholders this test writes.
const BASE_CHECKPOINT: &str = include_str!("fixtures/engram_first_extraction_checkpoint.txt");

/// When the check completed, in place of the time the host stamped.
const CHECK_COMPLETED_AT: &str = "<CHECK-COMPLETED-AT>";
/// When the turn's own observation was taken, likewise.
const TURN_OBSERVED_AT: &str = "<TURN-OBSERVED-AT>";

/// `text` with the values no two runs share replaced by placeholders: the
/// worktree root the run's repository happened to be created at, its content
/// revision, and the environment fingerprint, which hashes that root. Each
/// is checked on its own before it is replaced. The stamped times are not
/// replaced here: they are relabelled by the record that carries them, so
/// that two equal times cannot be confused.
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
    // The times, by the record that carries each: the environment is timed
    // with its check, and the turn's own observation no earlier than it.
    let check_completed_at = observations[0]
        .observed_at
        .clone()
        .expect("the check is timed");
    let turn_observed_at = observations[1]
        .observed_at
        .clone()
        .expect("the turn's observation is timed");
    assert_eq!(environment[0].observed_at, check_completed_at);
    assert!(
        check_completed_at <= turn_observed_at,
        "{check_completed_at} should not follow {turn_observed_at}"
    );
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
