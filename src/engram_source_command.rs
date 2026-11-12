// Adapter for the producer-owned `engram source --root ABS --json` v1 contract.
// Owns bounded envelope admission, not Git's committed-input predicate, source
// authority, the content revision, or the run/review fingerprint algorithms.

const ENGRAM_SOURCE_JSON_LIMIT: usize = 1024 * 1024;
// The same scalar configures the launcher's source process and metadata GET.
const ENGRAM_SOURCE_COMMAND_BUDGET: Duration =
    Duration::from_millis(include!("../scripts/source-id-budget-ms.json"));
const ENGRAM_SOURCE_PROCESS_BUDGET: Duration = Duration::from_millis(
    if ENGRAM_SOURCE_COMMAND_BUDGET.as_millis() > ENGRAM_CALL_BUDGET_MS[0] as u128 {
        ENGRAM_SOURCE_COMMAND_BUDGET.as_millis() as u64 + ENGRAM_CALL_BUDGET_MS[1]
    } else {
        ENGRAM_CALL_BUDGET_MS[0] + ENGRAM_CALL_BUDGET_MS[1]
    },
);
const ENGRAM_SOURCE_METADATA_BUDGET: Duration = ENGRAM_SOURCE_PROCESS_BUDGET.saturating_mul(2);

/// One admitted plan, retained through queueing and every nested source helper.
/// The first window funds preparation/version; the second leaves a late-issued
/// source process its own default and delivery reserve. Longer source stages
/// stay longer; the owner gives its version process a separate finite window.
#[derive(Clone, Copy, Debug)]
struct EngramSourcePhasePlan {
    probe_deadline: std::time::Instant,
    source_admission_deadline: std::time::Instant,
    completion_deadline: std::time::Instant,
    admitted: bool,
}
impl EngramSourcePhasePlan {
    fn at(requested: std::time::Instant, admitted_at: std::time::Instant) -> Self {
        let probe_deadline = requested.max(admitted_at + ENGRAM_SOURCE_PROCESS_BUDGET);
        Self { probe_deadline, source_admission_deadline: probe_deadline,
            completion_deadline: probe_deadline + ENGRAM_SOURCE_PROCESS_BUDGET,
            admitted: requested > admitted_at }
    }

    fn check(&self) -> Result<()> {
        if !self.admitted { bail!("Engram source deadline exceeded"); }
        engram_source_before_deadline(self.source_admission_deadline)
    }

    fn source_deadline_at(&self, now: std::time::Instant) -> Result<std::time::Instant> {
        if !self.admitted || now >= self.source_admission_deadline { bail!("Engram source deadline exceeded"); }
        Ok(self.probe_deadline.max(now + ENGRAM_SOURCE_PROCESS_BUDGET))
    }

    fn remaining(&self) -> Duration {
        self.completion_deadline.saturating_duration_since(std::time::Instant::now())
    }

    fn following_observation(self) -> Self {
        Self { probe_deadline: self.probe_deadline,
            source_admission_deadline: self.completion_deadline,
            completion_deadline: self.completion_deadline + ENGRAM_SOURCE_PROCESS_BUDGET,
            admitted: self.admitted }
    }
}

