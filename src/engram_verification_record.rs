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

use super::*;

/// The frozen body's file in the run directory: written once, never rebuilt.
pub(crate) const ENGRAM_VERIFICATION_RECORD_BODY_FILE: &str = "engram-verification-record.json";

/// The record's progress beside the body: pending (and why), settled, or
/// rejected. Informational: a received, settled or rejected record is decided
/// by the host's own `TestRunVerificationAck` on the registration, never by
/// this file, which an agent can write.
pub(crate) const ENGRAM_VERIFICATION_RECORD_STATE_FILE: &str =
    "engram-verification-record-state.json";

/// The most settled runs one producer pass writes the summary's state-file
/// copy for again (`repair_settled_verification_mirrors`).
const ENGRAM_VERIFICATION_MIRROR_REPAIRS_PER_TICK: usize = 16;

/// The most one pass over the registered runs may take, on its own thread
/// (src/engram_retry_schedule.rs), so it never holds up the owed binds; each
/// Engram call and durability wait gets what is left of it.
pub(crate) const ENGRAM_VERIFICATION_RECORD_TICK_BUDGET: Duration = ENGRAM_MIN_CALL_TIMEOUT;

/// How long the settlement of a held receipt waits for its associations to be
/// read back durable when no tick budget bounds it.
const ENGRAM_VERIFICATION_RECORD_SETTLE_FENCE: Duration = Duration::from_secs(5);

/// The first wait before a run left pending, or with its receipt unsettled,
/// is looked at again; each further look that leaves it so doubles the wait,
/// up to `ENGRAM_VERIFICATION_RECORD_BACKOFF_MAX`.
pub(crate) const ENGRAM_VERIFICATION_RECORD_BACKOFF_START: Duration = Duration::from_secs(2);

/// The longest wait between two looks at a run that stays pending.
pub(crate) const ENGRAM_VERIFICATION_RECORD_BACKOFF_MAX: Duration = Duration::from_secs(5 * 60);

/// Engram's refusals of the record itself: the producer's own refusal and a
/// request it cannot decode or validate. Only these are final; any other
/// remote code, known transient or unknown, leaves the record pending and the
/// same frozen body goes again under the same key.
const ENGRAM_VERIFICATION_RECORD_DEFINITE_REFUSALS: &[&str] =
    &["verification_record_refused", "invalid_request"];

