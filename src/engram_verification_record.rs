// The TermAl sender of Engram's `verification_record` operation for a
// registered, bound, ordinary committed-input launcher run (`"input":
// "commit"` in its request; src/test_run_named_root.rs).
//
// Owns: which registered runs emit (bound to a named root's claim, request
// input "commit", terminal results, a coherent committed-input record with a
// post-run classification); the record body built once from the run's own
// files (registration, input, run, one check per stage with its check
// fingerprint over the stage's actual argv, post-run classification), frozen
// in a file beside the run and resent byte for byte under the idempotency key
// "termal-run:<run id>"; the route: the initiating session's ordinary
// work-session connection, with its exact store identity and child generation;
// the refusal to send while that connection's work-session answer does not list
// `verification_record_v1` (src/engram_capabilities.rs), kept as a typed
// pending reason beside the run; which Engram refusals are definite (the
// record is rejected) and which are deferred (the same frozen body is sent
// again under the same key); the reading and checking of Engram's receipt,
// held once valid on the run's registration in state
// (`TestRunVerificationAck`, which the state file beside the run only
// mirrors) and never sent for again; handing its
// records to the shared association settlement
// (`settle_test_run_producer_records_locked` in src/test_run_evidence.rs) as
// its second source, and marking the record settled only once they reached
// the run's own launch observation and that write is read back durable; and
// the frozen body checked on every read-back against the record rebuilt from
// the host's registration and the run's files, and a held receipt against
// that body, before either is sent or settled from; and the tick's own budget
// (`ENGRAM_VERIFICATION_RECORD_TICK_BUDGET`) and round-robin cursor
// (`EngramVerificationRecordCursor`), with its in-memory per-run backoff.
//
// Does not own: registration, binding or the named root
// (src/test_run_registration.rs, src/test_run_named_root.rs); the launcher's
// committed-input classification (src/test_run_committed_input.rs,
// scripts/test-launcher.mjs); command recognition and the check fingerprint
// (src/engram_check_recognition.rs via `EngramHost::check_fingerprint`); the
// control transport and its capability reading (src/engram_host_adapter.rs,
// src/engram_capabilities.rs); the tick thread (src/engram_retry_schedule.rs);
// any credit, acceptance or tracker decision, which stay Engram's.

/// The frozen body's file in the run directory: written once, never rebuilt.
const ENGRAM_VERIFICATION_RECORD_BODY_FILE: &str = "engram-verification-record.json";

/// The record's progress beside the body: pending (and why), settled, or
/// rejected. Informational: a received, settled or rejected record is decided
/// by the host's own `TestRunVerificationAck` on the registration, never by
/// this file, which an agent can write.
const ENGRAM_VERIFICATION_RECORD_STATE_FILE: &str = "engram-verification-record-state.json";

/// The most one pass over the registered runs may take, on its own thread
/// (src/engram_retry_schedule.rs), so it never holds up the owed binds; each
/// Engram call and durability wait gets what is left of it.
const ENGRAM_VERIFICATION_RECORD_TICK_BUDGET: Duration = Duration::from_secs(20);

/// How long the settlement of a held receipt waits for its associations to be
/// read back durable when no tick budget bounds it.
const ENGRAM_VERIFICATION_RECORD_SETTLE_FENCE: Duration = Duration::from_secs(5);

/// The first wait before a run left pending, or with its receipt unsettled,
/// is looked at again; each further look that leaves it so doubles the wait,
/// up to `ENGRAM_VERIFICATION_RECORD_BACKOFF_MAX`.
const ENGRAM_VERIFICATION_RECORD_BACKOFF_START: Duration = Duration::from_secs(2);

/// The longest wait between two looks at a run that stays pending.
const ENGRAM_VERIFICATION_RECORD_BACKOFF_MAX: Duration = Duration::from_secs(5 * 60);

/// Engram's refusals of the record itself: the producer's own refusal and a
/// request it cannot decode or validate. Only these are final; any other
/// remote code, known transient or unknown, leaves the record pending and the
/// same frozen body goes again under the same key.
const ENGRAM_VERIFICATION_RECORD_DEFINITE_REFUSALS: &[&str] =
    &["verification_record_refused", "invalid_request"];