impl From<std::time::Instant> for EngramSourcePhasePlan {
    fn from(deadline: std::time::Instant) -> Self { Self::at(deadline, std::time::Instant::now()) }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EngramSourceStatus {
    Clean,
    NotClean,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EngramSourceRefusalCode {
    InputUnavailable,
    Deadline,
    OutputLimit,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EngramSourceRefusal {
    code: EngramSourceRefusalCode,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EngramSourceEntry {
    status: String,
    path: String,
    origin: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EngramSourceObservation {
    version: u32,
    status: EngramSourceStatus,
    #[serde(deserialize_with = "engram_source_required_nullable")]
    root: Option<String>,
    #[serde(deserialize_with = "engram_source_required_nullable")]
    source: Option<String>,
    #[serde(deserialize_with = "engram_source_required_nullable")]
    tree: Option<String>,
    entries: Vec<EngramSourceEntry>,
    #[serde(deserialize_with = "engram_source_required_nullable")]
    refusal: Option<EngramSourceRefusal>,
    #[serde(deserialize_with = "engram_source_required_nullable")]
    source_id: Option<EngramSourceId>,
}

impl EngramSourceObservation {
    fn into_source_id(self, deadline: std::time::Instant) -> Result<EngramSourceId> {
        engram_source_before_deadline(deadline)?;
        if let Some(refusal) = self.refusal {
            let message: String = refusal.message.chars().take(1024)
                .flat_map(char::escape_default).collect();
            bail!("Engram source {:?}: {message}", refusal.code);
        }
        let source_id = self.source_id.context("Engram source supplied no complete source_id")?;
        engram_source_before_deadline(deadline)?;
        Ok(source_id)
    }
}

// Missing is not an alternative spelling of the required explicit JSON null.
fn engram_source_required_nullable<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn engram_source_before_deadline(deadline: std::time::Instant) -> Result<()> {
    check_capture_cancellation()?;
    if std::time::Instant::now() >= deadline {
        bail!("Engram source deadline exceeded");
    }
    Ok(())
}

fn parse_engram_source_observation(
    stdout: &[u8],
    exit_code: Option<i32>,
    expected_root: &FsPath,
    deadline: std::time::Instant,
) -> Result<EngramSourceObservation> {
    engram_source_before_deadline(deadline)?;
    // The owner permits one trailing LF outside its one-MiB JSON bound.
    let json = stdout.strip_suffix(b"\n").unwrap_or(stdout);
    if json.len() > ENGRAM_SOURCE_JSON_LIMIT {
        bail!("Engram source output_limit: JSON exceeds one MiB");
    }
    let observation: EngramSourceObservation = serde_json::from_slice(json)
        .context("Engram source returned invalid UTF-8 or a malformed/unsupported envelope")?;
    engram_source_before_deadline(deadline)?;
    if observation.version != 1 {
        bail!("Engram source returned an unsupported version");
    }
    if let Some(root) = &observation.root {
        let path = FsPath::new(root);
        if !path.is_absolute() || root.contains('\0') || root.starts_with("\\\\?\\") {
            bail!("Engram source returned an invalid root");
        }
        // The producer and host may spell a canonical directory differently
        // (macOS /var versus /private/var, or a Windows junction). Compare
        // actual directory identity and retain the caller's aggregate deadline.
        let actual = fs::canonicalize(path).context("Engram source returned an unavailable root")?;
        let expected = fs::canonicalize(expected_root).context("Engram source expected root is unavailable")?;
        let same_root = actual == expected;
        engram_source_before_deadline(deadline)?;
        if !same_root {
            bail!("Engram source returned another root");
        }
    }
    match observation.status {
        EngramSourceStatus::Clean | EngramSourceStatus::NotClean => {
            if exit_code != Some(0) || observation.root.is_none() || observation.refusal.is_some() {
                bail!("Engram source status and process exit disagree");
            }
            let commit = observation.source.as_deref()
                .and_then(|source| source.strip_prefix("git-commit:"));
            if !commit.is_some_and(|id| id.len() == 40 && is_lowercase_hex(id))
                || !observation.tree.as_deref().is_some_and(|id| id.len() == 40 && is_lowercase_hex(id))
                || (observation.status == EngramSourceStatus::Clean) != observation.entries.is_empty()
            {
                bail!("Engram source returned incoherent committed input");
            }
            let source_id = observation.source_id.as_ref()
                .context("Engram source returned no complete source_id")?;
            source_id.validate(deadline)?;
            if Some(source_id.commit.as_str()) != commit
                || Some(source_id.tree.as_str()) != observation.tree.as_deref()
            {
                bail!("Engram source_id disagrees with its provenance");
            }
            for entry in &observation.entries {
                engram_source_before_deadline(deadline)?;
                let renamed = entry.status.bytes().any(|byte| matches!(byte, b'R' | b'C'));
                if entry.status.len() != 2 || entry.status == "  "
                    || !entry.status.bytes().all(|byte| b" MADRCUT?!".contains(&byte))
                    || entry.path.is_empty() || entry.path.contains('\0')
                    || renamed != entry.origin.is_some()
                    || entry.origin.as_ref().is_some_and(|origin| origin.is_empty() || origin.contains('\0'))
                {
                    bail!("Engram source returned an invalid changed-path entry");
                }
            }
        }
        EngramSourceStatus::Unavailable => {
            if exit_code != Some(1) || observation.source.is_some() || observation.tree.is_some()
                || observation.source_id.is_some()
                || !observation.entries.is_empty()
                || !observation.refusal.as_ref().is_some_and(|reason|
                    !reason.message.trim().is_empty() && reason.message.len() <= 4096)
            {
                bail!("Engram source returned an incoherent unavailable result");
            }
        }
    }
    engram_source_before_deadline(deadline)?;
    Ok(observation)
}

fn engram_source_command_args(root: &FsPath, deadline: std::time::Instant) -> Result<Vec<String>> {
    engram_source_before_deadline(deadline)?;
    // The installed owner chooses its own supported default. Host-side waits
    // cover the newer default without imposing an argument older owners refuse.
    Ok(vec!["source".into(), "--root".into(),
        normalize_user_facing_path(root).to_str()
            .context("Engram source root is not UTF-8")?.to_owned(),
        "--json".into()])
}