/// The idempotency key of one run's record: one key per host run, so every
/// retry, before or after a restart, names the same record.
pub(crate) fn engram_verification_record_key(run_id: &str) -> String {
    format!("termal-run:{run_id}")
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct EngramVerificationRecordBody {
    pub(crate) registration: EngramVerificationRecordRegistration,
    pub(crate) input: EngramVerificationRecordInput,
    pub(crate) run: EngramVerificationRecordRun,
    pub(crate) checks: Vec<EngramVerificationRecordCheck>,
    pub(crate) post_run: EngramVerificationRecordPostRun,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct EngramVerificationRecordRegistration {
    pub(crate) host_run_id: String,
    pub(crate) request_identity: String,
    pub(crate) registered_at: String,
    pub(crate) initiating_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) owning_session_id: Option<String>,
    pub(crate) binding: EngramControlWorkBinding,
    pub(crate) root: EngramVerificationRecordRoot,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct EngramVerificationRecordRoot {
    pub(crate) workspace_id: String,
    /// Positive and within `i64`, as the producer requires.
    pub(crate) generation: i64,
    pub(crate) state: EngramVerificationRecordRootState,
}

/// A registered run's root was named (bound) when it registered.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EngramVerificationRecordRootState {
    Named,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum EngramVerificationRecordInput {
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
    pub(crate) fn clean_commit(&self) -> Option<&str> {
        match self {
            Self::Commit { commit } => Some(commit),
            Self::SourceId { source_id } if source_id.is_clean() => Some(&source_id.commit),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for EngramVerificationRecordInput {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if value.get("kind").and_then(Value::as_str) == Some("source_id") {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct SourceInput {
                #[serde(rename = "kind")]
                _kind: String,
                source_id: EngramSourceId,
            }
            let input: SourceInput =
                serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            return Ok(Self::SourceId {
                source_id: input.source_id,
            });
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
            HistoricalInput::StagedTree { tree, base_commit } => {
                Ok(Self::StagedTree { tree, base_commit })
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EngramVerificationRecordMode {
    Full,
    Focused,
    Live,
}

impl EngramVerificationRecordMode {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Focused => "focused",
            Self::Live => "live",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EngramVerificationRecordRunOutcome {
    Passed,
    Failed,
    /// The launcher recorded the run as interrupted.
    Indeterminate,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct EngramVerificationRecordRun {
    pub(crate) mode: EngramVerificationRecordMode,
    /// The launcher invocation, rendered as TermAl's normalised command line
    /// (`engram_verification_record_command_text`).
    pub(crate) command: String,
    pub(crate) started_at: String,
    pub(crate) finished_at: String,
    pub(crate) outcome: EngramVerificationRecordRunOutcome,
}

/// A check's kind on the wire, lowercase as the producer's
/// `VerificationKind` reads it.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum EngramVerificationRecordCheckKind {
    Test,
    Build,
    Lint,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EngramVerificationRecordCheckOutcome {
    Passed,
    Failed,
    /// The stage never started.
    NotRun,
    /// The stage was running when the run ended: interrupted.
    Indeterminate,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct EngramVerificationRecordCheck {
    pub(crate) stage: String,
    pub(crate) check_kind: EngramVerificationRecordCheckKind,
    /// The stage's actual argv as TermAl's normalised command line: the very
    /// text its check fingerprint hashes.
    pub(crate) command: String,
    pub(crate) check_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) completed_at: Option<String>,
    pub(crate) outcome: EngramVerificationRecordCheckOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) refs: Vec<String>,
    /// The stage's own numeric test counts, as the launcher recorded them:
    /// non-negative integers only, at most `ENGRAM_VERIFICATION_RECORD_MAX_COUNTS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) counts: Option<BTreeMap<String, u64>>,
}

/// The most count keys the producer admits on one check.
const ENGRAM_VERIFICATION_RECORD_MAX_COUNTS: usize = 32;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub(crate) enum EngramVerificationRecordPostRun {
    SourceId(EngramVerificationRecordSourcePostRun),
    Commit(EngramVerificationRecordCommitPostRun),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EngramVerificationRecordSourcePostRun {
    pub(crate) source_id: EngramSourceId,
}

impl<'de> Deserialize<'de> for EngramVerificationRecordPostRun {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        // A present new field selects strict decoding, including explicit
        // null and mixed legacy fields. It never falls back to Commit.
        if value.get("source_id").is_some() {
            serde_json::from_value(value)
                .map(Self::SourceId)
                .map_err(serde::de::Error::custom)
        } else {
            serde_json::from_value(value)
                .map(Self::Commit)
                .map_err(serde::de::Error::custom)
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct EngramVerificationRecordCommitPostRun {
    pub(crate) outcome: TestRunCommittedInputOutcome,
    pub(crate) clean_before: bool,
    pub(crate) clean_after: bool,
    /// Paths only; empty, and so left out, for the clean runs the producer
    /// admits today.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) dirty_paths: Vec<String>,
    /// No disposition flow exists yet; reserved for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) disposition: Option<String>,
}

/// Engram's answer to a `verification_record`: `operation` and `producer`
/// both name the run's own `registered_host_run` object.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct EngramVerificationRecordReceipt {
    pub(crate) operation: String,
    pub(crate) producer: String,
    pub(crate) verifications: Vec<EngramVerificationRecordVerification>,
    pub(crate) run_outcome: EngramVerificationRecordRunOutcome,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct EngramVerificationRecordVerification {
    pub(crate) stage: String,
    pub(crate) record_id: String,
}

/// Why a frozen record has not been sent, or not settled.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum EngramVerificationRecordPending {
    /// The initiating session's ordinary work-session answer does not list
    /// the capability under the current connection and child generation.
    CapabilityUnsupported {
        support: EngramVerificationRecordSupport,
    },
    /// The initiating session no longer exists. No other session's
    /// connection is ever used in its place.
    InitiatingSessionGone,
    /// The initiating session's project has no enabled Engram connection.
    NoConnection,
    /// Historical progress only; current routing does not emit this reason.
    AwaitingNamingBinding,
    /// A stage is neither a test, build nor admitted lint check. Preparation
    /// names it without building a body; the tick rejects a completed generic
    /// run locally. Declared-native runs await their original end as Waiting,
    /// before classification; their exclusion from local rejection is defensive.
    UnclassifiedStages { stages: Vec<String> },
    /// The run's checkout was not clean after it. The producer admits only a
    /// clean post-run input today, so nothing is sent.
    PostRunNotClean {
        outcome: TestRunCommittedInputOutcome,
    },
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
    SourceChanged {
        detail: String,
        changes: Vec<EngramSourceIdDifference>,
    },
    /// The call failed without an answer from Engram; it is sent again.
    TransportFailed { error_kind: String },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum EngramVerificationRecordProgress {
    Pending {
        reason: EngramVerificationRecordPending,
    },
    /// Engram answered with a valid receipt: never sent again. Its records
    /// have not yet reached the run's own launch observation durably (the
    /// row's artifact may attach only after the launcher exits); a later look
    /// settles them from this receipt alone.
    Received {
        receipt: EngramVerificationRecordReceipt,
    },
    Settled {
        receipt: EngramVerificationRecordReceipt,
    },
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
pub(crate) enum TestRunVerificationAck {
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
    /// The host saw Engram refuse the record, answer it unreadably, or locally
    /// retired an unsent legacy input.
    Rejected {
        detail: String,
        /// Kept only for a definitive producer reply, from the body sent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        producer_body_digest: Option<String>,
    },
    /// Final local refusal before any producer send. Its presentation can be
    /// repaired after restart without reopening delivery or granting credit.
    LocalUnclassified { detail: String },
}

impl TestRunVerificationAck {
    /// The digest of the body a received or settled receipt answered.
    pub(crate) fn body_digest(&self) -> Option<&str> {
        match self {
            Self::Received { body_digest, .. } | Self::Settled { body_digest, .. } => {
                Some(body_digest)
            }
            Self::Rejected { .. } | Self::LocalUnclassified { .. } => None,
        }
    }
}

/// The SHA-256 of `body`'s canonical text (`engram_verification_record_text`,
/// the frozen bytes the host writes and sends); empty when it does not
/// serialize, which no acknowledgment ever matches.
pub(crate) fn engram_verification_record_body_digest(
    body: &EngramVerificationRecordBody,
) -> String {
    engram_verification_record_text(body)
        .map(|text| sha256_hex(text.as_bytes()))
        .unwrap_or_default()
}

/// Whether `digest`, kept on an acknowledgment, is the digest of `body`.
pub(crate) fn engram_verification_record_digest_answers(
    digest: &str,
    body: &EngramVerificationRecordBody,
) -> bool {
    !digest.is_empty() && engram_verification_record_body_digest(body) == digest
}

/// One run's frozen record, ready to send.
#[derive(Clone)]
pub(crate) struct EngramVerificationRecordFrozen {
    pub(crate) run_dir: PathBuf,
    pub(crate) run_id: String,
    /// The persisted body; it re-serializes to the exact persisted text.
    pub(crate) record: Value,
    pub(crate) body: EngramVerificationRecordBody,
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
pub(crate) enum EngramVerificationRecordPrepared {
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
pub(crate) enum EngramVerificationRecordStep {
    Pending(EngramVerificationRecordPending),
    Rejected(String),
    ProducerRejected {
        detail: String,
        body_digest: String,
    },
    Received {
        body: EngramVerificationRecordBody,
        receipt: EngramVerificationRecordReceipt,
    },
}

/// What one settlement of a held receipt came to. Only `Settled` marks the
/// record settled; the others leave it received, for a later look.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum EngramVerificationRecordSettlement {
    Settled,
    /// No launch observation of this run carries its artifact yet (it
    /// attaches once the launcher exits), or the records did not all reach
    /// one.
    NoLaunchRow,
    /// The associations were written but not read back durable (`detail`).
    NotDurable {
        detail: String,
    },
    /// The receipt does not answer the body (`detail`): nothing is settled.
    ReceiptMismatch {
        detail: String,
    },
    /// The host already holds a receipt for another body of this run (its
    /// kept digest differs): nothing is settled for this one.
    AcknowledgedBodyChanged,
}

/// What associating a receipt's records found: the run's own launch rows as
/// they now stand, each carrying every record, or none that does.
pub(crate) enum EngramVerificationRecordAssociation {
    Associated(Vec<TestRunCommandObservation>),
    NoLaunchRow,
}

/// The initiating session's ordinary work-session connection. The owning
/// session is audit only.
pub(crate) struct EngramVerificationRecordLink<'a> {
    pub(crate) adapter: &'a EngramHostAdapter,
    pub(crate) connection: &'a EngramConnectionConfig,
    pub(crate) timeout: Duration,
}

/// An owned `EngramVerificationRecordLink`, carried off the state lock: the
/// initiating session's owner-managed ordinary connection.
pub(crate) struct EngramVerificationRecordRoute {
    pub(crate) adapter: Arc<EngramHostAdapter>,
    pub(crate) connection: EngramConnectionConfig,
    pub(crate) timeout: Duration,
}

impl EngramVerificationRecordRoute {
    pub(crate) fn link(&self) -> EngramVerificationRecordLink<'_> {
        EngramVerificationRecordLink {
            adapter: &self.adapter,
            connection: &self.connection,
            timeout: self.timeout,
        }
    }
}

/// Whether a registration is bound the way a committed-input registration is
/// (src/test_run_named_root.rs): a claim, its named root and its store.
pub(crate) fn engram_verification_record_registration_bound(
    registration: &TestRunRegistration,
) -> bool {
    registration.work_binding.is_some()
        && registration.named_root.is_some()
        && registration.authority_store.is_some()
}

/// `word` as one shell word in the normalised form the check fingerprint
/// reads (`engram_shell_words` reads it back to `word`): bare when it holds
/// no whitespace or quote, else double-quoted with the Windows argument rules
/// for backslashes before a quote.
fn engram_verification_record_quote(word: &str) -> String {
    if !word.is_empty()
        && !word
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, '"' | '\''))
    {
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
pub(crate) fn engram_verification_record_check_command(
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
pub(crate) fn engram_verification_record_command_text(argv: &[String]) -> String {
    argv.iter()
        .map(|word| engram_verification_record_quote(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The stage's counts as the producer admits them: only entries whose value
/// is a non-negative integer, none when no such entry remains or more than
/// `ENGRAM_VERIFICATION_RECORD_MAX_COUNTS` do. A runner label is dropped.
pub(crate) fn engram_verification_record_counts(stage: &Value) -> Option<BTreeMap<String, u64>> {
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
            && args
                .iter()
                .map(String::as_str)
                .find(|word| !word.starts_with('+'))
                == Some("check"))
        || program == "tsc"
        || (program == "npx" && args.first().is_some_and(|word| word == "tsc"))
        || (program == "node"
            && args
                .first()
                .is_some_and(|script| script.replace('\\', "/").ends_with("typescript/bin/tsc")))
}

#[cfg(test)]
pub(crate) fn engram_verification_record_check_kind(
    mode: EngramVerificationRecordMode,
    stage: &str,
    argv: &[String],
    test_stages: &[String],
) -> Option<EngramVerificationRecordCheckKind> {
    engram_verification_record_classify_argv(mode, stage, argv, test_stages, true)
}

fn engram_verification_record_full_lint_stage(
    mode: EngramVerificationRecordMode,
    stage: &str,
) -> bool {
    mode == EngramVerificationRecordMode::Full && matches!(stage, "clippy" | "docs")
}

/// Exact lint commands and recognized test/build argv take precedence.
/// Other stages may use the existing launcher test-stage rules. A FULL
/// Clippy or docs row needs argv recognition, so its label cannot grant
/// Test credit.
fn engram_verification_record_classify_argv(
    mode: EngramVerificationRecordMode,
    stage: &str,
    argv: &[String],
    test_stages: &[String],
    admit_full_lint: bool,
) -> Option<EngramVerificationRecordCheckKind> {
    let full_lint_stage = engram_verification_record_full_lint_stage(mode, stage);
    if (!full_lint_stage || admit_full_lint)
        && engram_verification_record_is_lint(mode, stage, argv)
    {
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
    // Only actual test/build argv can override a FULL lint stage's name.
    if full_lint_stage && admit_full_lint {
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
    match (mode, stage, argv) {
        (EngramVerificationRecordMode::Full, "fmt", [program, subcommand, flag]) => {
            engram_program_name(program) == "cargo" && subcommand == "fmt" && flag == "--check"
        }
        (EngramVerificationRecordMode::Full, "clippy", [program, args @ ..]) => {
            engram_program_name(program) == "cargo"
                && !engram_verification_record_is_script_shim(program)
                && args
                    == [
                        "clippy",
                        "--all-targets",
                        "--all-features",
                        "--",
                        "-D",
                        "warnings",
                    ]
        }
        (EngramVerificationRecordMode::Full, "docs", [program, script])
        | (EngramVerificationRecordMode::Focused, "focused", [program, script]) => {
            engram_program_name(program) == "node"
                && !engram_verification_record_is_script_shim(program)
                && script == "scripts/check-doc-links.mjs"
        }
        _ => false,
    }
}

fn engram_verification_record_is_script_shim(program: &str) -> bool {
    let program = program.to_ascii_lowercase();
    [".cmd", ".bat", ".ps1"]
        .iter()
        .any(|suffix| program.ends_with(suffix))
}

/// Launcher stage ids this sender alone records as tests when neither their
/// argv nor the shared stage rule says so: the full gate's Node helper tests
/// run `node --test` behind duration-reporter flags, a form the shared focused
/// classifier does not read. Local to the sender; the shared stage rule
/// (`engram_launcher_test_stages`) and the focused classifier stay as they are.
// Retained results keep the original stage id during record readback.
const ENGRAM_VERIFICATION_RECORD_LAUNCHER_TEST_STAGES: &[&str] =
    &["node-helper-tests", "fingerprint-tests"];

/// A results stage's name and argv. Legacy routes can fall back to the
/// request's plan when no command was recorded.
fn engram_verification_record_stage_argv(
    stage: &Value,
    planned: &HashMap<String, Vec<String>>,
) -> Result<(String, Vec<String>), String> {
    let name = engram_verification_record_stage_name(stage)?;
    let argv = test_run_argv(stage.get("command"))
        .filter(|argv| !argv.is_empty())
        .or_else(|| planned.get(name).cloned())
        .ok_or_else(|| format!("stage {name} names no command"))?;
    Ok((name.to_owned(), argv))
}

fn engram_verification_record_stage_name(stage: &Value) -> Result<&str, String> {
    stage
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| test_run_valid_stage_name(name))
        .ok_or_else(|| "a stage has no valid name".to_owned())
}

/// Use the same stage classification for the named refusal and the body.
fn engram_verification_record_classify_stage(
    run_dir: &FsPath,
    root: &FsPath,
    stage: &Value,
    planned: &HashMap<String, Vec<String>>,
    mode: EngramVerificationRecordMode,
    test_stages: &[String],
    admit_full_lint: bool,
) -> Result<
    (
        String,
        Vec<String>,
        Option<EngramVerificationRecordCheckKind>,
    ),
    String,
> {
    let name = engram_verification_record_stage_name(stage)?;
    let unrun = stage.get("state").and_then(Value::as_str) == Some("unrun")
        && ["command", "started", "ended", "code"]
            .iter()
            .all(|key| stage.get(*key).is_none());
    let argv =
        if admit_full_lint && engram_verification_record_full_lint_stage(mode, name) && !unrun {
            // Missing or unusable observed argv must yield a named refusal, even
            // if the request planned an exact lint or a real test/build command.
            test_run_argv(stage.get("command"))
                .filter(|argv| {
                    argv.first()
                        .is_some_and(|program| !program.trim().is_empty())
                })
                .unwrap_or_default()
        } else {
            engram_verification_record_stage_argv(stage, planned)?.1
        };
    if mode == EngramVerificationRecordMode::Focused
        && name == "focused"
        && let Some(plan) = planned
            .get(name)
            .filter(|plan| engram_is_focused_dart_sdk_test(plan))
    {
        if !engram_supports_focused_dart_test(plan) {
            return Ok((name.to_owned(), argv, None));
        }
        let command = if unrun {
            Some(plan.clone())
        } else {
            engram_focused_dart_check_argv(run_dir, root, stage, plan)
        };
        return Ok(match command {
            Some(command) => (
                name.to_owned(),
                command,
                Some(EngramVerificationRecordCheckKind::Test),
            ),
            None => (name.to_owned(), argv, None),
        });
    }
    let kind =
        engram_verification_record_classify_argv(mode, name, &argv, test_stages, admit_full_lint);
    Ok((name.to_owned(), argv, kind))
}

/// The stages of a run that are neither a test, build nor admitted lint check
/// (`engram_verification_record_classify_argv`), in results order.
pub(crate) fn engram_verification_record_unclassified_stages(
    run_dir: &FsPath,
    request: &Value,
    results: &Value,
) -> Vec<String> {
    let mode = engram_verification_record_mode(request);
    let planned = engram_verification_record_planned_argv(request);
    let test_stages = engram_launcher_test_stages(request);
    let root = FsPath::new(
        request
            .get("root")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    results
        .get("stages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|stage| {
            match engram_verification_record_classify_stage(
                run_dir,
                root,
                stage,
                &planned,
                mode,
                &test_stages,
                true,
            ) {
                Ok((name, _, None)) => Some(name),
                _ => None,
            }
        })
        .collect()
}

pub(crate) fn engram_verification_record_registration_unclassified_stages(
    registration: &TestRunRegistration,
    request: &Value,
    results: &Value,
) -> Vec<String> {
    let mut stages = engram_verification_record_unclassified_stages(
        FsPath::new(&registration.run_directory),
        request,
        results,
    );
    if registration.declared_native.is_some() && registration.declared_terminal.is_some() {
        // The body builder separately checks the actual command against this
        // captured binding. Merely having a focused stage is insufficient.
        stages.retain(|stage| stage != "focused");
    }
    stages
}

/// The argv each stage was planned with in the request (`command` and
/// `args`), by stage name: what an unrun stage would have run.
pub(crate) fn engram_verification_record_planned_argv(
    request: &Value,
) -> HashMap<String, Vec<String>> {
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
    } else if request
        .get("liveEngram")
        .is_some_and(|live| !live.is_null())
    {
        EngramVerificationRecordMode::Live
    } else {
        EngramVerificationRecordMode::Focused
    }
}

/// The launcher's own recorded command (`command`, an argv, in results.json
/// or else request.json), when either records one. Today's launcher records
/// none, so `engram_verification_record_run_command` reconstructs it.
fn engram_verification_record_recorded_command(
    request: &Value,
    results: &Value,
) -> Option<Vec<String>> {
    if let Some(command) = request
        .get("nativeExecution")
        .and_then(Value::as_str)
        .and_then(|_| test_run_argv(request.get("observedCommand")))
        .filter(|argv| !argv.is_empty())
    {
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
    let mut command: Vec<String> = [
        "node",
        "scripts/test-launcher.mjs",
        mode.word(),
        "--input",
        "commit",
    ]
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
    run_dir: &FsPath,
    root: &FsPath,
    stage: &Value,
    planned: &HashMap<String, Vec<String>>,
    mode: EngramVerificationRecordMode,
    test_stages: &[String],
    admit_full_lint: bool,
) -> Result<EngramVerificationRecordCheck, String> {
    let (name, argv, kind) = engram_verification_record_classify_stage(
        run_dir,
        root,
        stage,
        planned,
        mode,
        test_stages,
        admit_full_lint,
    )?;
    let outcome = match test_run_stage_state(stage.get("state").and_then(Value::as_str)) {
        TestRunStageState::Passed => EngramVerificationRecordCheckOutcome::Passed,
        TestRunStageState::Failed => EngramVerificationRecordCheckOutcome::Failed,
        TestRunStageState::Unrun => EngramVerificationRecordCheckOutcome::NotRun,
        TestRunStageState::Running => EngramVerificationRecordCheckOutcome::Indeterminate,
    };
    let Some(kind) = kind else {
        return Err(format!(
            "stage {name} is neither a test, build nor admitted lint check"
        ));
    };
    let dart = mode == EngramVerificationRecordMode::Focused
        && name == "focused"
        && engram_supports_focused_dart_test(&argv);
    let command = engram_verification_record_command_text(&argv);
    let exit = stage.get("code").and_then(Value::as_i64);
    let mut refs = Vec::new();
    let command_ref = format!("command:{command}");
    if command_ref.len() <= ENGRAM_CHECK_REF_MAX_BYTES {
        refs.push(command_ref);
    }
    if let Some(code) = exit {
        refs.push(format!("exit:{code}"));
    }
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
    let mut check = EngramVerificationRecordCheck {
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
    };
    if dart {
        engram_focused_dart_check_result(run_dir, stage, &mut check);
    }
    Ok(check)
}

pub(crate) fn engram_verification_record_declared_check(
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
    let safe = |text: &str| {
        text == text.trim() && !text.chars().any(|c| {
        c.is_control() || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    })
    };
    let summary = engram_declared_summary(&command, Some(binding), exit, &result);
    if !safe(&command.normalized)
        || summary.len() > ENGRAM_CHECK_SUMMARY_MAX_BYTES
        || !safe(&summary)
        || refs.len() > 64
        || refs
            .iter()
            .any(|value| value.len() > ENGRAM_CHECK_REF_MAX_BYTES || !safe(value))
    {
        return Err("declared provenance is outside the verification text contract".to_owned());
    }
    let counts = result.counters.as_ref().map(|counters| {
        counters
            .values
            .iter()
            .cloned()
            .collect::<BTreeMap<String, u64>>()
    });
    if counts.as_ref().is_some_and(|counts| {
        counts.len() > 32 || counts.keys().any(|key| key.len() > 128 || !safe(key))
    }) {
        return Err("declared counters exceed the verification contract".to_owned());
    }
    Ok(EngramVerificationRecordCheck {
        stage: name,
        check_kind: EngramVerificationRecordCheckKind::Test,
        command: command.normalized.clone(),
        check_fingerprint: EngramHost::check_fingerprint(&command),
        started_at: test_run_str(stage, "started"),
        completed_at: test_run_str(stage, "ended"),
        outcome,
        summary: Some(summary),
        refs,
        counts,
    })
}

// Producer-only terminal admission. Generic launcher disk parsing is unchanged.
pub(crate) fn engram_verification_record_results_are_terminal(
    registration: &TestRunRegistration,
    results: &Value,
) -> bool {
    let Some(start) = &registration.declared_native else {
        return test_run_results_are_terminal(results);
    };
    let Some(terminal) = &registration.declared_terminal else {
        return false;
    };
    // A native artifact verdict may be unknown or failed despite exit 0 or
    // Claude ReportedSuccess. Only this host-owned terminal and exact digest
    // admit it; generic launchers keep their existing numeric terminal rule.
    let state = declared_run_result_state(&terminal.check);
    let exit = results.get("exitCode");
    if registration.registered_at != start.registered_at
        || registration.request_identity != start.request_identity
        || !start.accepts_registration_binding(registration.work_binding.as_ref())
        || registration.named_root.as_ref() != Some(&start.named)
        || registration.authority_store.as_ref() != Some(&start.store)
        || results.get("runId").and_then(Value::as_str) != Some(start.run_id.as_str())
        || results.get("state").and_then(Value::as_str) != Some(state)
        || !results
            .get("ended")
            .and_then(Value::as_str)
            .is_some_and(|ended| EngramHost::check_timestamp(ended).is_some())
    {
        return false;
    }
    let Some(stages) = results.get("stages").and_then(Value::as_array) else {
        return false;
    };
    let [stage] = stages.as_slice() else {
        return false;
    };
    stage.get("name").and_then(Value::as_str) == Some("focused")
        && stage.get("state").and_then(Value::as_str) == Some(state)
        && stage.get("reportedSuccess").and_then(Value::as_bool) == Some(terminal.reported_success)
        && stage.get("code") == exit
        && test_run_str(stage, "ended") == terminal.check.completed_at
        && serde_json::to_vec_pretty(results)
            .ok()
            .is_some_and(|bytes| file_content_hash(&bytes) == terminal.results_digest)
}

/// The record of a bound committed-input run, from its registration and its
/// own request and terminal results; why not, when the run cannot emit one.
pub(crate) fn engram_verification_record_body(
    registration: &TestRunRegistration,
    request: &Value,
    results: &Value,
) -> Result<EngramVerificationRecordBody, String> {
    engram_verification_record_build_body(registration, request, results, None)
}

// A retained body is checked in its original format, never converted during
// replay. Only that historical reader can reconstruct a commit projection
// from source-bearing artifacts; a fresh record keeps the complete identity.
pub(crate) fn engram_verification_record_build_body(
    registration: &TestRunRegistration,
    request: &Value,
    results: &Value,
    retained_input: Option<&EngramVerificationRecordInput>,
) -> Result<EngramVerificationRecordBody, String> {
    let body = engram_verification_record_build_with_stage_policy(
        registration,
        request,
        results,
        retained_input,
        true,
    );
    let Some(digest) = registration
        .verification_record
        .as_ref()
        .and_then(TestRunVerificationAck::body_digest)
        .filter(|digest| !digest.is_empty())
    else {
        return body;
    };
    if retained_input.is_none()
        || engram_verification_record_mode(request) != EngramVerificationRecordMode::Full
        || body
            .as_ref()
            .is_ok_and(|body| engram_verification_record_digest_answers(digest, body))
    {
        return body;
    }
    // The run directory is writable by the launcher. An old body can use the
    // old classification only when reconstruction matches the digest already
    // recorded by the host for an acknowledged receipt.
    let historical = engram_verification_record_build_with_stage_policy(
        registration,
        request,
        results,
        retained_input,
        false,
    );
    if historical
        .as_ref()
        .is_ok_and(|body| engram_verification_record_digest_answers(digest, body))
    {
        historical
    } else {
        body
    }
}

pub(crate) fn engram_verification_record_build_with_stage_policy(
    registration: &TestRunRegistration,
    request: &Value,
    results: &Value,
    retained_input: Option<&EngramVerificationRecordInput>,
    admit_full_lint: bool,
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
    let root = FsPath::new(
        request
            .get("root")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let mut checks = Vec::new();
    for stage in results
        .get("stages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let check = engram_verification_record_check(
            FsPath::new(&registration.run_directory),
            root,
            stage,
            &planned,
            mode,
            &test_stages,
            admit_full_lint,
        )?;
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
    let historical = retained_input
        .is_some_and(|input| matches!(input, EngramVerificationRecordInput::Commit { .. }));
    let has_source = request.get("sourceId").is_some()
        || results.get("sourceIdBefore").is_some()
        || results.get("sourceIdAfter").is_some();
    let endpoints = if has_source {
        let admitted = test_run_source_id(request.get("sourceId"))
            .ok_or("the admitted owner source_id is unavailable")?;
        let (before, after) = test_run_source_endpoints(results)
            .ok_or("the owner source_id endpoints are unavailable")?;
        match (admitted.compare(&before), before.compare(&after)) {
            (None, _) | (_, None) => {
                return Err(
                    "the owner source_id formats across the run boundaries are incomparable"
                        .to_owned(),
                );
            }
            (Some(true), Some(true)) if before.commit == commit => {}
            _ => return Err("the owner source_id differs across the run boundaries".to_owned()),
        }
        if !before.is_clean() {
            return Err("the owner source_id contradicts the clean committed input".to_owned());
        }
        Some((before, after))
    } else {
        None
    };
    let (record_input, post_run) = if !historical {
        // SourceId carries no legacy classification fields. Retain the
        // ordinary committed-input policy even when rebuilding a held receipt.
        if outcome != TestRunCommittedInputOutcome::Clean || !after.clean || !after.paths.is_empty()
        {
            return Err("the run's post-run committed input is not clean".to_owned());
        }
        let (before, after) = endpoints.ok_or("the run records no complete owner source_id")?;
        (
            EngramVerificationRecordInput::SourceId { source_id: before },
            EngramVerificationRecordPostRun::SourceId(EngramVerificationRecordSourcePostRun {
                source_id: after,
            }),
        )
    } else {
        // A retained commit body keeps its historical shape, without
        // inventing equivalence with a new SourceId or rewriting its bytes.
        (
            EngramVerificationRecordInput::Commit { commit },
            EngramVerificationRecordPostRun::Commit(EngramVerificationRecordCommitPostRun {
                outcome,
                clean_before: input.clean_before,
                clean_after: after.clean,
                dirty_paths: after.paths.iter().map(|path| path.path.clone()).collect(),
                disposition: None,
            }),
        )
    };
    let body = engram_verification_record_envelope(
        registration,
        request,
        results,
        record_input,
        post_run,
        checks,
    )?;
    engram_verification_record_frame_bound(
        &body,
        &registration.run_id,
        "verification_frame_too_large",
    )?;
    Ok(body)
}

fn engram_verification_record_native_body(
    registration: &TestRunRegistration,
    request: &Value,
    results: &Value,
    start: &DeclaredRunReservation,
) -> Result<EngramVerificationRecordBody, String> {
    let terminal = registration
        .declared_terminal
        .as_ref()
        .ok_or("the host has not confirmed the native terminal")?;
    let after = terminal.source.as_ref().ok_or_else(|| {
        format!(
            "native source_id after the run is unavailable: {}",
            terminal
                .source_refusal
                .as_deref()
                .unwrap_or("owner measurement unavailable")
        )
    })?;
    let digest =
        serde_json::to_vec_pretty(results).map_err(|_| "native results cannot be encoded")?;
    let input = EngramVerificationRecordInput::SourceId {
        source_id: start.source.source_id.clone(),
    };
    let post_run =
        EngramVerificationRecordPostRun::SourceId(EngramVerificationRecordSourcePostRun {
            source_id: after.source_id.clone(),
        });
    if terminal.results_digest != file_content_hash(&digest)
        || registration.registered_at != start.registered_at
        || !start.accepts_registration_binding(registration.work_binding.as_ref())
        || registration.named_root.as_ref() != Some(&start.named)
        || registration.authority_store.as_ref() != Some(&start.store)
        || registration.source_owner.as_ref() != Some(&start.source.source_owner)
        || registration.request_identity != start.request_identity
        || test_run_registration_identity(request).ok().as_ref() != Some(&start.request_identity)
        || request.get("input").and_then(Value::as_str) != Some("source_id")
        || request.get("nativeSourceId") != Some(&json!(start.source.source_id))
        || after.root != start.source.root
        || after.source_owner != start.source.source_owner
        || results.get("input")
            != Some(&serde_json::to_value(&input).map_err(|error| error.to_string())?)
        || results.get("post_run")
            != Some(&serde_json::to_value(&post_run).map_err(|error| error.to_string())?)
    {
        return Err("native terminal does not match its host reservation".to_owned());
    }
    match start.source.source_id.compare(&after.source_id) {
        Some(true) => {}
        Some(false) => {
            return Err(engram_source_id_change_detail(
                &engram_source_id_differences(&start.source.source_id, &after.source_id),
            ));
        }
        None => return Err("native source_id formats are incomparable".to_owned()),
    }
    if results.get("nativeEligible").and_then(Value::as_bool) != Some(true) {
        return Err("native run is ineligible under its host authority checks".to_owned());
    }
    if !engram_verification_record_results_are_terminal(registration, results) {
        return Err("the native run has no terminal result".to_owned());
    }
    if results
        .get("stages")
        .and_then(Value::as_array)
        .is_none_or(|stages| {
            stages.len() != 1 || stages[0].get("name").and_then(Value::as_str) != Some("focused")
        })
    {
        return Err("the native run does not have its single original stage".to_owned());
    }
    let body = engram_verification_record_envelope(
        registration,
        request,
        results,
        input,
        post_run,
        vec![terminal.check.clone()],
    )?;
    // Reuse the control transport's operation-specific frame bound. Keep the full
    // identity and a named refusal instead of sending a shortened record or
    // letting a permanent oversize frame become a transport retry.
    engram_verification_record_frame_bound(
        &body,
        &registration.run_id,
        "native_verification_frame_too_large",
    )?;
    Ok(body)
}

pub(crate) fn engram_verification_record_frame_bound(
    body: &EngramVerificationRecordBody,
    run_id: &str,
    code: &str,
) -> Result<(), String> {
    let frame = EngramControlRequest::VerificationRecord {
        operation_key: engram_verification_record_key(run_id),
        record: serde_json::to_value(body).map_err(|_| "the record cannot be encoded")?,
    };
    let bytes = serde_json::to_vec(&frame).map_err(|_| "the record frame cannot be encoded")?;
    let framed_len = bytes.len().saturating_add(1); // The transport writes one LF.
    let limit = engram_control_request_frame_limit(&frame);
    if framed_len > limit {
        return Err(format!(
            "{code}: {framed_len} bytes exceeds the {limit} byte verification frame"
        ));
    }
    Ok(())
}

fn engram_verification_record_envelope(
    registration: &TestRunRegistration,
    request: &Value,
    results: &Value,
    input: EngramVerificationRecordInput,
    post_run: EngramVerificationRecordPostRun,
    checks: Vec<EngramVerificationRecordCheck>,
) -> Result<EngramVerificationRecordBody, String> {
    let binding = registration
        .work_binding
        .clone()
        .ok_or("the run is not bound to a claim")?;
    let root = registration
        .named_root
        .as_ref()
        .ok_or("the run names no root")?;
    let mode = engram_verification_record_mode(request);
    let run_outcome = if results.get("interrupted").and_then(Value::as_bool) == Some(true)
        || checks
            .iter()
            .any(|check| check.outcome == EngramVerificationRecordCheckOutcome::Indeterminate)
    {
        EngramVerificationRecordRunOutcome::Indeterminate
    } else if results.get("state").and_then(Value::as_str) == Some("passed")
        && checks
            .iter()
            .all(|check| check.outcome == EngramVerificationRecordCheckOutcome::Passed)
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
pub(crate) fn engram_verification_record_text(
    body: &EngramVerificationRecordBody,
) -> Result<String, String> {
    serde_json::to_value(body)
        .and_then(|value| serde_json::to_string(&value))
        .map_err(|error| format!("the record does not serialize: {error}"))
}

/// The frozen record in `text`, only when it reads back to exactly `text`.
pub(crate) fn engram_verification_record_parse(
    text: &str,
) -> Option<(Value, EngramVerificationRecordBody)> {
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
pub(crate) fn engram_verification_record_request(
    frozen: &EngramVerificationRecordFrozen,
) -> EngramControlRequest {
    EngramControlRequest::VerificationRecord {
        operation_key: engram_verification_record_key(&frozen.run_id),
        record: frozen.record.clone(),
    }
}

/// The stages the producer mints a verification for: those with a completion
/// time whose outcome is not `not_run`.
pub(crate) fn engram_verification_record_emitted_stages(
    body: &EngramVerificationRecordBody,
) -> Vec<&str> {
    body.checks
        .iter()
        .filter(|check| {
            check.completed_at.is_some()
                && check.outcome != EngramVerificationRecordCheckOutcome::NotRun
        })
        .map(|check| check.stage.as_str())
        .collect()
}

/// Whether `receipt` answers exactly `body`: its operation is the producer
/// object it names, a well-formed record id; one well-formed, distinct record
/// for each emitted stage (`engram_verification_record_emitted_stages`) and
/// no other; and the run's own outcome.
pub(crate) fn engram_verification_record_receipt_check(
    body: &EngramVerificationRecordBody,
    receipt: &EngramVerificationRecordReceipt,
) -> Result<(), &'static str> {
    if !is_engram_record_id(&receipt.producer) || receipt.operation != receipt.producer {
        return Err("the receipt does not name its producer as its operation");
    }
    if receipt.run_outcome != body.run.outcome {
        return Err("the receipt states another run outcome");
    }
    let emitted: HashSet<&str> = engram_verification_record_emitted_stages(body)
        .into_iter()
        .collect();
    if receipt.verifications.len() != emitted.len() {
        return Err("the receipt does not name one record per emitted check");
    }
    let mut stages = HashSet::new();
    let mut records = HashSet::new();
    for verification in &receipt.verifications {
        if !emitted.contains(verification.stage.as_str())
            || !stages.insert(verification.stage.as_str())
        {
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
pub(crate) fn engram_verification_record_producer_refusal(
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
        .map(
            |stage| EngramVerificationRecordPending::StartedBeforeRegistration {
                stage: test_run_str(stage, "name"),
            },
        )
}

/// Writes `bytes` at `path` through a synced temporary file and a rename.
pub(crate) fn engram_verification_record_write(path: &FsPath, bytes: &[u8]) -> io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    let mut file = fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)
}

pub(crate) fn engram_verification_record_progress(
    run_dir: &FsPath,
) -> Option<EngramVerificationRecordProgress> {
    let text = fs::read_to_string(run_dir.join(ENGRAM_VERIFICATION_RECORD_STATE_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Records `progress` beside the run when it differs from what is there, so
/// an unchanged pending reason is neither rewritten nor logged again.
/// Writes `progress` to the state file beside the run, a copy the launcher
/// summary reads; whether the file holds it now. A failure is logged.
pub(crate) fn engram_verification_record_set_progress(
    run_dir: &FsPath,
    run_id: &str,
    progress: EngramVerificationRecordProgress,
) -> bool {
    match engram_verification_record_put_progress(run_dir, &progress) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("engram> run={run_id} verification record state not written: {error}");
            false
        }
    }
}

/// `engram_verification_record_set_progress` without its log line.
fn engram_verification_record_put_progress(
    run_dir: &FsPath,
    progress: &EngramVerificationRecordProgress,
) -> io::Result<()> {
    if engram_verification_record_progress(run_dir).as_ref() == Some(progress) {
        return Ok(());
    }
    let bytes = serde_json::to_vec(progress).map_err(io::Error::other)?;
    engram_verification_record_write(&run_dir.join(ENGRAM_VERIFICATION_RECORD_STATE_FILE), &bytes)
}

/// The frozen body beside `run_dir`, read back exactly; `None` when it is
/// missing or does not read back to its own bytes.
pub(crate) fn engram_verification_record_frozen(
    run_dir: &FsPath,
    run_id: &str,
) -> Option<EngramVerificationRecordFrozen> {
    let text = fs::read_to_string(run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE)).ok()?;
    engram_verification_record_frozen_text(run_dir, run_id, &text)
}

fn engram_verification_record_frozen_text(
    run_dir: &FsPath,
    run_id: &str,
    text: &str,
) -> Option<EngramVerificationRecordFrozen> {
    let (record, body) = engram_verification_record_parse(text)?;
    Some(EngramVerificationRecordFrozen {
        run_dir: run_dir.to_path_buf(),
        run_id: run_id.to_owned(),
        record,
        body,
    })
}

/// The frozen body's text beside the run: `Ok(None)` when there is none,
/// `Ok(Some(None))` when its bytes were read but are not UTF-8 (a body that
/// does not parse), `Err(())` when it cannot be read just now (a read to try
/// again later, never a refusal of the body).
fn engram_verification_record_body_text(run_dir: &FsPath) -> Result<Option<Option<String>>, ()> {
    match fs::read(run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE)) {
        Ok(bytes) => Ok(Some(String::from_utf8(bytes).ok())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

/// Whether `body` is exactly the record the host builds again from its own
/// `registration` and the run's request and results beside it: the check a
/// frozen body read back from the run directory must pass before anything is
/// sent, settled or admitted from it.
pub(crate) fn engram_verification_record_is_host_body(
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

/// A run's Engram verification record as the host itself holds it, for the
/// exact run read (`GET /api/test-runs/{id}`, `detail.verificationRecord`):
/// the host's own ack, whatever the state file beside the run says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TestRunVerificationRecordView {
    /// `settled`, `received` (Engram answered; not yet settled here),
    /// `rejected` (refused by Engram) or `refusedByHost` (before sending).
    pub(crate) state: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) record_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) run_outcome: Option<EngramVerificationRecordRunOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

/// `run_id`'s registration, live or else retained with its compacted history.
pub(crate) fn test_run_registration_live_or_retained<'a>(
    inner: &'a StateInner,
    run_id: &str,
) -> Option<&'a TestRunRegistration> {
    inner.test_run_registrations.get(run_id).or_else(|| {
        inner
            .test_run_compacted
            .get(run_id)
            .and_then(|row| row.registration.as_ref())
    })
}

fn test_run_verification_record_view(
    ack: &TestRunVerificationAck,
) -> TestRunVerificationRecordView {
    let receipt_view =
        |state, receipt: &EngramVerificationRecordReceipt| TestRunVerificationRecordView {
            state,
            record_ids: receipt
                .verifications
                .iter()
                .map(|record| record.record_id.clone())
                .collect(),
            run_outcome: Some(receipt.run_outcome),
            detail: None,
        };
    let refusal_view = |state, detail: &String| TestRunVerificationRecordView {
        state,
        record_ids: Vec::new(),
        run_outcome: None,
        detail: Some(detail.clone()),
    };
    match ack {
        TestRunVerificationAck::Settled { receipt, .. } => receipt_view("settled", receipt),
        TestRunVerificationAck::Received { receipt, .. } => receipt_view("received", receipt),
        TestRunVerificationAck::Rejected { detail, .. } => refusal_view("rejected", detail),
        TestRunVerificationAck::LocalUnclassified { detail } => {
            refusal_view("refusedByHost", detail)
        }
    }
}

/// The host's own ack of `run`'s record, live or retained, for the exact
/// run's detail; none while nothing was received or refused.
/// `selected_request` is the request identity of the artifact the detail was
/// selected for: only that artifact's own registration lends it a record,
/// never a registration under a reused run id for another request.
pub(crate) fn test_run_verification_record_for_detail(
    inner: &StateInner,
    run: &TestRunSummary,
    selected_request: Option<&str>,
) -> Option<TestRunVerificationRecordView> {
    let registration = test_run_registration_live_or_retained(inner, &run.run_id)?;
    if selected_request != Some(registration.request_identity.as_str()) {
        return None;
    }
    registration
        .verification_record
        .as_ref()
        .map(test_run_verification_record_view)
}

/// The authoritative read of a run's record
/// (`GET /api/test-runs/{id}?view=verification`): the host's own ack from
/// the run's live or retained registration, with no read of the run's files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TestRunVerificationRead {
    pub(crate) run_id: String,
    /// Null while the host holds no answer for the run.
    pub(crate) verification_record: Option<TestRunVerificationRecordView>,
}

pub(crate) fn test_run_verification_read(
    inner: &StateInner,
    run_id: &str,
) -> Result<TestRunVerificationRead, ApiError> {
    let registration = test_run_registration_live_or_retained(inner, run_id)
        .ok_or_else(|| ApiError::not_found("unknown test run"))?;
    Ok(TestRunVerificationRead {
        run_id: registration.run_id.clone(),
        verification_record: registration
            .verification_record
            .as_ref()
            .map(test_run_verification_record_view),
    })
}

/// Keeps `look` as the run's latest look, under the state lock.
fn store_engram_verification_record_look_locked(
    inner: &mut StateInner,
    run_id: &str,
    look: EngramVerificationRecordLook,
) {
    inner
        .test_run_record_pending
        .insert(run_id.to_owned(), look);
    // A held receipt found unassociated belongs to the look that found it:
    // only this look's own completion, which follows it, may find it again.
    inner.test_run_unassociated_receipts.remove(run_id);
}

/// What the producer's latest look at a run's record came to, kept in memory
/// (`StateInner::test_run_record_pending`) for a run wait to decide from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EngramVerificationRecordLook {
    /// The look left the record pending, for this reason.
    Pending(EngramVerificationRecordPending),
    /// The look found nothing holding the record back: it is being sent or
    /// settled, or read again later.
    NoBlocker,
    /// The run's own request or results, as this look read them, make no
    /// record (`EngramVerificationRecordInputs::make_no_record_of_a_finished_run`).
    /// For a finished run that means its run directory was removed or its
    /// files rewritten; a look that began before the run was known to have
    /// finished may only have read it mid-publication. `begun` is the look's
    /// place in `StateInner::test_run_look_sequence`, taken before it read
    /// any file, so that a run wait counts only a look begun after it marked
    /// the run (test_run_wait_records.rs).
    NoRecord { begun: u64 },
}

/// What a bound run's own request and terminal results, which its record is
/// built from, come to on one read. Reads only; writes nothing.
pub(crate) enum EngramVerificationRecordInputs {
    /// Both read: the request and the terminal results.
    Read(Value, Value),
    /// The results are not terminal yet: read again later.
    Waiting,
    /// The request or results could not be read just now (a failed read, or
    /// launcher results that do not parse): read again later. Unlike
    /// `Waiting`, this read decided nothing about the run.
    Unreadable,
    /// No results file beside a readable request: a run still going, which
    /// the producer looks at again later. Of a run already known to have
    /// finished, the results are gone.
    ResultsMissing,
    /// Files that were read, or are gone, from which no record is built: an
    /// unbound run, a missing or malformed request, a request that is not
    /// committed input or not the registered one, or results that hold no
    /// committed-input record.
    NotEligible,
    /// A declared native run's results over the run JSON bound.
    NativeResultsTooLarge,
    /// A launcher run's results over the run JSON bound, which are not read:
    /// the producer looks again later. Of a run already known to have
    /// finished, its results were rewritten past what a record is built
    /// from.
    ResultsTooLarge,
    /// A declared native run whose host-written terminal is recorded, but
    /// whose results now read as something else: the producer looks again
    /// later. Of a run already known to have finished, they were rewritten
    /// after the host wrote them, which no later read undoes; a look begun
    /// earlier may have read them while the host was still publishing them.
    NativeResultsChanged,
}

impl EngramVerificationRecordInputs {
    /// Whether these inputs, read for a run already known to have finished,
    /// can no longer make a record: its files were removed or rewritten.
    /// What the producer's look keeps for a run wait
    /// (`EngramVerificationRecordLook::NoRecord`).
    pub(crate) fn make_no_record_of_a_finished_run(&self) -> bool {
        matches!(
            self,
            Self::NotEligible
                | Self::ResultsMissing
                | Self::ResultsTooLarge
                | Self::NativeResultsChanged
        )
    }
}

pub(crate) fn engram_verification_record_inputs(
    registration: &TestRunRegistration,
) -> EngramVerificationRecordInputs {
    use EngramVerificationRecordInputs as Inputs;
    if !engram_verification_record_registration_bound(registration) {
        return Inputs::NotEligible;
    }
    let run_dir = PathBuf::from(&registration.run_directory);
    let request = match test_run_read_json(&run_dir.join("request.json")) {
        TestRunJson::Parsed(request, _, _) => request,
        // A read that failed just now (a version race or a filesystem
        // error) is read again on a later tick; it is never a refusal.
        TestRunJson::Failed => return Inputs::Unreadable,
        _ => return Inputs::NotEligible,
    };
    if !test_run_request_is_committed_input(&request) && registration.declared_native.is_none() {
        return Inputs::NotEligible;
    }
    if test_run_registration_identity(&request).ok().as_ref()
        != Some(&registration.request_identity)
    {
        return Inputs::NotEligible;
    }
    let results = match test_run_read_json(&run_dir.join("results.json")) {
        TestRunJson::Parsed(results, _, _)
            if engram_verification_record_results_are_terminal(registration, &results) =>
        {
            results
        }
        TestRunJson::TooLarge if registration.declared_native.is_some() => {
            return Inputs::NativeResultsTooLarge;
        }
        // Read whole, whether it parses or not, but not the terminal the host
        // wrote and recorded: rewritten since.
        TestRunJson::Parsed(..) | TestRunJson::Malformed(_)
            if registration.declared_native.is_some()
                && registration.declared_terminal.is_some() =>
        {
            return Inputs::NativeResultsChanged;
        }
        TestRunJson::TooLarge => return Inputs::ResultsTooLarge,
        TestRunJson::Missing => return Inputs::ResultsMissing,
        TestRunJson::Failed | TestRunJson::Malformed(_) => return Inputs::Unreadable,
        _ => return Inputs::Waiting,
    };
    // A run whose results hold no committed-input record never emits; only
    // a record that cannot be built from a present one is held pending.
    if registration.declared_native.is_none() && TestRunCommittedInput::read(&results).is_none() {
        return Inputs::NotEligible;
    }
    Inputs::Read(request, results)
}

impl AppState {
    /// Authenticate the same results used to rebuild the body. Older
    /// rejections lack a body digest and need the original launch
    /// observation's terminal digest as independent host evidence.
    pub(crate) fn engram_rejected_verification_body_matches(
        &self,
        registration: &TestRunRegistration,
        body: &EngramVerificationRecordBody,
    ) -> bool {
        let Some(TestRunVerificationAck::Rejected {
            producer_body_digest,
            ..
        }) = &registration.verification_record
        else {
            return false;
        };
        let run_dir = FsPath::new(&registration.run_directory);
        let (TestRunJson::Parsed(request, _, _), TestRunJson::Parsed(results, digest, _)) = (
            test_run_read_json(&run_dir.join("request.json")),
            test_run_read_json(&run_dir.join("results.json")),
        ) else {
            return false;
        };
        if test_run_registration_identity(&request).ok().as_ref()
            != Some(&registration.request_identity)
            || !engram_verification_record_build_body(
                registration,
                &request,
                &results,
                Some(&body.input),
            )
            .is_ok_and(|rebuilt| rebuilt == *body)
        {
            return false;
        }
        if let Some(expected) = producer_body_digest {
            return engram_verification_record_digest_answers(expected, body);
        }
        let inner = self.inner.lock().expect("state mutex poisoned");
        let live = inner
            .test_run_observations
            .index()
            .artifact_run(&registration.run_id)
            .filter_map(|key| inner.test_run_observations.get(key));
        let compacted = inner
            .test_run_compacted
            .get(&registration.run_id)
            .filter(|row| row.compacted && test_run_compact_is_installed(&*inner, row))
            .into_iter()
            .flat_map(|row| &row.observations);
        let mut terminals = live
            .chain(compacted)
            .filter(|row| engram_verification_record_row_matches(row, registration))
            .filter_map(|row| {
                let digest = row.artifact.as_ref()?.terminal_digest.as_deref()?;
                Some((row.summary.state, digest))
            });
        let matches = |(state, retained): (TestRunState, &str)| {
            state == TestRunState::Passed
                && test_run_terminal_digest_matches(Some(&digest), retained)
        };
        terminals.next().is_some_and(matches) && terminals.all(matches)
    }
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
/// file beside the run: a finished run's producer body is never rebuilt, and a
/// file that claims a receipt, a settlement or a rejection the host never
/// observed neither holds nor stops the send.
pub(crate) fn engram_verification_record_prepare(
    registration: &TestRunRegistration,
) -> EngramVerificationRecordPrepared {
    engram_verification_record_prepare_noting(registration, &std::cell::Cell::new(false))
}

/// `engram_verification_record_prepare`, setting `no_record` to whether the
/// run's own request and results, as the last read this decision made of them
/// found them, make no record
/// (`EngramVerificationRecordInputs::make_no_record_of_a_finished_run`); left
/// as it is when the decision read neither.
fn engram_verification_record_prepare_noting(
    registration: &TestRunRegistration,
    no_record: &std::cell::Cell<bool>,
) -> EngramVerificationRecordPrepared {
    use EngramVerificationRecordPrepared as Prepared;
    if !engram_verification_record_registration_bound(registration) {
        return Prepared::NotEligible;
    }
    let run_dir = PathBuf::from(&registration.run_directory);
    let pending = |reason: EngramVerificationRecordPending| {
        engram_verification_record_set_progress(
            &run_dir,
            &registration.run_id,
            EngramVerificationRecordProgress::Pending {
                reason: reason.clone(),
            },
        );
        Prepared::Pending(reason)
    };
    let held = match &registration.verification_record {
        Some(
            TestRunVerificationAck::Settled { .. }
            | TestRunVerificationAck::Rejected { .. }
            | TestRunVerificationAck::LocalUnclassified { .. },
        ) => {
            return Prepared::Finished;
        }
        Some(TestRunVerificationAck::Received {
            receipt,
            body_digest,
        }) => Some((receipt.clone(), body_digest.clone())),
        None => None,
    };
    // The run's own request and terminal results, as the record is built
    // from; `Err` is what the look comes to without them.
    let inputs = || -> std::result::Result<(Value, Value), Prepared> {
        let read = engram_verification_record_inputs(registration);
        no_record.set(read.make_no_record_of_a_finished_run());
        match read {
            EngramVerificationRecordInputs::Read(request, results) => Ok((request, results)),
            EngramVerificationRecordInputs::Waiting
            | EngramVerificationRecordInputs::Unreadable
            | EngramVerificationRecordInputs::ResultsMissing
            | EngramVerificationRecordInputs::ResultsTooLarge
            | EngramVerificationRecordInputs::NativeResultsChanged => Err(Prepared::Waiting),
            EngramVerificationRecordInputs::NotEligible => Err(Prepared::NotEligible),
            EngramVerificationRecordInputs::NativeResultsTooLarge => {
                Err(pending(EngramVerificationRecordPending::BodyNotBuilt {
                    detail: format!(
                        "native_results_too_large: exceeds the existing {TEST_RUN_JSON_MAX_BYTES} byte run JSON bound"
                    ),
                }))
            }
        }
    };
    // Answered: settled from the receipt, never built or sent again, once
    // the frozen body is the host's own record and the receipt answers it.
    // A body that no longer reads back, or is not the host's, keeps the
    // receipt as it is; a receipt that does not answer it is not settled
    // from, and the same body goes again under the same key. A body that is
    // the host's record of rewritten run files, but not the body the receipt
    // answered, is never settled from that receipt.
    if let Some((receipt, body_digest)) = held {
        // A body that cannot be read just now is read again later; one that
        // is gone or does not parse is unreadable.
        let Ok(text) = engram_verification_record_body_text(&run_dir) else {
            return Prepared::Waiting;
        };
        let Some(frozen) = text.flatten().and_then(|text| {
            engram_verification_record_frozen_text(&run_dir, &registration.run_id, &text)
        }) else {
            return pending(EngramVerificationRecordPending::BodyUnreadable);
        };
        let rebuilt = match inputs() {
            Ok((request, results)) => engram_verification_record_build_body(
                registration,
                &request,
                &results,
                Some(&frozen.body.input),
            )
            .ok(),
            // Results that cannot be read just now are read again on a later
            // tick; only readable inputs can show the body is not the host's.
            Err(Prepared::Waiting) => return Prepared::Waiting,
            Err(_) => None,
        };
        if rebuilt.as_ref() != Some(&frozen.body) {
            return pending(EngramVerificationRecordPending::BodyMismatch);
        }
        if !engram_verification_record_digest_answers(&body_digest, &frozen.body) {
            return pending(EngramVerificationRecordPending::AcknowledgedBodyChanged);
        }
        if let Err(why) = engram_verification_record_receipt_check(&frozen.body, &receipt) {
            return pending(EngramVerificationRecordPending::ReceiptMismatch {
                detail: why.to_owned(),
            });
        }
        return Prepared::ReceiptHeld { frozen, receipt };
    }
    let (request, results) = match inputs() {
        Ok(inputs) => inputs,
        Err(prepared) => return prepared,
    };
    let stages = engram_verification_record_registration_unclassified_stages(
        registration,
        &request,
        &results,
    );
    let refusal = if stages.is_empty() {
        engram_verification_record_producer_refusal(registration, &results)
    } else {
        Some(EngramVerificationRecordPending::UnclassifiedStages { stages })
    };
    if let Some(reason) = refusal {
        return pending(reason);
    }
    // A retained body that cannot be read just now is read again later: built
    // without its retained input, the record could look like another body.
    let Ok(retained_text) = engram_verification_record_body_text(&run_dir) else {
        return Prepared::Waiting;
    };
    let retained = retained_text.as_ref().and_then(|text| {
        text.as_deref().and_then(|text| {
            engram_verification_record_frozen_text(&run_dir, &registration.run_id, text)
        })
    });
    let built = match retained.as_ref() {
        Some(frozen) => engram_verification_record_build_body(
            registration,
            &request,
            &results,
            Some(&frozen.body.input),
        ),
        None => engram_verification_record_body(registration, &request, &results),
    }
    .and_then(|body| engram_verification_record_text(&body).map(|text| (body, text)));
    let (rebuilt, rebuilt_text) = match built {
        Ok(built) => built,
        Err(detail) => {
            let changes = registration
                .declared_native
                .as_ref()
                .zip(registration.declared_terminal.as_ref())
                .and_then(|(start, terminal)| {
                    terminal.source.as_ref().map(|after| {
                        engram_source_id_differences(&start.source.source_id, &after.source_id)
                    })
                });
            if let Some(changes) = changes.filter(|changes| {
                !changes.is_empty() && engram_source_id_change_detail(changes) == detail
            }) {
                return pending(EngramVerificationRecordPending::SourceChanged { detail, changes });
            }
            return pending(EngramVerificationRecordPending::BodyNotBuilt { detail });
        }
    };
    // The body this look already read, or the one it writes now: one read
    // of the frozen body per look, so no second read can fail after the
    // first succeeded.
    let body_path = run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    let text = match retained_text {
        Some(text) => text,
        None => {
            if engram_verification_record_write(&body_path, rebuilt_text.as_bytes()).is_err() {
                return pending(EngramVerificationRecordPending::PersistFailed);
            }
            Some(rebuilt_text)
        }
    };
    let Some((record, body)) = text.as_deref().and_then(engram_verification_record_parse) else {
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

/// A purely local disposition shared by direct sends and the retry tick. The
/// tick checks it before resolving a route; held receipts bypass it entirely.
fn engram_verification_record_retire_legacy(
    frozen: &EngramVerificationRecordFrozen,
) -> Option<EngramVerificationRecordStep> {
    if !matches!(
        frozen.body.input,
        EngramVerificationRecordInput::Commit { .. }
    ) {
        return None;
    }
    let detail = "legacy_source_input_retired: retained commit input is not sent; any earlier producer outcome is unchanged".to_owned();
    eprintln!(
        "engram> run={} verification record not settled: {detail}",
        frozen.run_id
    );
    engram_verification_record_set_progress(
        &frozen.run_dir,
        &frozen.run_id,
        EngramVerificationRecordProgress::Rejected {
            detail: detail.clone(),
        },
    );
    Some(EngramVerificationRecordStep::Rejected(detail))
}

/// Sends current input when its connection supports the operation. Retired
/// input rejects locally; missing capability/link leaves current input pending.
pub(crate) fn engram_verification_record_send(
    frozen: EngramVerificationRecordFrozen,
    link: std::result::Result<EngramVerificationRecordLink<'_>, EngramVerificationRecordPending>,
) -> EngramVerificationRecordStep {
    if let Some(retired) = engram_verification_record_retire_legacy(&frozen) {
        return retired;
    }
    let pending = |reason: EngramVerificationRecordPending| {
        engram_verification_record_set_progress(
            &frozen.run_dir,
            &frozen.run_id,
            EngramVerificationRecordProgress::Pending {
                reason: reason.clone(),
            },
        );
        EngramVerificationRecordStep::Pending(reason)
    };
    let rejected = |detail: String, producer_body_digest: Option<String>| {
        eprintln!(
            "engram> run={} verification record not settled: {detail}",
            frozen.run_id
        );
        engram_verification_record_set_progress(
            &frozen.run_dir,
            &frozen.run_id,
            EngramVerificationRecordProgress::Rejected {
                detail: detail.clone(),
            },
        );
        match producer_body_digest {
            Some(body_digest) => EngramVerificationRecordStep::ProducerRejected {
                detail,
                body_digest,
            },
            None => EngramVerificationRecordStep::Rejected(detail),
        }
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
    // Engram validates both command labels before association. Refuse the
    // original command explicitly rather than changing its text/fingerprint.
    const COMMAND_LIMIT: usize = 8192;
    for (field, command) in std::iter::once(("run.command".to_owned(), &frozen.body.run.command))
        .chain(
            frozen
                .body
                .checks
                .iter()
                .enumerate()
                .map(|(index, check)| (format!("checks[{index}].command"), &check.command)),
        )
    {
        if command.len() > COMMAND_LIMIT {
            return rejected(
                format!(
                    "verification_command_too_long: {field} has {} UTF-8 bytes; Engram limit is {COMMAND_LIMIT} bytes",
                    command.len()
                ),
                None,
            );
        }
    }
    let request = engram_verification_record_request(&frozen);
    let reply = link
        .adapter
        .request(link.connection, &request, link.timeout);
    match reply {
        Ok(value) => {
            let receipt = match parse_engram_result::<EngramVerificationRecordReceipt>(value) {
                Ok(receipt) => receipt,
                Err(_) => return rejected("the receipt is unreadable".to_owned(), None),
            };
            if let Err(why) = engram_verification_record_receipt_check(&frozen.body, &receipt) {
                return rejected(why.to_owned(), None);
            }
            // Held beside the run before anything else, so no later look
            // sends again; marked settled only once its records reached the
            // run's associations durably
            // (`AppState::complete_engram_verification_record`).
            engram_verification_record_set_progress(
                &frozen.run_dir,
                &frozen.run_id,
                EngramVerificationRecordProgress::Received {
                    receipt: receipt.clone(),
                },
            );
            EngramVerificationRecordStep::Received {
                body: frozen.body,
                receipt,
            }
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
                && error.code.as_deref().is_some_and(|code| {
                    ENGRAM_VERIFICATION_RECORD_DEFINITE_REFUSALS.contains(&code)
                }) =>
        {
            rejected(
                engram_verification_record_refusal_detail(
                    error.code.as_deref(),
                    error.refused_field.as_deref(),
                ),
                Some(engram_verification_record_body_digest(&frozen.body)),
            )
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

/// The detail kept for a definite refusal: Engram's code alone, which says
/// nothing private. A refusal of the record's claim binding also names its
/// usual cause and what avoids it, because the run cannot be credited again:
/// Engram refuses a record whose claim, fence or run moved after
/// registration, and a claim that lapsed and was taken again has a new fence.
pub(crate) fn engram_verification_record_refusal_detail(
    code: Option<&str>,
    field: Option<&str>,
) -> String {
    let refused = format!("Engram refused it: {}", code.unwrap_or("no code"));
    if field == Some("claim") {
        format!(
            "{refused} (field: claim): the claim was recovered under a new fence before the record \
             was sent, or it lapsed, a handoff of it was pending, or the claim or its run moved \
             after registration; keep the claim live until the record lands, and offer no handoff \
             of it until then"
        )
    } else {
        refused
    }
}

/// Only a definitive reply from Engram can be disclosed as a producer
/// rejection. The same host state also retains local terminal refusals.
pub(crate) fn engram_verification_rejected_by_producer(
    ack: Option<&TestRunVerificationAck>,
) -> bool {
    let Some(TestRunVerificationAck::Rejected {
        detail,
        producer_body_digest,
    }) = ack
    else {
        return false;
    };
    if producer_body_digest.is_some() {
        return true;
    }
    // The historical host record distinguished producer refusals only in its
    // detail. Using it for admission also requires a retained terminal.
    let Some(reason) = detail.strip_prefix("Engram refused it: ") else {
        return false;
    };
    let code = reason.split_once(" (").map_or(reason, |(code, _)| code);
    ENGRAM_VERIFICATION_RECORD_DEFINITE_REFUSALS.contains(&code)
}

/// The source basis of a run's checks in its original named root. Current
/// records retain the complete owner value. Historical commit records remain
/// readable, but cannot create new producer observations. Staged inputs also
/// supply no producer basis.
pub(crate) fn engram_verification_record_basis(
    body: &EngramVerificationRecordBody,
) -> Option<EngramExecutionSourceBasis> {
    let (source_id, source_revision) = match &body.input {
        EngramVerificationRecordInput::SourceId { source_id } => {
            (Some(source_id.clone()), String::new())
        }
        EngramVerificationRecordInput::Commit { .. }
        | EngramVerificationRecordInput::StagedTree { .. } => return None,
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
pub(crate) fn engram_verification_record_producers(
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
                    observation_id: sha256_hex(
                        format!("termal-run:{run_id}:{}", check.stage).as_bytes(),
                    ),
                    action_fingerprint: check.check_fingerprint.clone(),
                    effect: EngramEffect::Observe,
                    outcome: match check.outcome {
                        EngramVerificationRecordCheckOutcome::Passed => {
                            EngramExecutionOutcome::Succeeded
                        }
                        EngramVerificationRecordCheckOutcome::Failed => {
                            EngramExecutionOutcome::Failed
                        }
                        EngramVerificationRecordCheckOutcome::NotRun
                        | EngramVerificationRecordCheckOutcome::Indeterminate => {
                            EngramExecutionOutcome::Unknown
                        }
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
pub(crate) struct EngramVerificationRecordCursor {
    pub(crate) last: Option<String>,
    pub(crate) backoff: HashMap<String, EngramVerificationRecordBackoff>,
    // Presentation repaired once per unchanged registration in this process.
    // A fresh process retries from the authoritative local disposition.
    pub(crate) local_refusals: HashMap<String, EngramLocalRefusalPresentation>,
    /// The settled run whose state-file copy the last repair looked at; the
    /// next repair goes on after it (`repair_settled_verification_mirrors`).
    pub(crate) last_mirror_repair: Option<String>,
}

#[derive(Debug)]
pub(crate) struct EngramVerificationRecordBackoff {
    pub(crate) next: std::time::Instant,
    pub(crate) wait: Duration,
    pub(crate) ack: Option<TestRunVerificationAck>,
}

impl EngramVerificationRecordCursor {
    pub(crate) fn waits(
        &mut self,
        registration: &TestRunRegistration,
        now: std::time::Instant,
    ) -> bool {
        if self
            .backoff
            .get(&registration.run_id)
            .is_some_and(|backoff| backoff.ack != registration.verification_record)
        {
            self.backoff.remove(&registration.run_id);
        }
        self.backoff
            .get(&registration.run_id)
            .is_some_and(|backoff| now < backoff.next)
    }

    fn back_off(&mut self, registration: &TestRunRegistration, now: std::time::Instant) {
        let wait = self
            .backoff
            .get(&registration.run_id)
            .map_or(ENGRAM_VERIFICATION_RECORD_BACKOFF_START, |backoff| {
                (backoff.wait * 2).min(ENGRAM_VERIFICATION_RECORD_BACKOFF_MAX)
            });
        self.backoff.insert(
            registration.run_id.clone(),
            EngramVerificationRecordBackoff {
                next: now + wait,
                wait,
                ack: registration.verification_record.clone(),
            },
        );
    }

    pub(crate) fn retain(&mut self, selected: &[TestRunRegistration]) {
        let runs: HashSet<&str> = selected
            .iter()
            .map(|registration| registration.run_id.as_str())
            .collect();
        self.backoff
            .retain(|run_id, _| runs.contains(run_id.as_str()));
        self.local_refusals
            .retain(|run_id, _| runs.contains(run_id.as_str()));
    }

    pub(crate) fn order(
        &self,
        mut registrations: Vec<TestRunRegistration>,
    ) -> Vec<TestRunRegistration> {
        registrations.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        let start = self.last.as_ref().map_or(0, |last| {
            registrations
                .iter()
                .position(|registration| registration.run_id > *last)
                .unwrap_or(0)
        });
        registrations.rotate_left(start);
        registrations
    }
}

/// Whether the observation `row` is this registered run's own launch.
pub(crate) fn engram_verification_record_row_matches(
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
    pub(crate) fn engram_verification_records_tick(&self) {
        self.engram_verification_records_tick_within(
            ENGRAM_VERIFICATION_RECORD_TICK_BUDGET,
            &|| false,
        );
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
    pub(crate) fn engram_verification_records_tick_within(
        &self,
        budget: Duration,
        stopping: &dyn Fn() -> bool,
    ) -> usize {
        self.engram_verification_records_tick_from(
            budget,
            stopping,
            &mut EngramVerificationRecordCursor::default(),
        )
    }

    /// `engram_verification_records_tick_within`, starting after the run
    /// `cursor` names and leaving it at the last run this pass looked at.
    /// A run whose look left it pending, or its receipt unsettled, waits out
    /// its backoff in `cursor` before the next look.
    pub(crate) fn engram_verification_records_tick_from(
        &self,
        budget: Duration,
        stopping: &dyn Fn() -> bool,
        cursor: &mut EngramVerificationRecordCursor,
    ) -> usize {
        let clock = self.engram_budget_clock();
        let deadline = clock.now() + budget;
        // Producer outcomes are finished. Local refusals also retain bounded
        // presentation repair work until this process has completed it.
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
                            Some(
                                TestRunVerificationAck::Settled { .. }
                                    | TestRunVerificationAck::Rejected { .. }
                            )
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
            if cursor
                .local_refusals
                .get(&registration.run_id)
                .is_some_and(|presentation| {
                    presentation.registration == registration && presentation.finished()
                })
            {
                continue;
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
            let mut attempted = registration.clone();
            let done = if matches!(
                registration.verification_record,
                Some(TestRunVerificationAck::LocalUnclassified { .. })
            ) {
                cursor.repair_local_refusal(self, &registration)
            } else {
                match self.prepare_engram_verification_record(&registration) {
                    EngramVerificationRecordPrepared::Ready(frozen) => {
                        let step = if let Some(retired) =
                            engram_verification_record_retire_legacy(&frozen)
                        {
                            retired
                        } else {
                            let route = self
                                .engram_verification_record_route_until(&registration, deadline);
                            engram_verification_record_send(
                                frozen,
                                route
                                    .as_ref()
                                    .map(EngramVerificationRecordRoute::link)
                                    .map_err(Clone::clone),
                            )
                        };
                        self.note_engram_verification_record_step(&registration, &step);
                        match step {
                            EngramVerificationRecordStep::Received { body, receipt } => {
                                settled(self.complete_engram_verification_record_until(
                                    &registration,
                                    &body,
                                    &receipt,
                                    deadline,
                                ))
                            }
                            EngramVerificationRecordStep::Rejected(_)
                            | EngramVerificationRecordStep::ProducerRejected { .. } => true,
                            EngramVerificationRecordStep::Pending(_) => false,
                        }
                    }
                    EngramVerificationRecordPrepared::ReceiptHeld { frozen, receipt } => {
                        settled(self.complete_engram_verification_record_until(
                            &registration,
                            &frozen.body,
                            &receipt,
                            deadline,
                        ))
                    }
                    EngramVerificationRecordPrepared::Pending(
                        EngramVerificationRecordPending::UnclassifiedStages { stages },
                    ) if registration.declared_native.is_none() => {
                        if self.reject_unclassified_verification_record(&registration, &stages) {
                            attempted.verification_record =
                                Some(TestRunVerificationAck::LocalUnclassified {
                                    detail: unclassified_verification_detail(&stages),
                                });
                            cursor.repair_local_refusal(self, &attempted)
                        } else {
                            false
                        }
                    }
                    EngramVerificationRecordPrepared::Pending(_) => false,
                    EngramVerificationRecordPrepared::NotEligible
                    | EngramVerificationRecordPrepared::Waiting
                    | EngramVerificationRecordPrepared::Finished => true,
                }
            };
            // Left pending, or with its receipt unsettled: looked at again
            // only after its backoff, unless its acknowledgment changes first.
            if done {
                cursor.backoff.remove(&registration.run_id);
            } else {
                cursor.back_off(&attempted, clock.now());
            }
        }
        // Copies for the launcher summary come after the producer's own work,
        // within what is left of this pass.
        self.repair_settled_verification_mirrors(cursor, deadline, stopping);
        looked
    }

    /// `engram_verification_record_prepare` for the producer's look, keeping
    /// what the look came to in memory (`test_run_record_pending`): its
    /// pending reason, or that it found none. A run wait decides from this
    /// latest look, even when the state file beside the run cannot be
    /// written or is older.
    pub(crate) fn prepare_engram_verification_record(
        &self,
        registration: &TestRunRegistration,
    ) -> EngramVerificationRecordPrepared {
        // The look's place in the sequence, taken before it reads any file,
        // and the look in flight until it publishes what it came to.
        let begun = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let begun = inner.test_run_look_sequence;
            inner.test_run_look_sequence += 1;
            inner
                .test_run_record_looks_in_flight
                .insert(registration.run_id.clone(), begun);
            begun
        };
        let no_record = std::cell::Cell::new(false);
        let prepared = engram_verification_record_prepare_noting(registration, &no_record);
        // A frozen body this process may send from memory.
        let built = matches!(
            prepared,
            EngramVerificationRecordPrepared::Ready(_)
                | EngramVerificationRecordPrepared::ReceiptHeld { .. }
        );
        let look = match &prepared {
            EngramVerificationRecordPrepared::Pending(reason) => {
                EngramVerificationRecordLook::Pending(reason.clone())
            }
            // The same read the look came from says whether the run's own
            // files still make a record. For a run still going, or one read
            // while its terminal results were being published, that says
            // nothing yet: a run wait counts it only when the look began
            // after the wait knew the run had finished.
            EngramVerificationRecordPrepared::NotEligible
            | EngramVerificationRecordPrepared::Waiting
                if no_record.get() =>
            {
                EngramVerificationRecordLook::NoRecord { begun }
            }
            _ => EngramVerificationRecordLook::NoBlocker,
        };
        // The look, its frozen body and its end, published together: a run
        // wait never sees one without the others.
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if built {
            inner
                .test_run_record_bodies_built
                .insert(registration.run_id.clone());
        }
        store_engram_verification_record_look_locked(&mut inner, &registration.run_id, look);
        let published = inner
            .test_run_record_look_published
            .entry(registration.run_id.clone())
            .or_insert(begun);
        *published = (*published).max(begun);
        if inner
            .test_run_record_looks_in_flight
            .get(&registration.run_id)
            == Some(&begun)
        {
            inner
                .test_run_record_looks_in_flight
                .remove(&registration.run_id);
        }
        drop(inner);
        prepared
    }

    /// Keeps what the run's latest look came to: `reason`, or no blocker.
    fn note_engram_verification_record_look(
        &self,
        run_id: &str,
        reason: Option<EngramVerificationRecordPending>,
    ) {
        let look = match reason {
            Some(reason) => EngramVerificationRecordLook::Pending(reason),
            None => EngramVerificationRecordLook::NoBlocker,
        };
        self.store_engram_verification_record_look(run_id, look);
    }

    /// Keeps `look` as the run's latest look.
    fn store_engram_verification_record_look(
        &self,
        run_id: &str,
        look: EngramVerificationRecordLook,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        store_engram_verification_record_look_locked(&mut inner, run_id, look);
    }

    /// `complete_engram_verification_record_until`, waiting at most
    /// `ENGRAM_VERIFICATION_RECORD_SETTLE_FENCE` for durability.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn complete_engram_verification_record(
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
    pub(crate) fn complete_engram_verification_record_until(
        &self,
        registration: &TestRunRegistration,
        body: &EngramVerificationRecordBody,
        receipt: &EngramVerificationRecordReceipt,
        deadline: std::time::Instant,
    ) -> EngramVerificationRecordSettlement {
        // The requester's notice is presented in the Settled step itself
        // (`complete_engram_verification_record_state_until`).
        self.complete_engram_verification_record_state_until(registration, body, receipt, deadline)
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
            return EngramVerificationRecordSettlement::ReceiptMismatch {
                detail: why.to_owned(),
            };
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
            EngramVerificationRecordProgress::Received {
                receipt: receipt.clone(),
            },
        );
        // Whether a launch observation of the run carries the records, kept
        // for a run wait, which resumes on a receipt none carries.
        let association = self.settle_engram_verification_record(registration, body, receipt);
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            match &association {
                EngramVerificationRecordAssociation::Associated(_) => {
                    inner
                        .test_run_unassociated_receipts
                        .remove(&registration.run_id);
                }
                EngramVerificationRecordAssociation::NoLaunchRow => {
                    inner
                        .test_run_unassociated_receipts
                        .insert(registration.run_id.clone(), body_digest.clone());
                }
            }
        }
        let rows = match association {
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
        // The requester's notice is read off the lock now that durability is
        // confirmed, however late the confirmation came, and presented in
        // the same state step as the Settled ack, so no turn opens between
        // them and no settlement goes untold.
        let notice = ReceivedCheckNotice::at_settlement(
            registration,
            body,
            receipt,
            std::time::Instant::now() + RECEIVED_NOTICE_CAPTURE_BUDGET,
        );
        self.note_engram_verification_record_ack_presenting(
            registration,
            TestRunVerificationAck::Settled {
                receipt: receipt.clone(),
                body_digest,
            },
            notice,
        );
        // The state file is a copy for the launcher summary: one that cannot
        // be written now is written again by the producer's ticks.
        if !engram_verification_record_set_progress(
            run_dir,
            &registration.run_id,
            EngramVerificationRecordProgress::Settled {
                receipt: receipt.clone(),
            },
        ) {
            self.inner
                .lock()
                .expect("state mutex poisoned")
                .test_run_unmirrored_settlements
                .insert(registration.run_id.clone());
        }
        EngramVerificationRecordSettlement::Settled
    }

    /// Writes the Settled state file again, from the host's own Settled ack
    /// (live or retained registration), for settled runs whose copy could not
    /// be written (`test_run_unmirrored_settlements`). It runs after the
    /// producer's own work, at most `ENGRAM_VERIFICATION_MIRROR_REPAIRS_PER_TICK`
    /// runs a tick in turn from `cursor`, and stops at `deadline` or when
    /// stopping. A run is dropped once its copy holds, once it is no longer
    /// settled, or once its run directory is gone (no summary can read a copy
    /// there; a restart seeds it again from the registration). A state path
    /// that is not a regular file is left alone, never read.
    fn repair_settled_verification_mirrors(
        &self,
        cursor: &mut EngramVerificationRecordCursor,
        deadline: std::time::Instant,
        stopping: &dyn Fn() -> bool,
    ) {
        let clock = self.engram_budget_clock();
        let pending: Vec<(String, Option<(PathBuf, EngramVerificationRecordReceipt)>)> = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let after = cursor.last_mirror_repair.clone().unwrap_or_default();
            let ids = &inner.test_run_unmirrored_settlements;
            ids.range::<String, _>((
                std::ops::Bound::Excluded(&after),
                std::ops::Bound::Unbounded,
            ))
            .chain(ids.range::<String, _>(..=&after))
            .take(ENGRAM_VERIFICATION_MIRROR_REPAIRS_PER_TICK)
            .map(|run_id| {
                let settled = test_run_registration_live_or_retained(&inner, run_id).and_then(
                    |row| match &row.verification_record {
                        Some(TestRunVerificationAck::Settled { receipt, .. }) => {
                            Some((PathBuf::from(&row.run_directory), receipt.clone()))
                        }
                        _ => None,
                    },
                );
                (run_id.clone(), settled)
            })
            .collect()
        };
        for (run_id, settled) in pending {
            if stopping() || clock.now() >= deadline {
                break;
            }
            cursor.last_mirror_repair = Some(run_id.clone());
            let done = settled.is_none_or(|(run_dir, receipt)| {
                let state_file = run_dir.join(ENGRAM_VERIFICATION_RECORD_STATE_FILE);
                if !run_dir.is_dir() {
                    return true;
                }
                if fs::symlink_metadata(&state_file).is_ok_and(|meta| !meta.file_type().is_file()) {
                    return false;
                }
                engram_verification_record_put_progress(
                    &run_dir,
                    &EngramVerificationRecordProgress::Settled { receipt },
                )
                .is_ok()
            });
            if done {
                self.inner
                    .lock()
                    .expect("state mutex poisoned")
                    .test_run_unmirrored_settlements
                    .remove(&run_id);
            }
        }
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
        if self
            .persist_tx
            .send(PersistRequest::Fence(Box::new(fence)))
            .is_ok()
        {
            return waiter.wait().map_err(|error| format!("{error:?}"));
        }
        let inner = self.inner.lock().expect("state mutex poisoned");
        self.persist_internal_locked(&inner)
            .map_err(|error| format!("{error:#}"))
    }

    /// Keeps what one send step observed on the run's registration: a
    /// received receipt or a rejection; a pending step keeps only its reason
    /// in memory (`test_run_record_pending`), and any other step that it
    /// found no blocker.
    pub(crate) fn note_engram_verification_record_step(
        &self,
        registration: &TestRunRegistration,
        step: &EngramVerificationRecordStep,
    ) {
        self.note_engram_verification_record_look(
            &registration.run_id,
            match step {
                EngramVerificationRecordStep::Pending(reason) => Some(reason.clone()),
                _ => None,
            },
        );
        let ack = match step {
            EngramVerificationRecordStep::Received { body, receipt } => {
                TestRunVerificationAck::Received {
                    receipt: receipt.clone(),
                    body_digest: engram_verification_record_body_digest(body),
                }
            }
            EngramVerificationRecordStep::Rejected(detail) => TestRunVerificationAck::Rejected {
                detail: detail.clone(),
                producer_body_digest: None,
            },
            EngramVerificationRecordStep::ProducerRejected {
                detail,
                body_digest,
            } => TestRunVerificationAck::Rejected {
                detail: detail.clone(),
                producer_body_digest: Some(body_digest.clone()),
            },
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
    pub(crate) fn note_engram_verification_record_ack(
        &self,
        registration: &TestRunRegistration,
        ack: TestRunVerificationAck,
    ) {
        self.note_engram_verification_record_ack_presenting(registration, ack, None);
    }

    /// `note_engram_verification_record_ack`, presenting `notice` to the
    /// requesting session under the same lock once the ack is written
    /// (`AppState::present_settled_receipt_locked`).
    pub(crate) fn note_engram_verification_record_ack_presenting(
        &self,
        registration: &TestRunRegistration,
        ack: TestRunVerificationAck,
        notice: Option<ReceivedCheckNotice>,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let changes = inner.test_run_registrations.get(&registration.run_id).is_some_and(|row| {
            row.request_identity == registration.request_identity
                && row.run_directory == registration.run_directory
                && row.verification_record.as_ref() != Some(&ack)
                && !(matches!(ack, TestRunVerificationAck::Received { .. })
                    && matches!(
                        row.verification_record,
                        Some(TestRunVerificationAck::Settled { .. } | TestRunVerificationAck::Rejected { .. }
                            | TestRunVerificationAck::LocalUnclassified { .. })
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
        if !matches!(ack, TestRunVerificationAck::Received { .. }) {
            // When it became final, for a run wait on a run the index has not
            // listed (test_run_waits.rs).
            let generation = inner.test_run_scan_generation;
            inner
                .test_run_record_final_at_scan
                .entry(registration.run_id.clone())
                .or_insert(generation);
            // A received record held the run against compaction
            // (test_run_compaction.rs): select again now that it is final.
            inner.test_runs.compaction_examined_revision = None;
        }
        inner
            .test_run_registrations
            .update_unindexed_fields(&registration.run_id, |row| {
                row.verification_record = Some(ack)
            });
        if let Some(mut notice) = notice {
            AppState::present_settled_receipt_locked(&mut inner, &mut notice);
        }
        if let Err(error) = self.persist_internal_locked(&inner) {
            eprintln!(
                "engram> run={} verification record outcome not persisted: {error:#}",
                registration.run_id
            );
        }
    }

    /// `engram_verification_record_route_until` within one tick's budget.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn engram_verification_record_route(
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
                || target.settings.authority_store_key.as_ref()
                    != registration.authority_store.as_ref()
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
        target
            .adapter
            .read_work_session(
                &target.connection,
                registration
                    .authority_store
                    .as_ref()
                    .expect("checked store"),
                timeout_left()?,
            )
            .map_err(|error| EngramVerificationRecordPending::TransportFailed {
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
    pub(crate) fn settle_engram_verification_record(
        &self,
        registration: &TestRunRegistration,
        body: &EngramVerificationRecordBody,
        receipt: &EngramVerificationRecordReceipt,
    ) -> EngramVerificationRecordAssociation {
        let (Some(store), Some(binding)) = (
            registration.authority_store.as_ref(),
            registration.work_binding.as_ref(),
        ) else {
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
        let mut settlements =
            HashMap::<&str, Vec<(&EngramExecutionObservationInput, &String)>>::new();
        for (stage, producer) in &producers {
            if let Some(record_id) = records.get(stage.as_str()) {
                settlements
                    .entry(producer.observation_id.as_str())
                    .or_default()
                    .push((producer, *record_id));
            }
        }
        for row in
            Self::settle_test_run_producer_records_locked(&mut inner, settlements, store, binding)
        {
            changed.insert(row.execution_id.clone(), row);
        }
        self.persist_test_run_evidence_batch_locked(
            &mut inner,
            changed.into_values().collect(),
            Vec::new(),
        );
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