/// The idempotency key of one run's record: one key per host run, so every
/// retry, before or after a restart, names the same record.
fn engram_verification_record_key(run_id: &str) -> String {
    format!("termal-run:{run_id}")
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
struct EngramVerificationRecordBody {
    registration: EngramVerificationRecordRegistration,
    input: EngramVerificationRecordInput,
    run: EngramVerificationRecordRun,
    checks: Vec<EngramVerificationRecordCheck>,
    post_run: EngramVerificationRecordPostRun,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct EngramVerificationRecordRegistration {
    host_run_id: String,
    request_identity: String,
    registered_at: String,
    initiating_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owning_session_id: Option<String>,
    binding: EngramControlWorkBinding,
    root: EngramVerificationRecordRoot,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct EngramVerificationRecordRoot {
    workspace_id: String,
    /// Positive and within `i64`, as the producer requires.
    generation: i64,
    state: EngramVerificationRecordRootState,
}

/// A registered run's root was named (bound) when it registered.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EngramVerificationRecordRootState {
    Named,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EngramVerificationRecordInput {
    SourceId {
        source_id: EngramSourceId,
    },
    Commit {
        commit: String,
    },
    /// The staged/freeze lane's input. A type only: no staged-lane run emits
    /// a record yet.
    #[allow(dead_code)]
    StagedTree {
        tree: String,
        base_commit: String,
    },
}

impl EngramVerificationRecordInput {
    // Commit-only consumers keep their existing policy after the full body
    // has been rebuilt and its receipt/digest checked by the caller.
    fn clean_commit(&self) -> Option<&str> {
        match self {
            Self::Commit { commit } => Some(commit),
            Self::SourceId { source_id } if source_id.changes.is_empty()
                && source_id.submodules.iter().all(|child| child.changes.is_empty()) =>
                    Some(&source_id.commit),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for EngramVerificationRecordInput {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if value.get("kind").and_then(Value::as_str) == Some("source_id") {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct SourceInput {
                #[serde(rename = "kind")]
                _kind: String,
                source_id: EngramSourceId,
            }
            let input: SourceInput = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            return Ok(Self::SourceId { source_id: input.source_id });
        }
        // Preserve the historical variants' decoding behavior. The new
        // closed branch cannot fall through to a commit-only interpretation.
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum HistoricalInput {
            Commit { commit: String },
            StagedTree { tree: String, base_commit: String },
        }
        match serde_json::from_value(value).map_err(serde::de::Error::custom)? {
            HistoricalInput::Commit { commit } => Ok(Self::Commit { commit }),
            HistoricalInput::StagedTree { tree, base_commit } => Ok(Self::StagedTree { tree, base_commit }),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EngramVerificationRecordMode {
    Full,
    Focused,
    Live,
}

impl EngramVerificationRecordMode {
    fn word(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Focused => "focused",
            Self::Live => "live",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EngramVerificationRecordRunOutcome {
    Passed,
    Failed,
    /// The launcher recorded the run as interrupted.
    Indeterminate,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct EngramVerificationRecordRun {
    mode: EngramVerificationRecordMode,
    /// The launcher invocation, rendered as TermAl's normalised command line
    /// (`engram_verification_record_command_text`).
    command: String,
    started_at: String,
    finished_at: String,
    outcome: EngramVerificationRecordRunOutcome,
}

/// A check's kind on the wire, lowercase as the producer's
/// `VerificationKind` reads it.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum EngramVerificationRecordCheckKind {
    Test,
    Build,
    Lint,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EngramVerificationRecordCheckOutcome {
    Passed,
    Failed,
    /// The stage never started.
    NotRun,
    /// The stage was running when the run ended: interrupted.
    Indeterminate,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct EngramVerificationRecordCheck {
    stage: String,
    check_kind: EngramVerificationRecordCheckKind,
    /// The stage's actual argv as TermAl's normalised command line: the very
    /// text its check fingerprint hashes.
    command: String,
    check_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
    outcome: EngramVerificationRecordCheckOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    refs: Vec<String>,
    /// The stage's own numeric test counts, as the launcher recorded them:
    /// non-negative integers only, at most `ENGRAM_VERIFICATION_RECORD_MAX_COUNTS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    counts: Option<BTreeMap<String, u64>>,
}

/// The most count keys the producer admits on one check.
const ENGRAM_VERIFICATION_RECORD_MAX_COUNTS: usize = 32;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(untagged)]
enum EngramVerificationRecordPostRun {
    SourceId(EngramVerificationRecordSourcePostRun),
    Commit(EngramVerificationRecordCommitPostRun),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EngramVerificationRecordSourcePostRun {
    source_id: EngramSourceId,
}

impl<'de> Deserialize<'de> for EngramVerificationRecordPostRun {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        // A present new field selects strict decoding, including explicit
        // null and mixed legacy fields. It never falls back to Commit.
        if value.get("source_id").is_some() {
            serde_json::from_value(value).map(Self::SourceId).map_err(serde::de::Error::custom)
        } else {
            serde_json::from_value(value).map(Self::Commit).map_err(serde::de::Error::custom)
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct EngramVerificationRecordCommitPostRun {
    outcome: TestRunCommittedInputOutcome,
    clean_before: bool,
    clean_after: bool,
    /// Paths only; empty, and so left out, for the clean runs the producer
    /// admits today.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dirty_paths: Vec<String>,
    /// No disposition flow exists yet; reserved for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    disposition: Option<String>,
}

/// Engram's answer to a `verification_record`: `operation` and `producer`
/// both name the run's own `registered_host_run` object.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct EngramVerificationRecordReceipt {
    operation: String,
    producer: String,
    verifications: Vec<EngramVerificationRecordVerification>,
    run_outcome: EngramVerificationRecordRunOutcome,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct EngramVerificationRecordVerification {
    stage: String,
    record_id: String,
}

/// Why a frozen record has not been sent, or not settled.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EngramVerificationRecordPending {
    /// The initiating session's ordinary work-session answer does not list
    /// the capability under the current connection and child generation.
    CapabilityUnsupported { support: EngramVerificationRecordSupport },
    /// The initiating session no longer exists. No other session's
    /// connection is ever used in its place.
    InitiatingSessionGone,
    /// The initiating session's project has no enabled Engram connection.
    NoConnection,
    /// Historical progress only; current routing does not emit this reason.
    AwaitingNamingBinding,
    /// A stage is neither a test, build nor admitted lint check; the record
    /// is not built until someone decides how it is recorded.
    UnclassifiedStages { stages: Vec<String> },
    /// The run's checkout was not clean after it. The producer admits only a
    /// clean post-run input today, so nothing is sent.
    PostRunNotClean { outcome: TestRunCommittedInputOutcome },
    /// The registration's root generation is not a positive `i64`.
    RootGenerationOutOfRange { generation: u64 },
    /// A stage's recorded start precedes the registration, or the run ended
    /// before it: the producer refuses such a record, and no timestamp is
    /// ever adjusted to pass.
    StartedBeforeRegistration { stage: Option<String> },
    /// Engram answered with an error that is not a definite refusal of the
    /// record (`code`: transient, such as `deadline_exceeded` or `busy`, or
    /// unknown to this host); the same frozen body is sent again under the
    /// same key.
    RemoteDeferred { code: String },
    /// The project's authority store is no longer the registration's.
    StoreChanged,
    /// The frozen body beside the run cannot be read back exactly; it is
    /// never rebuilt, so the record stays unsent.
    BodyUnreadable,
    /// The frozen body beside the run is not the record the host builds now
    /// from its own registration and the run's request and results: written
    /// before the host froze it, changed since, or the run's files changed.
    /// It is never sent, nor settled from.
    BodyMismatch,
    /// The receipt held beside the run does not answer the frozen body
    /// (`engram_verification_record_receipt_check`): it is not settled from,
    /// and the same body goes again under the same key.
    ReceiptMismatch { detail: String },
    /// The frozen body is not the body the held receipt answered (its digest
    /// differs from the one kept with the receipt), though it is the host's
    /// record of the run's files as they now stand: those files and the body
    /// were rewritten after Engram's answer. Nothing is settled from it.
    AcknowledgedBodyChanged,
    /// The frozen body could not be written beside the run.
    PersistFailed,
    /// The committed-input run's record could not be built from its own
    /// registration, request and results (`detail` says why); nothing is
    /// frozen or sent.
    BodyNotBuilt { detail: String },
    /// Full owner observations differ; the artifact verdict is unchanged.
    SourceChanged { detail: String, changes: Vec<EngramSourceIdDifference> },
    /// The call failed without an answer from Engram; it is sent again.
    TransportFailed { error_kind: String },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
enum EngramVerificationRecordProgress {
    Pending { reason: EngramVerificationRecordPending },
    /// Engram answered with a valid receipt: never sent again. Its records
    /// have not yet reached the run's own launch observation durably (the
    /// row's artifact may attach only after the launcher exits); a later look
    /// settles them from this receipt alone.
    Received { receipt: EngramVerificationRecordReceipt },
    Settled { receipt: EngramVerificationRecordReceipt },
    /// Engram refused the record or answered unreadably: not sent again.
    Rejected { detail: String },
}

/// What the host itself observed of a run's record, kept on its registration
/// (`TestRunRegistration::verification_record`) under the state lock: the
/// authority for whether the record is held, finished, or acknowledged. The
/// run directory's state file, which an agent can write, only mirrors it.
/// A receipt is bound to the body it answered by `body_digest`
/// (`engram_verification_record_body_digest`): a frozen body that no longer
/// has that digest is acknowledged by nothing, however it was rewritten.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
enum TestRunVerificationAck {
    /// Engram's receipt as the host received and checked it; not settled yet.
    Received {
        receipt: EngramVerificationRecordReceipt,
        #[serde(default)]
        body_digest: String,
    },
    /// That receipt's records reached the run's launch observation durably.
    Settled {
        receipt: EngramVerificationRecordReceipt,
        #[serde(default)]
        body_digest: String,
    },
    /// The host saw Engram refuse the record, or answer it unreadably.
    Rejected { detail: String },
}

impl TestRunVerificationAck {
    /// The digest of the body a received or settled receipt answered.
    fn body_digest(&self) -> Option<&str> {
        match self {
            Self::Received { body_digest, .. } | Self::Settled { body_digest, .. } => Some(body_digest),
            Self::Rejected { .. } => None,
        }
    }
}

/// The SHA-256 of `body`'s canonical text (`engram_verification_record_text`,
/// the frozen bytes the host writes and sends); empty when it does not
/// serialize, which no acknowledgment ever matches.
fn engram_verification_record_body_digest(body: &EngramVerificationRecordBody) -> String {
    engram_verification_record_text(body)
        .map(|text| sha256_hex(text.as_bytes()))
        .unwrap_or_default()
}

/// Whether `digest`, kept on an acknowledgment, is the digest of `body`.
fn engram_verification_record_digest_answers(digest: &str, body: &EngramVerificationRecordBody) -> bool {
    !digest.is_empty() && engram_verification_record_body_digest(body) == digest
}

/// One run's frozen record, ready to send.
#[derive(Clone)]
struct EngramVerificationRecordFrozen {
    run_dir: PathBuf,
    run_id: String,
    /// The persisted body; it re-serializes to the exact persisted text.
    record: Value,
    body: EngramVerificationRecordBody,
}

impl std::fmt::Debug for EngramVerificationRecordFrozen {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EngramVerificationRecordFrozen")
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}

/// What one look at a registered run found before any call. The reasons are
/// read by tests and diagnostics only; the tick acts on `Ready` alone.
#[derive(Clone, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
enum EngramVerificationRecordPrepared {
    /// The run is not a bound, registered committed-input run: it emits
    /// nothing.
    NotEligible,
    /// Its results are not terminal yet.
    Waiting,
    /// Settled or rejected before.
    Finished,
    /// Held pending without a call, as its state file now says.
    Pending(EngramVerificationRecordPending),
    Ready(EngramVerificationRecordFrozen),
    /// Answered with a valid receipt that is not settled yet: settled from
    /// the receipt alone, never sent again.
    ReceiptHeld {
        frozen: EngramVerificationRecordFrozen,
        receipt: EngramVerificationRecordReceipt,
    },
}

/// What one send step did; beside the run, its state file says the same. A
/// valid receipt is recorded as received, not settled: it is settled only
/// once its records reached the run's own launch observation and that write
/// is read back durable (`AppState::complete_engram_verification_record`).
/// Until then every later look settles from the held receipt, with no
/// further send.
#[derive(Clone, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
enum EngramVerificationRecordStep {
    Pending(EngramVerificationRecordPending),
    Rejected(String),
    Received {
        body: EngramVerificationRecordBody,
        receipt: EngramVerificationRecordReceipt,
    },
}

/// What one settlement of a held receipt came to. Only `Settled` marks the
/// record settled; the others leave it received, for a later look.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
enum EngramVerificationRecordSettlement {
    Settled,
    /// No launch observation of this run carries its artifact yet (it
    /// attaches once the launcher exits), or the records did not all reach
    /// one.
    NoLaunchRow,
    /// The associations were written but not read back durable (`detail`).
    NotDurable { detail: String },
    /// The receipt does not answer the body (`detail`): nothing is settled.
    ReceiptMismatch { detail: String },
    /// The host already holds a receipt for another body of this run (its
    /// kept digest differs): nothing is settled for this one.
    AcknowledgedBodyChanged,
}

/// What associating a receipt's records found: the run's own launch rows as
/// they now stand, each carrying every record, or none that does.
enum EngramVerificationRecordAssociation {
    Associated(Vec<TestRunCommandObservation>),
    NoLaunchRow,
}

/// The initiating session's ordinary work-session connection. The owning
/// session is audit only.
struct EngramVerificationRecordLink<'a> {
    adapter: &'a EngramHostAdapter,
    connection: &'a EngramConnectionConfig,
    timeout: Duration,
}

/// An owned `EngramVerificationRecordLink`, carried off the state lock: the
/// initiating session's owner-managed ordinary connection.
struct EngramVerificationRecordRoute {
    adapter: Arc<EngramHostAdapter>,
    connection: EngramConnectionConfig,
    timeout: Duration,
}

impl EngramVerificationRecordRoute {
    fn link(&self) -> EngramVerificationRecordLink<'_> {
        EngramVerificationRecordLink {
            adapter: &self.adapter,
            connection: &self.connection,
            timeout: self.timeout,
        }
    }
}

/// Whether a registration is bound the way a committed-input registration is
/// (src/test_run_named_root.rs): a claim, its named root and its store.
fn engram_verification_record_registration_bound(registration: &TestRunRegistration) -> bool {
    registration.work_binding.is_some()
        && registration.named_root.is_some()
        && registration.authority_store.is_some()
}

/// `word` as one shell word in the normalised form the check fingerprint
/// reads (`engram_shell_words` reads it back to `word`): bare when it holds
/// no whitespace or quote, else double-quoted with the Windows argument rules
/// for backslashes before a quote.
fn engram_verification_record_quote(word: &str) -> String {
    if !word.is_empty() && !word.chars().any(|character| character.is_whitespace() || matches!(character, '"' | '\'')) {
        return word.to_owned();
    }
    let mut quoted = String::from('"');
    let mut backslashes = 0usize;
    for character in word.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            other => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(other);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

/// The typed check command of a stage's actual argv, of `kind`: one simple
/// command the launcher ran directly, with no shell between, normalised word
/// by word. `None` for an empty argv.
fn engram_verification_record_check_command(
    argv: &[String],
    kind: EngramVerificationKind,
) -> Option<EngramCheckCommand> {
    let first = argv.first()?;
    Some(EngramCheckCommand {
        kind,
        normalized: engram_verification_record_command_text(argv),
        simple: true,
        program: engram_program_name(first),
        argv: argv.to_vec(),
        literal_pipe: false,
        dialect: EngramShellDialect::Unknown,
        directory: None,
    })
}

/// `argv` as TermAl's normalised command line: each word as the check
/// fingerprint's normalisation spells it (`engram_verification_record_quote`),
/// so the text reads back to the argv and the fingerprint hashes this text.
fn engram_verification_record_command_text(argv: &[String]) -> String {
    argv.iter()
        .map(|word| engram_verification_record_quote(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The stage's counts as the producer admits them: only entries whose value
/// is a non-negative integer, none when no such entry remains or more than
/// `ENGRAM_VERIFICATION_RECORD_MAX_COUNTS` do. A runner label is dropped.
fn engram_verification_record_counts(stage: &Value) -> Option<BTreeMap<String, u64>> {
    let counts: BTreeMap<String, u64> = stage
        .get("tests")?
        .as_object()?
        .iter()
        .filter_map(|(key, value)| Some((key.clone(), value.as_u64()?)))
        .collect();
    (!counts.is_empty() && counts.len() <= ENGRAM_VERIFICATION_RECORD_MAX_COUNTS).then_some(counts)
}

/// Whether `argv` is a true build command: `cargo build` (and its focused
/// launcher form) as `engram_is_build_command` reads it, `cargo check`, or
/// the TypeScript compiler (`tsc`, `npx tsc`, or Node running TypeScript's
/// `bin/tsc`, as the launcher's own typecheck stage does).
fn engram_verification_record_is_build(argv: &[String]) -> bool {
    let Some(first) = argv.first() else {
        return false;
    };
    let program = engram_program_name(first);
    let args = &argv[1..];
    engram_is_build_command(&program, args)
        || (program == "cargo"
            && args.iter().map(String::as_str).find(|word| !word.starts_with('+')) == Some("check"))
        || program == "tsc"
        || (program == "npx" && args.first().is_some_and(|word| word == "tsc"))
        || (program == "node"
            && args
                .first()
                .is_some_and(|script| script.replace('\\', "/").ends_with("typescript/bin/tsc")))
}

/// The kind of a stage's check. Every stage, in every mode, is first read by
/// its actual argv as the focused launcher command a session would type
/// (`engram_is_test_command` on `node scripts/test-launcher.mjs focused --
/// ARGV`, the classifier checkpoint verification applies, which takes `cargo
/// test`, `npx vitest`, Node's `--test` runner and the launcher itself): a
/// test there is a Test. Else a true build command (`cargo check`, `cargo
/// build`, `tsc`) is a Build. The full gate's reserved `fmt` stage admits
/// only the exact check-only Cargo formatter argv as Lint. The exact focused
/// doc-link checker is Lint too. Else the launcher's
/// agreed test-stage rule over the run's own request (`engram_launcher_test_stages`; a live run's
/// `engram-live` stage, as `engram_check_showed_passing_tests` counts it), or
/// the sender's own launcher stage table
/// (`ENGRAM_VERIFICATION_RECORD_LAUNCHER_TEST_STAGES`), makes it a Test. A stage none of these takes has no kind: the record is
/// not built, and the run stays pending naming it
/// (`EngramVerificationRecordPending::UnclassifiedStages`), never silently
/// without it.
fn engram_verification_record_check_kind(
    mode: EngramVerificationRecordMode,
    stage: &str,
    argv: &[String],
    test_stages: &[String],
) -> Option<EngramVerificationRecordCheckKind> {
    if engram_verification_record_is_lint(mode, stage, argv) {
        return Some(EngramVerificationRecordCheckKind::Lint);
    }
    if mode == EngramVerificationRecordMode::Full && stage == "fmt" {
        return None;
    }
    // Category stages use Vitest's same CLI but retain their existing
    // no-credit policy. Only the reserved focused stage adds this route;
    // a client-supplied test-stage list cannot turn a category into a Test.
    if mode == EngramVerificationRecordMode::Focused
        && stage != "focused"
        && matches!(argv, [program, args @ ..]
            if engram_program_name(program) == "node" && engram_is_direct_vitest_run(args))
    {
        return None;
    }
    let focused: Vec<String> = ["scripts/test-launcher.mjs", "focused", "--"]
        .into_iter()
        .map(str::to_owned)
        .chain(argv.iter().cloned())
        .collect();
    if !argv.is_empty() && engram_is_test_command("node", &focused) {
        return Some(EngramVerificationRecordCheckKind::Test);
    }
    if engram_verification_record_is_build(argv) {
        return Some(EngramVerificationRecordCheckKind::Build);
    }
    // A focused command is not a native test because a client labels it one.
    // Unsupported suites need the host's captured declaration and artifact.
    if mode == EngramVerificationRecordMode::Focused && stage == "focused" {
        return None;
    }
    let stage_rule = test_stages.iter().any(|name| name == stage)
        || (mode == EngramVerificationRecordMode::Live && stage == "engram-live")
        || ENGRAM_VERIFICATION_RECORD_LAUNCHER_TEST_STAGES.contains(&stage);
    stage_rule.then_some(EngramVerificationRecordCheckKind::Test)
}

/// Explicit registered lint admission is deliberately narrower than ordinary
/// test/build recognition: no formatter write, extra arguments or wrapper.
fn engram_verification_record_is_lint(
    mode: EngramVerificationRecordMode,
    stage: &str,
    argv: &[String],
) -> bool {
    (mode == EngramVerificationRecordMode::Full
        && stage == "fmt"
        && matches!(argv, [program, subcommand, flag]
        if engram_program_name(program) == "cargo"
            && subcommand == "fmt" && flag == "--check"))
        || (mode == EngramVerificationRecordMode::Focused
            && stage == "focused"
            && matches!(argv, [program, script]
                if engram_program_name(program) == "node"
                    && ![".cmd", ".bat", ".ps1"].iter()
                        .any(|suffix| program.to_ascii_lowercase().ends_with(suffix))
                    && script == "scripts/check-doc-links.mjs"))
}

/// Launcher stage ids this sender alone records as tests when neither their
/// argv nor the shared stage rule says so: the full gate's Node helper tests
/// run `node --test` behind duration-reporter flags, a form the shared focused
/// classifier does not read. Local to the sender; the shared stage rule
/// (`engram_launcher_test_stages`) and the focused classifier stay as they are.
const ENGRAM_VERIFICATION_RECORD_LAUNCHER_TEST_STAGES: &[&str] = &["fingerprint-tests"];

/// A results stage's name and the argv it ran: the actual argv results.json
/// records, or for a stage that never started, the one its request planned.
fn engram_verification_record_stage_argv(
    stage: &Value,
    planned: &HashMap<String, Vec<String>>,
) -> Result<(String, Vec<String>), String> {
    let name = test_run_str(stage, "name")
        .filter(|name| test_run_valid_stage_name(name))
        .ok_or_else(|| "a stage has no valid name".to_owned())?;
    let argv = test_run_argv(stage.get("command"))
        .filter(|argv| !argv.is_empty())
        .or_else(|| planned.get(&name).cloned())
        .ok_or_else(|| format!("stage {name} names no command"))?;
    Ok((name, argv))
}

/// The stages of a run that are neither a test, build nor admitted lint check
/// (`engram_verification_record_check_kind`), in results order.
fn engram_verification_record_unclassified_stages(request: &Value, results: &Value) -> Vec<String> {
    let mode = engram_verification_record_mode(request);
    let planned = engram_verification_record_planned_argv(request);
    let test_stages = engram_launcher_test_stages(request);
    results
        .get("stages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|stage| engram_verification_record_stage_argv(stage, &planned).ok())
        .filter(|(name, argv)| {
            engram_verification_record_check_kind(mode, name, argv, &test_stages).is_none()
        })
        .map(|(name, _)| name)
        .collect()
}

fn engram_verification_record_registration_unclassified_stages(
    registration: &TestRunRegistration,
    request: &Value,
    results: &Value,
) -> Vec<String> {
    let mut stages = engram_verification_record_unclassified_stages(request, results);
    if registration.declared_native.is_some() && registration.declared_terminal.is_some() {
        // The body builder separately checks the actual command against this
        // captured binding. Merely having a focused stage is insufficient.
        stages.retain(|stage| stage != "focused");
    }
    stages
}

/// The argv each stage was planned with in the request (`command` and
/// `args`), by stage name: what an unrun stage would have run.
fn engram_verification_record_planned_argv(request: &Value) -> HashMap<String, Vec<String>> {
    request
        .get("stages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|stage| {
            let name = test_run_str(stage, "name")?;
            let mut argv = vec![test_run_str(stage, "command")?];
            argv.extend(test_run_argv(stage.get("args")).unwrap_or_default());
            Some((name, argv))
        })
        .collect()
}

fn engram_verification_record_mode(request: &Value) -> EngramVerificationRecordMode {
    if request.get("full").and_then(Value::as_bool) == Some(true) {
        EngramVerificationRecordMode::Full
    } else if request.get("liveEngram").is_some_and(|live| !live.is_null()) {
        EngramVerificationRecordMode::Live
    } else {
        EngramVerificationRecordMode::Focused
    }
}

/// The launcher's own recorded command (`command`, an argv, in results.json
/// or else request.json), when either records one. Today's launcher records
/// none, so `engram_verification_record_run_command` reconstructs it.
fn engram_verification_record_recorded_command(request: &Value, results: &Value) -> Option<Vec<String>> {
    if let Some(command) = request.get("nativeExecution").and_then(Value::as_str)
        .and_then(|_| test_run_argv(request.get("observedCommand"))).filter(|argv| !argv.is_empty()) {
        return Some(command);
    }
    [results, request]
        .into_iter()
        .find_map(|record| test_run_argv(record.get("command")).filter(|argv| !argv.is_empty()))
}

/// The launcher invocation reconstructed from a run's mode and input; a
/// focused run adds its planned command after `--`. Used only when the
/// launcher recorded no command of its own.
fn engram_verification_record_run_command(
    mode: EngramVerificationRecordMode,
    request: &Value,
) -> Vec<String> {
    let mut command: Vec<String> = ["node", "scripts/test-launcher.mjs", mode.word(), "--input", "commit"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    if mode == EngramVerificationRecordMode::Focused
        && let Some(argv) = request
            .get("stages")
            .and_then(Value::as_array)
            .and_then(|stages| stages.first())
            .and_then(|stage| {
                let mut argv = vec![test_run_str(stage, "command")?];
                argv.extend(test_run_argv(stage.get("args")).unwrap_or_default());
                Some(argv)
            })
    {
        command.push("--".to_owned());
        command.extend(argv);
    }
    command
}

/// One stage of terminal results as a check; a stage that is neither a test,
/// build nor admitted lint check (`engram_verification_record_check_kind`)
/// fails the build of the whole record rather than leaving it out.
fn engram_verification_record_check(
    stage: &Value,
    planned: &HashMap<String, Vec<String>>,
    mode: EngramVerificationRecordMode,
    test_stages: &[String],
) -> Result<EngramVerificationRecordCheck, String> {
    let (name, argv) = engram_verification_record_stage_argv(stage, planned)?;
    let outcome = match test_run_stage_state(stage.get("state").and_then(Value::as_str)) {
        TestRunStageState::Passed => EngramVerificationRecordCheckOutcome::Passed,
        TestRunStageState::Failed => EngramVerificationRecordCheckOutcome::Failed,
        TestRunStageState::Unrun => EngramVerificationRecordCheckOutcome::NotRun,
        TestRunStageState::Running => EngramVerificationRecordCheckOutcome::Indeterminate,
    };
    let Some(kind) = engram_verification_record_check_kind(mode, &name, &argv, test_stages) else {
        return Err(format!("stage {name} is neither a test, build nor admitted lint check"));
    };
    let command = engram_verification_record_command_text(&argv);
    let exit = stage.get("code").and_then(Value::as_i64);
    let outcome_word = match outcome {
        EngramVerificationRecordCheckOutcome::Passed => "passed",
        EngramVerificationRecordCheckOutcome::Failed => "failed",
        EngramVerificationRecordCheckOutcome::NotRun => "not run",
        EngramVerificationRecordCheckOutcome::Indeterminate => "interrupted",
    };
    let summary = match exit {
        Some(code) => format!("{name}: {outcome_word} (exit {code})"),
        None => format!("{name}: {outcome_word}"),
    };
    let mut refs = Vec::new();
    let command_ref = format!("command:{command}");
    if command_ref.len() <= ENGRAM_CHECK_REF_MAX_BYTES {
        refs.push(command_ref);
    }
    if let Some(code) = exit {
        refs.push(format!("exit:{code}"));
    }
    Ok(EngramVerificationRecordCheck {
        stage: name,
        check_kind: kind,
        check_fingerprint: EngramHost::normalized_check_fingerprint(&command),
        command,
        started_at: test_run_str(stage, "started"),
        completed_at: test_run_str(stage, "ended"),
        outcome,
        summary: Some(summary),
        refs,
        counts: engram_verification_record_counts(stage),
    })
}

fn engram_verification_record_declared_check(
    stage: &Value,
    planned: &HashMap<String, Vec<String>>,
    binding: &EngramDeclaredBinding,
    exit: EngramCommandExit,
) -> Result<EngramVerificationRecordCheck, String> {
    let (name, argv) = engram_verification_record_stage_argv(stage, planned)?;
    if engram_declared_words(&binding.entry.command).ok().as_ref() != Some(&argv) {
        return Err("declared stage differs from the host's captured command".to_owned());
    }
    let command = engram_verification_record_check_command(&argv, EngramVerificationKind::Test)
        .ok_or("declared stage has no native command")?;
    if command.normalized.len() > 8192 {
        return Err("declared native command exceeds the verification contract".to_owned());
    }
    let result = engram_declared_artifact_result(binding);
    let outcome = match &result.verdict {
            EngramTrxVerdict::Pass => EngramVerificationRecordCheckOutcome::Passed,
            EngramTrxVerdict::Fail(_) => EngramVerificationRecordCheckOutcome::Failed,
            EngramTrxVerdict::Unknown(_) => EngramVerificationRecordCheckOutcome::Indeterminate,
    };
    let refs = engram_declared_refs(&command, Some(binding), exit, &result);
    let safe = |text: &str| text == text.trim() && !text.chars().any(|c| {
        c.is_control() || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    });
    let summary = engram_declared_summary(&command, Some(binding), exit, &result);
    if !safe(&command.normalized)
        || summary.len() > ENGRAM_CHECK_SUMMARY_MAX_BYTES || !safe(&summary)
        || refs.len() > 64
        || refs.iter().any(|value| value.len() > ENGRAM_CHECK_REF_MAX_BYTES || !safe(value))
    {
        return Err("declared provenance is outside the verification text contract".to_owned());
    }
    let counts = result.counters.as_ref().map(|counters| {
        counters.values.iter().cloned().collect::<BTreeMap<String, u64>>()
    });
    if counts.as_ref().is_some_and(|counts| counts.len() > 32 || counts.keys().any(|key| key.len() > 128 || !safe(key))) {
        return Err("declared counters exceed the verification contract".to_owned());
    }
    Ok(EngramVerificationRecordCheck {
        stage: name, check_kind: EngramVerificationRecordCheckKind::Test,
        command: command.normalized.clone(),
        check_fingerprint: EngramHost::check_fingerprint(&command),
        started_at: test_run_str(stage, "started"), completed_at: test_run_str(stage, "ended"),
        outcome, summary: Some(summary),
        refs, counts,
    })
}

// Producer-only terminal admission. Generic launcher disk parsing is unchanged.
fn engram_verification_record_results_are_terminal(registration: &TestRunRegistration, results: &Value) -> bool {
    let Some(start) = &registration.declared_native else { return test_run_results_are_terminal(results); };
    let Some(terminal) = &registration.declared_terminal else { return false; };
    // A native artifact verdict may be unknown or failed despite exit 0 or
    // Claude ReportedSuccess. Only this host-owned terminal and exact digest
    // admit it; generic launchers keep their existing numeric terminal rule.
    let state = declared_run_result_state(&terminal.check);
    let exit = results.get("exitCode");
    if registration.registered_at != start.registered_at
        || registration.request_identity != start.request_identity
        || registration.work_binding.as_ref() != Some(&start.binding)
        || registration.named_root.as_ref() != Some(&start.named)
        || registration.authority_store.as_ref() != Some(&start.store)
        || results.get("runId").and_then(Value::as_str) != Some(start.run_id.as_str())
        || results.get("state").and_then(Value::as_str) != Some(state)
        || !results.get("ended").and_then(Value::as_str).is_some_and(|ended| EngramHost::check_timestamp(ended).is_some())
    { return false; }
    let Some(stages) = results.get("stages").and_then(Value::as_array) else { return false; };
    let [stage] = stages.as_slice() else { return false; };
    stage.get("name").and_then(Value::as_str) == Some("focused")
        && stage.get("state").and_then(Value::as_str) == Some(state)
        && stage.get("reportedSuccess").and_then(Value::as_bool) == Some(terminal.reported_success)
        && stage.get("code") == exit
        && test_run_str(stage, "ended") == terminal.check.completed_at
        && serde_json::to_vec_pretty(results).ok().is_some_and(|bytes|
            file_content_hash(&bytes) == terminal.results_digest)
}

/// The record of a bound committed-input run, from its registration and its
/// own request and terminal results; why not, when the run cannot emit one.
fn engram_verification_record_body(
    registration: &TestRunRegistration,
    request: &Value,
    results: &Value,
) -> Result<EngramVerificationRecordBody, String> {
    engram_verification_record_build_body(registration, request, results, None)
}

// A retained body is checked in its original format, never converted during
// replay. Only that historical reader can reconstruct a commit projection
// from source-bearing artifacts; a fresh record keeps the complete identity.
fn engram_verification_record_build_body(
    registration: &TestRunRegistration,
    request: &Value,
    results: &Value,
    retained_input: Option<&EngramVerificationRecordInput>,
) -> Result<EngramVerificationRecordBody, String> {
    if let Some(start) = registration.declared_native.as_ref() {
        return engram_verification_record_native_body(registration, request, results, start);
    }
    if !test_run_request_is_committed_input(request) {
        return Err("the run is not a committed-input run".to_owned());
    }
    if !engram_verification_record_results_are_terminal(registration, results) {
        return Err("the run has no terminal result".to_owned());
    }
    let input = TestRunCommittedInput::read(results)
        .ok_or_else(|| "the run records no committed input".to_owned())?;
    let commit = input
        .commit
        .clone()
        .filter(|_| input.clean_before)
        .ok_or_else(|| "the run was refused before any stage ran".to_owned())?;
    let (Some(outcome), Some(after)) = (input.outcome, input.after.as_ref()) else {
        return Err("the run records no post-run classification".to_owned());
    };
    let mode = engram_verification_record_mode(request);
    let planned = engram_verification_record_planned_argv(request);
    let test_stages = engram_launcher_test_stages(request);
    let mut checks = Vec::new();
    for stage in results
        .get("stages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let check = engram_verification_record_check(stage, &planned, mode, &test_stages)?;
        if checks
            .iter()
            .any(|earlier: &EngramVerificationRecordCheck| earlier.stage == check.stage)
        {
            return Err(format!("stage {} is recorded twice", check.stage));
        }
        checks.push(check);
    }
    if checks.is_empty() {
        return Err("the run records no stage".to_owned());
    }
    let historical = retained_input.is_some_and(|input| matches!(input,
        EngramVerificationRecordInput::Commit { .. }));
    let has_source = request.get("sourceId").is_some()
        || results.get("sourceIdBefore").is_some() || results.get("sourceIdAfter").is_some();
    let endpoints = if has_source {
        let admitted = test_run_source_id(request.get("sourceId"))
            .ok_or("the admitted owner source_id is unavailable")?;
        let (before, after) = test_run_source_endpoints(results)
            .ok_or("the owner source_id endpoints are unavailable")?;
        if admitted != before || before != after || before.commit != commit {
            return Err("the owner source_id differs across the run boundaries".to_owned());
        }
        if !before.changes.is_empty() || before.submodules.iter().any(|child| !child.changes.is_empty()) {
            return Err("the owner source_id contradicts the clean committed input".to_owned());
        }
        Some((before, after))
    } else { None };
    let (record_input, post_run) = if !historical {
        // SourceId carries no legacy classification fields. Retain the
        // ordinary committed-input policy even when rebuilding a held receipt.
        if outcome != TestRunCommittedInputOutcome::Clean || !after.clean || !after.paths.is_empty() {
            return Err("the run's post-run committed input is not clean".to_owned());
        }
        let (before, after) = endpoints.ok_or("the run records no complete owner source_id")?;
        (EngramVerificationRecordInput::SourceId { source_id: before },
            EngramVerificationRecordPostRun::SourceId(EngramVerificationRecordSourcePostRun { source_id: after }))
    } else {
        // A retained commit body keeps its historical shape, without
        // inventing equivalence with a new SourceId or rewriting its bytes.
        (EngramVerificationRecordInput::Commit { commit },
            EngramVerificationRecordPostRun::Commit(EngramVerificationRecordCommitPostRun {
            outcome, clean_before: input.clean_before, clean_after: after.clean,
            dirty_paths: after.paths.iter().map(|path| path.path.clone()).collect(), disposition: None,
        }))
    };
    let body = engram_verification_record_envelope(registration, request, results, record_input, post_run, checks)?;
    engram_verification_record_frame_bound(&body, &registration.run_id, "verification_frame_too_large")?;
    Ok(body)
}

fn engram_verification_record_native_body(
    registration: &TestRunRegistration, request: &Value, results: &Value,
    start: &DeclaredRunReservation,
) -> Result<EngramVerificationRecordBody, String> {
    let terminal = registration.declared_terminal.as_ref()
        .ok_or("the host has not confirmed the native terminal")?;
    let after = terminal.source.as_ref().ok_or_else(|| format!("native source_id after the run is unavailable: {}",
        terminal.source_refusal.as_deref().unwrap_or("owner measurement unavailable")))?;
    let digest = serde_json::to_vec_pretty(results).map_err(|_| "native results cannot be encoded")?;
    let input = EngramVerificationRecordInput::SourceId { source_id: start.source.source_id.clone() };
    let post_run = EngramVerificationRecordPostRun::SourceId(EngramVerificationRecordSourcePostRun {
        source_id: after.source_id.clone(),
    });
    if terminal.results_digest != file_content_hash(&digest)
        || registration.registered_at != start.registered_at
        || registration.work_binding.as_ref() != Some(&start.binding)
        || registration.named_root.as_ref() != Some(&start.named)
        || registration.authority_store.as_ref() != Some(&start.store)
        || registration.source_owner.as_ref() != Some(&start.source.source_owner)
        || registration.request_identity != start.request_identity
        || test_run_registration_identity(request).ok().as_ref() != Some(&start.request_identity)
        || request.get("input").and_then(Value::as_str) != Some("source_id")
        || request.get("nativeSourceId") != Some(&json!(start.source.source_id))
        || after.root != start.source.root || after.source_owner != start.source.source_owner
        || results.get("input") != Some(&serde_json::to_value(&input).map_err(|error| error.to_string())?)
        || results.get("post_run") != Some(&serde_json::to_value(&post_run).map_err(|error| error.to_string())?)
    {
        return Err("native terminal does not match its host reservation".to_owned());
    }
    if after.source_id != start.source.source_id {
        return Err(engram_source_id_change_detail(&engram_source_id_differences(&start.source.source_id, &after.source_id)));
    }
    if results.get("nativeEligible").and_then(Value::as_bool) != Some(true) {
        return Err("native run is ineligible under its host authority checks".to_owned());
    }
    if !engram_verification_record_results_are_terminal(registration, results) {
        return Err("the native run has no terminal result".to_owned());
    }
    if results.get("stages").and_then(Value::as_array).is_none_or(|stages|
        stages.len() != 1 || stages[0].get("name").and_then(Value::as_str) != Some("focused")) {
        return Err("the native run does not have its single original stage".to_owned());
    }
    let body = engram_verification_record_envelope(registration, request, results, input, post_run,
        vec![terminal.check.clone()])?;
    // Reuse the control transport's operation-specific frame bound. Keep the full
    // identity and a named refusal instead of sending a shortened record or
    // letting a permanent oversize frame become a transport retry.
    engram_verification_record_frame_bound(&body, &registration.run_id, "native_verification_frame_too_large")?;
    Ok(body)
}

fn engram_verification_record_frame_bound(body: &EngramVerificationRecordBody, run_id: &str, code: &str)
    -> Result<(), String>
{
    let frame = EngramControlRequest::VerificationRecord {
        operation_key: engram_verification_record_key(run_id),
        record: serde_json::to_value(body).map_err(|_| "the record cannot be encoded")?,
    };
    let bytes = serde_json::to_vec(&frame).map_err(|_| "the record frame cannot be encoded")?;
    let framed_len = bytes.len().saturating_add(1); // The transport writes one LF.
    let limit = engram_control_request_frame_limit(&frame);
    if framed_len > limit {
        return Err(format!("{code}: {framed_len} bytes exceeds the {limit} byte verification frame"));
    }
    Ok(())
}

fn engram_verification_record_envelope(
    registration: &TestRunRegistration, request: &Value, results: &Value,
    input: EngramVerificationRecordInput, post_run: EngramVerificationRecordPostRun,
    checks: Vec<EngramVerificationRecordCheck>,
) -> Result<EngramVerificationRecordBody, String> {
    let binding = registration.work_binding.clone().ok_or("the run is not bound to a claim")?;
    let root = registration.named_root.as_ref().ok_or("the run names no root")?;
    let mode = engram_verification_record_mode(request);
    let run_outcome = if results.get("interrupted").and_then(Value::as_bool) == Some(true)
        || checks.iter().any(|check| check.outcome == EngramVerificationRecordCheckOutcome::Indeterminate)
    {
        EngramVerificationRecordRunOutcome::Indeterminate
    } else if results.get("state").and_then(Value::as_str) == Some("passed")
        && checks.iter().all(|check| check.outcome == EngramVerificationRecordCheckOutcome::Passed)
    {
        EngramVerificationRecordRunOutcome::Passed
    } else {
        EngramVerificationRecordRunOutcome::Failed
    };
    Ok(EngramVerificationRecordBody {
        registration: EngramVerificationRecordRegistration {
            host_run_id: registration.run_id.clone(),
            request_identity: registration.request_identity.clone(),
            registered_at: registration.registered_at.clone(),
            initiating_session_id: registration.initiating_session_id.clone(),
            owning_session_id: Some(registration.owning_session_id.clone()),
            binding,
            root: EngramVerificationRecordRoot {
                workspace_id: root.workspace_id.clone(),
                generation: i64::try_from(root.generation)
                    .ok()
                    .filter(|generation| *generation > 0)
                    .ok_or_else(|| "the run's root generation is not a positive i64".to_owned())?,
                state: EngramVerificationRecordRootState::Named,
            },
        },
        input,
        run: EngramVerificationRecordRun {
            mode,
            command: engram_verification_record_command_text(
                &engram_verification_record_recorded_command(request, results)
                    .unwrap_or_else(|| engram_verification_record_run_command(mode, request)),
            ),
            started_at: test_run_str(request, "started")
                .or_else(|| test_run_str(results, "started"))
                .ok_or_else(|| "the run records no start".to_owned())?,
            finished_at: test_run_str(results, "ended")
                .ok_or_else(|| "the run records no end".to_owned())?,
            outcome: run_outcome,
        },
        checks,
        post_run,
    })
}

/// The persisted text of `body`: its JSON value, serialized once. Reading
/// that text back and serializing the value again gives the same bytes,
/// whatever map order serde_json was built with.
fn engram_verification_record_text(body: &EngramVerificationRecordBody) -> Result<String, String> {
    serde_json::to_value(body)
        .and_then(|value| serde_json::to_string(&value))
        .map_err(|error| format!("the record does not serialize: {error}"))
}

/// The frozen record in `text`, only when it reads back to exactly `text`.
fn engram_verification_record_parse(text: &str) -> Option<(Value, EngramVerificationRecordBody)> {
    let record: Value = serde_json::from_str(text).ok()?;
    if !record.is_object() || serde_json::to_string(&record).ok()?.as_str() != text {
        return None;
    }
    let body: EngramVerificationRecordBody = serde_json::from_value(record.clone()).ok()?;
    // A typed comparison alone would ignore unknown fields while the raw
    // value is flattened into the outgoing request. Preserve the file, but
    // refuse to send or settle any body that carries such extra fields.
    if serde_json::to_value(&body).ok()? != record {
        return None;
    }
    Some((record, body))
}

/// The tokenless ordinary request sends the same frozen key and body bytes
/// on every attempt.
fn engram_verification_record_request(
    frozen: &EngramVerificationRecordFrozen,
) -> EngramControlRequest {
    EngramControlRequest::VerificationRecord {
        operation_key: engram_verification_record_key(&frozen.run_id),
        record: frozen.record.clone(),
    }
}

/// The stages the producer mints a verification for: those with a completion
/// time whose outcome is not `not_run`.
fn engram_verification_record_emitted_stages(body: &EngramVerificationRecordBody) -> Vec<&str> {
    body.checks
        .iter()
        .filter(|check| {
            check.completed_at.is_some() && check.outcome != EngramVerificationRecordCheckOutcome::NotRun
        })
        .map(|check| check.stage.as_str())
        .collect()
}

/// Whether `receipt` answers exactly `body`: its operation is the producer
/// object it names, a well-formed record id; one well-formed, distinct record
/// for each emitted stage (`engram_verification_record_emitted_stages`) and
/// no other; and the run's own outcome.
fn engram_verification_record_receipt_check(
    body: &EngramVerificationRecordBody,
    receipt: &EngramVerificationRecordReceipt,
) -> Result<(), &'static str> {
    if !is_engram_record_id(&receipt.producer) || receipt.operation != receipt.producer {
        return Err("the receipt does not name its producer as its operation");
    }
    if receipt.run_outcome != body.run.outcome {
        return Err("the receipt states another run outcome");
    }
    let emitted: HashSet<&str> = engram_verification_record_emitted_stages(body).into_iter().collect();
    if receipt.verifications.len() != emitted.len() {
        return Err("the receipt does not name one record per emitted check");
    }
    let mut stages = HashSet::new();
    let mut records = HashSet::new();
    for verification in &receipt.verifications {
        if !emitted.contains(verification.stage.as_str()) || !stages.insert(verification.stage.as_str()) {
            return Err("the receipt names a stage the producer does not record, or one twice");
        }
        if !is_engram_record_id(&verification.record_id)
            || !records.insert(verification.record_id.as_str())
        {
            return Err("the receipt names a malformed or repeated record id");
        }
    }
    Ok(())
}

/// Why the producer would refuse this run's record before it is built, as a
/// pending reason: a post-run checkout that is not clean, a root generation
/// outside a positive `i64`, or a stage start (or the run's end) before the
/// registration. Times are compared as recorded, never adjusted.
fn engram_verification_record_producer_refusal(
    registration: &TestRunRegistration,
    results: &Value,
) -> Option<EngramVerificationRecordPending> {
    if let Some(outcome) = TestRunCommittedInput::read(results)
        .and_then(|input| input.outcome)
        .filter(|outcome| *outcome != TestRunCommittedInputOutcome::Clean)
    {
        return Some(EngramVerificationRecordPending::PostRunNotClean { outcome });
    }
    if let Some(root) = registration.named_root.as_ref()
        && i64::try_from(root.generation).map_or(true, |generation| generation <= 0)
    {
        return Some(EngramVerificationRecordPending::RootGenerationOutOfRange {
            generation: root.generation,
        });
    }
    let time = |text: &str| EngramHost::check_timestamp(text);
    let registered = time(&registration.registered_at)?;
    if test_run_str(results, "ended")
        .and_then(|ended| time(&ended))
        .is_some_and(|ended| ended < registered)
    {
        return Some(EngramVerificationRecordPending::StartedBeforeRegistration { stage: None });
    }
    results
        .get("stages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|stage| {
            test_run_str(stage, "started")
                .and_then(|started| time(&started))
                .is_some_and(|started| started < registered)
        })
        .map(|stage| EngramVerificationRecordPending::StartedBeforeRegistration {
            stage: test_run_str(stage, "name"),
        })
}

/// Writes `bytes` at `path` through a synced temporary file and a rename.
fn engram_verification_record_write(path: &FsPath, bytes: &[u8]) -> io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    let mut file = fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)
}

fn engram_verification_record_progress(run_dir: &FsPath) -> Option<EngramVerificationRecordProgress> {
    let text = fs::read_to_string(run_dir.join(ENGRAM_VERIFICATION_RECORD_STATE_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Records `progress` beside the run when it differs from what is there, so
/// an unchanged pending reason is neither rewritten nor logged again.
fn engram_verification_record_set_progress(
    run_dir: &FsPath,
    run_id: &str,
    progress: EngramVerificationRecordProgress,
) {
    if engram_verification_record_progress(run_dir).as_ref() == Some(&progress) {
        return;
    }
    let written = serde_json::to_vec(&progress)
        .map_err(io::Error::other)
        .and_then(|bytes| {
            engram_verification_record_write(&run_dir.join(ENGRAM_VERIFICATION_RECORD_STATE_FILE), &bytes)
        });
    if let Err(error) = written {
        eprintln!("engram> run={run_id} verification record state not written: {error}");
    }
}

/// The frozen body beside `run_dir`, read back exactly; `None` when it is
/// missing or does not read back to its own bytes.
fn engram_verification_record_frozen(run_dir: &FsPath, run_id: &str) -> Option<EngramVerificationRecordFrozen> {
    let text = fs::read_to_string(run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE)).ok()?;
    let (record, body) = engram_verification_record_parse(&text)?;
    Some(EngramVerificationRecordFrozen {
        run_dir: run_dir.to_path_buf(),
        run_id: run_id.to_owned(),
        record,
        body,
    })
}

/// Whether `body` is exactly the record the host builds again from its own
/// `registration` and the run's request and results beside it: the check a
/// frozen body read back from the run directory must pass before anything is
/// sent, settled or admitted from it.
fn engram_verification_record_is_host_body(
    registration: &TestRunRegistration,
    body: &EngramVerificationRecordBody,
) -> bool {
    let run_dir = PathBuf::from(&registration.run_directory);
    let (TestRunJson::Parsed(request, _, _), TestRunJson::Parsed(results, _, _)) = (
        test_run_read_json(&run_dir.join("request.json")),
        test_run_read_json(&run_dir.join("results.json")),
    ) else {
        return false;
    };
    engram_verification_record_build_body(registration, &request, &results, Some(&body.input))
        .is_ok_and(|rebuilt| rebuilt == *body)
}

/// Looks at one registered run off the state lock: whether it emits, and
/// when it does, its frozen record, built and persisted on the first look.
/// The run directory is the launcher's and so an agent's to write: on every
/// later look the frozen body is read back and must equal, exactly, the
/// record the host builds again from its own registration and the run's
/// request and results; any other body (one written before the host froze
/// it, changed since, or over changed run files) is `BodyMismatch`, never
/// sent nor settled from. A held receipt is settled from only once it
/// answers that validated body (`engram_verification_record_receipt_check`).
/// The record's own progress is read first, from what the host observed and
/// kept on `registration` (`TestRunVerificationAck`), never from the state
/// file beside the run: a finished run is never read or hashed again, and a
/// file that claims a receipt, a settlement or a rejection the host never
/// observed neither holds nor stops the send.
fn engram_verification_record_prepare(registration: &TestRunRegistration) -> EngramVerificationRecordPrepared {
    use EngramVerificationRecordPrepared as Prepared;
    if !engram_verification_record_registration_bound(registration) {
        return Prepared::NotEligible;
    }
    let run_dir = PathBuf::from(&registration.run_directory);
    let pending = |reason: EngramVerificationRecordPending| {
        engram_verification_record_set_progress(
            &run_dir,
            &registration.run_id,
            EngramVerificationRecordProgress::Pending { reason: reason.clone() },
        );
        Prepared::Pending(reason)
    };
    let held = match &registration.verification_record {
        Some(TestRunVerificationAck::Settled { .. } | TestRunVerificationAck::Rejected { .. }) => {
            return Prepared::Finished;
        }
        Some(TestRunVerificationAck::Received { receipt, body_digest }) => {
            Some((receipt.clone(), body_digest.clone()))
        }
        None => None,
    };
    // The run's own request and terminal results, as the record is built
    // from; `Err` is what the look comes to without them.
    let inputs = || -> std::result::Result<(Value, Value), Prepared> {
        let TestRunJson::Parsed(request, _, _) = test_run_read_json(&run_dir.join("request.json")) else {
            return Err(Prepared::NotEligible);
        };
        if !test_run_request_is_committed_input(&request) && registration.declared_native.is_none() {
            return Err(Prepared::NotEligible);
        }
        if test_run_registration_identity(&request).ok().as_ref() != Some(&registration.request_identity) {
            return Err(Prepared::NotEligible);
        }
        let results = match test_run_read_json(&run_dir.join("results.json")) {
            TestRunJson::Parsed(results, _, _) if engram_verification_record_results_are_terminal(registration, &results) => results,
            TestRunJson::TooLarge if registration.declared_native.is_some() => return Err(pending(
                EngramVerificationRecordPending::BodyNotBuilt {
                    detail: format!("native_results_too_large: exceeds the existing {TEST_RUN_JSON_MAX_BYTES} byte run JSON bound"),
                })),
            _ => return Err(Prepared::Waiting),
        };
        // A run whose results hold no committed-input record never emits;
        // only a record that cannot be built from a present one is held
        // pending.
        if registration.declared_native.is_none() && TestRunCommittedInput::read(&results).is_none() {
            return Err(Prepared::NotEligible);
        }
        Ok((request, results))
    };
    // Answered: settled from the receipt, never built or sent again, once
    // the frozen body is the host's own record and the receipt answers it.
    // A body that no longer reads back, or is not the host's, keeps the
    // receipt as it is; a receipt that does not answer it is not settled
    // from, and the same body goes again under the same key. A body that is
    // the host's record of rewritten run files, but not the body the receipt
    // answered, is never settled from that receipt.
    if let Some((receipt, body_digest)) = held {
        let Some(frozen) = engram_verification_record_frozen(&run_dir, &registration.run_id) else {
            return Prepared::Pending(EngramVerificationRecordPending::BodyUnreadable);
        };
        let rebuilt = inputs()
            .ok()
            .and_then(|(request, results)| engram_verification_record_build_body(
                registration, &request, &results, Some(&frozen.body.input)).ok());
        if rebuilt.as_ref() != Some(&frozen.body) {
            return Prepared::Pending(EngramVerificationRecordPending::BodyMismatch);
        }
        if !engram_verification_record_digest_answers(&body_digest, &frozen.body) {
            return pending(EngramVerificationRecordPending::AcknowledgedBodyChanged);
        }
        if let Err(why) = engram_verification_record_receipt_check(&frozen.body, &receipt) {
            return pending(EngramVerificationRecordPending::ReceiptMismatch { detail: why.to_owned() });
        }
        return Prepared::ReceiptHeld { frozen, receipt };
    }
    let (request, results) = match inputs() {
        Ok(inputs) => inputs,
        Err(prepared) => return prepared,
    };
    let stages = engram_verification_record_registration_unclassified_stages(registration, &request, &results);
    let refusal = if stages.is_empty() {
        engram_verification_record_producer_refusal(registration, &results)
    } else {
        Some(EngramVerificationRecordPending::UnclassifiedStages { stages })
    };
    if let Some(reason) = refusal {
        return pending(reason);
    }
    let retained = engram_verification_record_frozen(&run_dir, &registration.run_id);
    let built = match retained.as_ref() {
        Some(frozen) => engram_verification_record_build_body(registration, &request, &results, Some(&frozen.body.input)),
        None => engram_verification_record_body(registration, &request, &results),
    }.and_then(|body| engram_verification_record_text(&body).map(|text| (body, text)));
    let (rebuilt, rebuilt_text) = match built {
        Ok(built) => built,
        Err(detail) => {
            let changes = registration.declared_native.as_ref().zip(registration.declared_terminal.as_ref())
                .and_then(|(start, terminal)| terminal.source.as_ref().map(|after|
                    engram_source_id_differences(&start.source.source_id, &after.source_id)));
            if let Some(changes) = changes.filter(|changes| !changes.is_empty()
                && engram_source_id_change_detail(changes) == detail) {
                return pending(EngramVerificationRecordPending::SourceChanged { detail, changes });
            }
            return pending(EngramVerificationRecordPending::BodyNotBuilt { detail });
        }
    };
    let body_path = run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    let text = match fs::read_to_string(&body_path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if engram_verification_record_write(&body_path, rebuilt_text.as_bytes()).is_err() {
                return pending(EngramVerificationRecordPending::PersistFailed);
            }
            rebuilt_text
        }
        Err(_) => String::new(),
    };
    let Some((record, body)) = engram_verification_record_parse(&text) else {
        return pending(EngramVerificationRecordPending::BodyUnreadable);
    };
    if body != rebuilt {
        return pending(EngramVerificationRecordPending::BodyMismatch);
    }
    Prepared::Ready(EngramVerificationRecordFrozen {
        run_dir: run_dir.clone(),
        run_id: registration.run_id.clone(),
        record,
        body,
    })
}

/// Sends `frozen` on `link` when its connection supports the operation, and
/// records what came of it beside the run. Without the capability, or
/// without a link, nothing is sent and the run stays pending with its reason.
fn engram_verification_record_send(
    frozen: EngramVerificationRecordFrozen,
    link: std::result::Result<EngramVerificationRecordLink<'_>, EngramVerificationRecordPending>,
) -> EngramVerificationRecordStep {
    let pending = |reason: EngramVerificationRecordPending| {
        engram_verification_record_set_progress(
            &frozen.run_dir,
            &frozen.run_id,
            EngramVerificationRecordProgress::Pending { reason: reason.clone() },
        );
        EngramVerificationRecordStep::Pending(reason)
    };
    let link = match link {
        Ok(link) => link,
        Err(reason) => return pending(reason),
    };
    let support = link
        .adapter
        .verification_record_route_support(link.connection);
    if !support.is_supported() {
        return pending(EngramVerificationRecordPending::CapabilityUnsupported { support });
    }
    let rejected = |detail: String| {
        eprintln!("engram> run={} verification record not settled: {detail}", frozen.run_id);
        engram_verification_record_set_progress(
            &frozen.run_dir,
            &frozen.run_id,
            EngramVerificationRecordProgress::Rejected { detail: detail.clone() },
        );
        EngramVerificationRecordStep::Rejected(detail)
    };
    // Engram validates both command labels before association. Refuse the
    // original command explicitly rather than changing its text/fingerprint.
    const COMMAND_LIMIT: usize = 8192;
    for (field, command) in std::iter::once(("run.command".to_owned(), &frozen.body.run.command))
        .chain(frozen.body.checks.iter().enumerate()
            .map(|(index, check)| (format!("checks[{index}].command"), &check.command))) {
        if command.len() > COMMAND_LIMIT {
            return rejected(format!("verification_command_too_long: {field} has {} UTF-8 bytes; Engram limit is {COMMAND_LIMIT} bytes", command.len()));
        }
    }
    let request = engram_verification_record_request(&frozen);
    let reply = link.adapter.request(link.connection, &request, link.timeout);
    match reply {
        Ok(value) => {
            let receipt = match parse_engram_result::<EngramVerificationRecordReceipt>(value) {
                Ok(receipt) => receipt,
                Err(_) => return rejected("the receipt is unreadable".to_owned()),
            };
            if let Err(why) = engram_verification_record_receipt_check(&frozen.body, &receipt) {
                return rejected(why.to_owned());
            }
            // Held beside the run before anything else, so no later look
            // sends again; marked settled only once its records reached the
            // run's associations durably
            // (`AppState::complete_engram_verification_record`).
            engram_verification_record_set_progress(
                &frozen.run_dir,
                &frozen.run_id,
                EngramVerificationRecordProgress::Received { receipt: receipt.clone() },
            );
            EngramVerificationRecordStep::Received { body: frozen.body, receipt }
        }
        // A refusal from a child that no longer serves the connection (it was
        // respawned under the send) says nothing about this record: the
        // capability is read again before it is sent again.
        Err(error)
            if error.kind == EngramTransportErrorKind::Remote
                && !link
                    .adapter
                    .verification_record_route_support(link.connection)
                    .is_supported() =>
        {
            pending(EngramVerificationRecordPending::CapabilityUnsupported {
                support: EngramVerificationRecordSupport::NotRead,
            })
        }
        // Only a definite refusal of the record itself is final.
        Err(error)
            if error.kind == EngramTransportErrorKind::Remote
                && error
                    .code
                    .as_deref()
                    .is_some_and(|code| ENGRAM_VERIFICATION_RECORD_DEFINITE_REFUSALS.contains(&code)) =>
        {
            rejected(format!(
                "Engram refused it: {}",
                error.code.as_deref().unwrap_or("no code")
            ))
        }
        // Any other answer, transient (a deadline, a busy or unavailable
        // control, a spent budget) or unknown to this host, says nothing
        // final about the record: it stays pending and the same frozen body
        // goes again under the same key.
        Err(error) if error.kind == EngramTransportErrorKind::Remote => {
            pending(EngramVerificationRecordPending::RemoteDeferred {
                code: error.code.clone().unwrap_or_else(|| "no code".to_owned()),
            })
        }
        Err(error) => pending(EngramVerificationRecordPending::TransportFailed {
            error_kind: format!("{:?}", error.kind),
        }),
    }
}

/// The source basis of a run's checks in its original named root. Current
/// records retain the complete owner value; historical commit records stay
/// in their stored format. Staged inputs still supply no producer basis.
fn engram_verification_record_basis(body: &EngramVerificationRecordBody) -> Option<EngramExecutionSourceBasis> {
    let (source_id, source_revision) = match &body.input {
        EngramVerificationRecordInput::SourceId { source_id } => (Some(source_id.clone()), String::new()),
        EngramVerificationRecordInput::Commit { commit } =>
            (None, format!("{COMMITTED_SOURCE_REVISION_SCHEME}:{commit}")),
        EngramVerificationRecordInput::StagedTree { .. } => return None,
    };
    Some(EngramExecutionSourceBasis {
        source_id,
        workspace_id: body.registration.root.workspace_id.clone(),
        source_revision,
        source_root_generation: Some(body.registration.root.generation),
        source_root_state: Some(EngramSourceRootState::Named),
    })
}

/// The producer observation each recorded test check of `run_id` stands
/// for, with its stage: the observations a receipt's records settle.
fn engram_verification_record_producers(
    run_id: &str,
    body: &EngramVerificationRecordBody,
) -> Vec<(String, EngramExecutionObservationInput)> {
    let Some(basis) = engram_verification_record_basis(body) else {
        return Vec::new();
    };
    body.checks
        .iter()
        // Only the test checks the producer records a verification for.
        .filter(|check| {
            check.check_kind == EngramVerificationRecordCheckKind::Test
                && check.completed_at.is_some()
                && check.outcome != EngramVerificationRecordCheckOutcome::NotRun
        })
        .map(|check| {
            (
                check.stage.clone(),
                EngramExecutionObservationInput {
                    observation_id: sha256_hex(format!("termal-run:{run_id}:{}", check.stage).as_bytes()),
                    action_fingerprint: check.check_fingerprint.clone(),
                    effect: EngramEffect::Observe,
                    outcome: match check.outcome {
                        EngramVerificationRecordCheckOutcome::Passed => EngramExecutionOutcome::Succeeded,
                        EngramVerificationRecordCheckOutcome::Failed => EngramExecutionOutcome::Failed,
                        EngramVerificationRecordCheckOutcome::NotRun
                        | EngramVerificationRecordCheckOutcome::Indeterminate => EngramExecutionOutcome::Unknown,
                    },
                    source_changed: false,
                    source_basis: Some(basis.clone()),
                    observed_at: check.completed_at.clone(),
                },
            )
        })
        .collect()
}

/// Where the passes over the registered runs go on from: the run the last
/// one looked at last. Each pass starts after it, in run id order, wrapping,
/// so a run whose calls spend a pass's budget goes behind the runs it held
/// up, and every run is looked at within a bounded number of passes. Kept by
/// the tick's own thread (src/engram_retry_schedule.rs) across its passes,
/// in memory only, with each run's backoff.
#[derive(Debug, Default)]
struct EngramVerificationRecordCursor {
    last: Option<String>,
    backoff: HashMap<String, EngramVerificationRecordBackoff>,
}

#[derive(Debug)]
struct EngramVerificationRecordBackoff {
    next: std::time::Instant,
    wait: Duration,
    ack: Option<TestRunVerificationAck>,
}

impl EngramVerificationRecordCursor {
    fn waits(&mut self, registration: &TestRunRegistration, now: std::time::Instant) -> bool {
        if self.backoff.get(&registration.run_id)
            .is_some_and(|backoff| backoff.ack != registration.verification_record)
        {
            self.backoff.remove(&registration.run_id);
        }
        self.backoff.get(&registration.run_id).is_some_and(|backoff| now < backoff.next)
    }

    fn back_off(&mut self, registration: &TestRunRegistration, now: std::time::Instant) {
        let wait = self.backoff.get(&registration.run_id)
            .map_or(ENGRAM_VERIFICATION_RECORD_BACKOFF_START, |backoff|
                (backoff.wait * 2).min(ENGRAM_VERIFICATION_RECORD_BACKOFF_MAX));
        self.backoff.insert(registration.run_id.clone(), EngramVerificationRecordBackoff {
            next: now + wait, wait, ack: registration.verification_record.clone(),
        });
    }

    fn retain(&mut self, selected: &[TestRunRegistration]) {
        let runs: HashSet<&str> = selected.iter().map(|registration| registration.run_id.as_str()).collect();
        self.backoff.retain(|run_id, _| runs.contains(run_id.as_str()));
    }

    fn order(&self, mut registrations: Vec<TestRunRegistration>) -> Vec<TestRunRegistration> {
        registrations.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        let start = self.last.as_ref().map_or(0, |last| {
            registrations.iter().position(|registration| registration.run_id > *last).unwrap_or(0)
        });
        registrations.rotate_left(start);
        registrations
    }
}

/// Whether the observation `row` is this registered run's own launch.
fn engram_verification_record_row_matches(
    row: &TestRunCommandObservation,
    registration: &TestRunRegistration,
) -> bool {
    row.summary.owner_session_id.as_deref() == Some(registration.initiating_session_id.as_str())
        && row.artifact.as_ref().is_some_and(|artifact| {
            artifact.run_id == registration.run_id
                && test_run_path_key(&artifact.run_directory)
                    == test_run_path_key(&registration.run_directory)
                && artifact.request_identity.as_ref() == Some(&registration.request_identity)
        })
}

impl AppState {
    /// One pass over the registered, bound runs within
    /// `ENGRAM_VERIFICATION_RECORD_TICK_BUDGET`; tests call it directly.
    #[cfg_attr(not(test), allow(dead_code))]
    fn engram_verification_records_tick(&self) {
        self.engram_verification_records_tick_within(ENGRAM_VERIFICATION_RECORD_TICK_BUDGET, &|| false);
    }

    /// One pass over the registered, bound runs: each that emits is
    /// prepared off the lock, routed to its initiating session's bound
    /// connection and sent off the lock; a held receipt is settled with no
    /// send. The pass ends once `budget` is spent or `stopping` (the host's
    /// shutdown) says so, both looked at before each run, and each Engram call
    /// and durability wait gets at most what is left of `budget`. Driven by a
    /// thread of its own (src/engram_retry_schedule.rs). Returns how many runs
    /// it looked at.
    #[cfg_attr(not(test), allow(dead_code))]
    fn engram_verification_records_tick_within(&self, budget: Duration, stopping: &dyn Fn() -> bool) -> usize {
        self.engram_verification_records_tick_from(budget, stopping, &mut EngramVerificationRecordCursor::default())
    }

    /// `engram_verification_records_tick_within`, starting after the run
    /// `cursor` names and leaving it at the last run this pass looked at.
    /// A run whose look left it pending, or its receipt unsettled, waits out
    /// its backoff in `cursor` before the next look.
    fn engram_verification_records_tick_from(
        &self,
        budget: Duration,
        stopping: &dyn Fn() -> bool,
        cursor: &mut EngramVerificationRecordCursor,
    ) -> usize {
        let clock = self.engram_budget_clock();
        let deadline = clock.now() + budget;
        // A settled or rejected run is finished: it is not selected, so
        // none of its files is read again.
        let registrations: Vec<TestRunRegistration> = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .test_run_registrations
                .values()
                .filter(|registration| {
                    inner.test_runs.registered.contains(&registration.run_id)
                        && engram_verification_record_registration_bound(registration)
                        && !matches!(
                            registration.verification_record,
                            Some(TestRunVerificationAck::Settled { .. } | TestRunVerificationAck::Rejected { .. })
                        )
                })
                .cloned()
                .collect()
        };
        cursor.retain(&registrations);
        let mut looked = 0;
        for registration in cursor.order(registrations) {
            if stopping() || clock.now() >= deadline {
                break;
            }
            // A run in its backoff is neither read nor routed.
            if cursor.waits(&registration, clock.now()) {
                continue;
            }
            looked += 1;
            cursor.last = Some(registration.run_id.clone());
            let settled = |settlement: EngramVerificationRecordSettlement| {
                settlement == EngramVerificationRecordSettlement::Settled
            };
            let done = match engram_verification_record_prepare(&registration) {
                EngramVerificationRecordPrepared::Ready(frozen) => {
                    let route = self.engram_verification_record_route_until(&registration, deadline);
                    let step = engram_verification_record_send(
                        frozen,
                        route.as_ref().map(EngramVerificationRecordRoute::link).map_err(Clone::clone),
                    );
                    self.note_engram_verification_record_step(&registration, &step);
                    match step {
                        EngramVerificationRecordStep::Received { body, receipt } => settled(
                            self.complete_engram_verification_record_until(&registration, &body, &receipt, deadline),
                        ),
                        EngramVerificationRecordStep::Rejected(_) => true,
                        EngramVerificationRecordStep::Pending(_) => false,
                    }
                }
                EngramVerificationRecordPrepared::ReceiptHeld { frozen, receipt } => settled(
                    self.complete_engram_verification_record_until(&registration, &frozen.body, &receipt, deadline),
                ),
                EngramVerificationRecordPrepared::Pending(_) => false,
                EngramVerificationRecordPrepared::NotEligible
                | EngramVerificationRecordPrepared::Waiting
                | EngramVerificationRecordPrepared::Finished => true,
            };
            // Left pending, or with its receipt unsettled: looked at again
            // only after its backoff, unless its acknowledgment changes first.
            if done {
                cursor.backoff.remove(&registration.run_id);
            } else {
                cursor.back_off(&registration, clock.now());
            }
        }
        looked
    }

    /// `complete_engram_verification_record_until`, waiting at most
    /// `ENGRAM_VERIFICATION_RECORD_SETTLE_FENCE` for durability.
    #[cfg_attr(not(test), allow(dead_code))]
    fn complete_engram_verification_record(
        &self,
        registration: &TestRunRegistration,
        body: &EngramVerificationRecordBody,
        receipt: &EngramVerificationRecordReceipt,
    ) -> EngramVerificationRecordSettlement {
        let deadline = self.engram_budget_clock().now() + ENGRAM_VERIFICATION_RECORD_SETTLE_FENCE;
        self.complete_engram_verification_record_until(registration, body, receipt, deadline)
    }

    /// Settles a valid receipt's records into the run's own launch
    /// observation, and marks the record settled only once every record
    /// reached a matching row and each such row is read back durable before
    /// `deadline`. Otherwise the receipt stays held beside the run (received)
    /// and a later look settles it again from the receipt alone: never a
    /// resend. Every step is idempotent, so a crash anywhere between them is
    /// repaired by the next look.
    fn complete_engram_verification_record_until(
        &self,
        registration: &TestRunRegistration,
        body: &EngramVerificationRecordBody,
        receipt: &EngramVerificationRecordReceipt,
        deadline: std::time::Instant,
    ) -> EngramVerificationRecordSettlement {
        // Optional presentation must never spend the receipt's durability
        // deadline. Finish settlement first, including all early outcomes,
        // then queue the owned notice worker within the caller's remaining
        // time and its presentation cap. Never extend a verification tick.
        let settled = self.complete_engram_verification_record_state_until(
            registration, body, receipt, deadline);
        if std::time::Instant::now() < deadline
            && !matches!(settled, EngramVerificationRecordSettlement::ReceiptMismatch { .. }
                | EngramVerificationRecordSettlement::AcknowledgedBodyChanged
                | EngramVerificationRecordSettlement::NotDurable { .. }) {
            self.schedule_received_launcher_notice(registration,
                deadline.min(std::time::Instant::now() + RECEIVED_NOTICE_CAPTURE_BUDGET));
        }
        settled
    }

    fn complete_engram_verification_record_state_until(
        &self,
        registration: &TestRunRegistration,
        body: &EngramVerificationRecordBody,
        receipt: &EngramVerificationRecordReceipt,
        deadline: std::time::Instant,
    ) -> EngramVerificationRecordSettlement {
        // Only a receipt that answers exactly this body settles anything.
        if let Err(why) = engram_verification_record_receipt_check(body, receipt) {
            return EngramVerificationRecordSettlement::ReceiptMismatch { detail: why.to_owned() };
        }
        // A receipt the host holds answers one body only: never settled
        // into, nor rebound to, another body of the same run.
        let body_digest = engram_verification_record_body_digest(body);
        let held_elsewhere = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .test_run_registrations
            .get(&registration.run_id)
            .and_then(|row| row.verification_record.as_ref())
            .and_then(TestRunVerificationAck::body_digest)
            .is_some_and(|held| held != body_digest);
        if held_elsewhere || body_digest.is_empty() {
            return EngramVerificationRecordSettlement::AcknowledgedBodyChanged;
        }
        let run_dir = FsPath::new(&registration.run_directory);
        self.note_engram_verification_record_ack(
            registration,
            TestRunVerificationAck::Received {
                receipt: receipt.clone(),
                body_digest: body_digest.clone(),
            },
        );
        engram_verification_record_set_progress(
            run_dir,
            &registration.run_id,
            EngramVerificationRecordProgress::Received { receipt: receipt.clone() },
        );
        let rows = match self.settle_engram_verification_record(registration, body, receipt) {
            EngramVerificationRecordAssociation::Associated(rows) => rows,
            EngramVerificationRecordAssociation::NoLaunchRow => {
                return EngramVerificationRecordSettlement::NoLaunchRow;
            }
        };
        let clock = self.engram_budget_clock();
        for row in rows {
            let execution_id = row.execution_id.clone();
            if let Err(detail) = self.confirm_engram_fence_target_until(
                PersistFenceTarget::TestRunObservation(Box::new(row)),
                clock.clone(),
                deadline,
            ) {
                eprintln!(
                    "engram> run={} verification record held: its association on {execution_id} \
                     is not confirmed durable: {detail}",
                    registration.run_id
                );
                return EngramVerificationRecordSettlement::NotDurable { detail };
            }
        }
        self.note_engram_verification_record_ack(
            registration,
            TestRunVerificationAck::Settled {
                receipt: receipt.clone(),
                body_digest,
            },
        );
        engram_verification_record_set_progress(
            run_dir,
            &registration.run_id,
            EngramVerificationRecordProgress::Settled { receipt: receipt.clone() },
        );
        EngramVerificationRecordSettlement::Settled
    }

    /// Waits, until `deadline` on `clock`, for the persistence worker to read
    /// `target` back from the committed store: the durable confirmation, not
    /// a queued write. A host without a running worker (its channel closed:
    /// shutdown, or a test state built without one) saves the whole state
    /// synchronously instead, which is itself the durable write. Every
    /// failure, the deadline included, is returned; the caller settles
    /// nothing on it. Blocking: the caller holds no state lock.
    fn confirm_engram_fence_target_until(
        &self,
        target: PersistFenceTarget,
        clock: EngramBudgetClock,
        deadline: std::time::Instant,
    ) -> std::result::Result<(), String> {
        let (fence, waiter) = PersistFence::new_with_clock(target, deadline, clock);
        if self.persist_tx.send(PersistRequest::Fence(Box::new(fence))).is_ok() {
            return waiter.wait().map_err(|error| format!("{error:?}"));
        }
        let inner = self.inner.lock().expect("state mutex poisoned");
        self.persist_internal_locked(&inner)
            .map_err(|error| format!("{error:#}"))
    }

    /// Keeps what one send step observed on the run's registration: a
    /// received receipt or a rejection; a pending step changes nothing.
    fn note_engram_verification_record_step(
        &self,
        registration: &TestRunRegistration,
        step: &EngramVerificationRecordStep,
    ) {
        let ack = match step {
            EngramVerificationRecordStep::Received { body, receipt } => TestRunVerificationAck::Received {
                receipt: receipt.clone(),
                body_digest: engram_verification_record_body_digest(body),
            },
            EngramVerificationRecordStep::Rejected(detail) => {
                TestRunVerificationAck::Rejected { detail: detail.clone() }
            }
            EngramVerificationRecordStep::Pending(_) => return,
        };
        self.note_engram_verification_record_ack(registration, ack);
    }

    /// Keeps `ack`, which the host itself observed, on `registration`'s row
    /// in state under the state lock, and persists it with the registrations.
    /// Only the sender's own observations come here: a receipt it received
    /// and checked, a settlement read back durable, or a refusal. A received
    /// receipt never replaces a finished record, and a row that is gone or is
    /// no longer this run's is left alone.
    fn note_engram_verification_record_ack(&self, registration: &TestRunRegistration, ack: TestRunVerificationAck) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let changes = inner.test_run_registrations.get(&registration.run_id).is_some_and(|row| {
            row.request_identity == registration.request_identity
                && row.run_directory == registration.run_directory
                && row.verification_record.as_ref() != Some(&ack)
                && !(matches!(ack, TestRunVerificationAck::Received { .. })
                    && matches!(
                        row.verification_record,
                        Some(TestRunVerificationAck::Settled { .. } | TestRunVerificationAck::Rejected { .. })
                    ))
                // A receipt held for one body is never rebound to another.
                && !row
                    .verification_record
                    .as_ref()
                    .and_then(TestRunVerificationAck::body_digest)
                    .zip(ack.body_digest())
                    .is_some_and(|(held, new)| held != new)
        });
        if !changes {
            return;
        }
        inner
            .test_run_registrations
            .update_unindexed_fields(&registration.run_id, |row| row.verification_record = Some(ack));
        if let Err(error) = self.persist_internal_locked(&inner) {
            eprintln!(
                "engram> run={} verification record outcome not persisted: {error:#}",
                registration.run_id
            );
        }
    }

    /// `engram_verification_record_route_until` within one tick's budget.
    #[cfg_attr(not(test), allow(dead_code))]
    fn engram_verification_record_route(
        &self,
        registration: &TestRunRegistration,
    ) -> std::result::Result<EngramVerificationRecordRoute, EngramVerificationRecordPending> {
        let deadline = self.engram_budget_clock().now() + ENGRAM_VERIFICATION_RECORD_TICK_BUDGET;
        self.engram_verification_record_route_until(registration, deadline)
    }

    /// The initiating session's ordinary work connection, never the owning
    /// session's or a reader's. Verify the registration's exact store and
    /// the serving child's work-session identity/capabilities. No control
    /// token, naming bind or admission recovery is needed or changed.
    /// Engram validates the registration's own work binding independently.
    /// Every call and the returned route stay inside `deadline`.
    fn engram_verification_record_route_until(
        &self,
        registration: &TestRunRegistration,
        deadline: std::time::Instant,
    ) -> std::result::Result<EngramVerificationRecordRoute, EngramVerificationRecordPending> {
        let session_id = registration.initiating_session_id.as_str();
        let target = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if inner.find_session_index(session_id).is_none() {
                return Err(EngramVerificationRecordPending::InitiatingSessionGone);
            }
            let target = Self::engram_source_root_target_locked(&inner, session_id)
                .ok()
                .flatten()
                .ok_or(EngramVerificationRecordPending::NoConnection)?;
            if registration.authority_store.is_none()
                || target.settings.authority_store_key.as_ref() != registration.authority_store.as_ref()
            {
                return Err(EngramVerificationRecordPending::StoreChanged);
            }
            target
        };
        // What is left of `deadline` for one call; none is started without.
        let timeout_left = || {
            let left = deadline.saturating_duration_since(target.budget_clock.now());
            if left.is_zero() {
                return Err(EngramVerificationRecordPending::TransportFailed {
                    error_kind: format!("{:?}", EngramTransportErrorKind::Deadline),
                });
            }
            Ok(target.settings.call_timeout().min(left))
        };
        target.adapter.read_work_session(
            &target.connection, registration.authority_store.as_ref().expect("checked store"), timeout_left()?,
        ).map_err(|error| EngramVerificationRecordPending::TransportFailed {
            error_kind: format!("{:?}", error.kind),
        })?;
        Ok(EngramVerificationRecordRoute {
            adapter: Arc::clone(&target.adapter),
            connection: target.connection.clone(),
            timeout: timeout_left()?,
        })
    }

    /// A valid receipt's records reach the run's own launch observations:
    /// each test check's producer is associated with them, unsettled, and
    /// the shared settlement fills the records in, as for a checkpoint
    /// receipt, but with no checkpoint or cursor. Returns the run's own
    /// launch rows as they now stand when each carries every record the
    /// receipt names for a test check; none while no row of the run carries
    /// its artifact (it attaches once the launcher exits).
    fn settle_engram_verification_record(
        &self,
        registration: &TestRunRegistration,
        body: &EngramVerificationRecordBody,
        receipt: &EngramVerificationRecordReceipt,
    ) -> EngramVerificationRecordAssociation {
        let (Some(store), Some(binding)) =
            (registration.authority_store.as_ref(), registration.work_binding.as_ref())
        else {
            return EngramVerificationRecordAssociation::NoLaunchRow;
        };
        let producers = engram_verification_record_producers(&registration.run_id, body);
        let records: HashMap<&str, &String> = receipt
            .verifications
            .iter()
            .map(|verification| (verification.stage.as_str(), &verification.record_id))
            .collect();
        // The records the receipt names for the producers: each must end on
        // the run's own launch row before the record is settled.
        let expected: Vec<(&str, &String)> = producers
            .iter()
            .filter_map(|(stage, producer)| {
                records
                    .get(stage.as_str())
                    .map(|record_id| (producer.observation_id.as_str(), *record_id))
            })
            .collect();
        if expected.is_empty() {
            // Nothing of this run is associated with a launch row.
            return EngramVerificationRecordAssociation::Associated(Vec::new());
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let keys: Vec<String> = inner
            .test_run_observations
            .index()
            .artifact_run(&registration.run_id)
            .cloned()
            .collect();
        let mut changed = BTreeMap::new();
        for key in &keys {
            let attached = inner.test_run_observations.update(key, |row| {
                if !engram_verification_record_row_matches(row, registration) {
                    return None;
                }
                let mut attached = false;
                for (_, producer) in &producers {
                    if row.evidence.iter().any(|association| {
                        association.producer_observation_id == producer.observation_id
                            && &association.store == store
                            && &association.binding == binding
                    }) {
                        continue;
                    }
                    let Some(source_basis) = producer.source_basis.clone() else {
                        continue;
                    };
                    row.validated_unbound = false;
                    row.final_unbound_proof = None;
                    row.evidence.push(TestRunEvidenceAssociation {
                        store: store.clone(),
                        binding: binding.clone(),
                        source_basis,
                        producer_observation_id: producer.observation_id.clone(),
                        action_fingerprint: producer.action_fingerprint.clone(),
                        reported_outcome: producer.outcome,
                        verification_record_id: None,
                        checkpoint_id: None,
                        confirmed_cursor: None,
                        lifecycle_read: None,
                    });
                    attached = true;
                }
                attached.then(|| row.clone())
            });
            if let Some(Some(row)) = attached {
                changed.insert(row.execution_id.clone(), row);
            }
        }
        let mut settlements = HashMap::<&str, Vec<(&EngramExecutionObservationInput, &String)>>::new();
        for (stage, producer) in &producers {
            if let Some(record_id) = records.get(stage.as_str()) {
                settlements
                    .entry(producer.observation_id.as_str())
                    .or_default()
                    .push((producer, *record_id));
            }
        }
        for row in Self::settle_test_run_producer_records_locked(
            &mut inner,
            settlements,
            store,
            binding,
        ) {
            changed.insert(row.execution_id.clone(), row);
        }
        self.persist_test_run_evidence_batch_locked(&mut inner, changed.into_values().collect(), Vec::new());
        let rows: Vec<TestRunCommandObservation> = keys
            .iter()
            .filter_map(|key| inner.test_run_observations.get(key))
            .filter(|row| engram_verification_record_row_matches(row, registration))
            .filter(|row| {
                expected.iter().all(|(observation_id, record_id)| {
                    row.evidence.iter().any(|association| {
                        association.producer_observation_id == *observation_id
                            && &association.store == store
                            && &association.binding == binding
                            && association.verification_record_id.as_ref() == Some(*record_id)
                    })
                })
            })
            .cloned()
            .collect();
        if rows.is_empty() {
            EngramVerificationRecordAssociation::NoLaunchRow
        } else {
            EngramVerificationRecordAssociation::Associated(rows)
        }
    }
}
