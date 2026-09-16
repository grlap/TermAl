// New Beads (bd) Work read adapter. Owns native binary resolution, the
// read-only argv transport, receipt normalization and the Beads detail route.
// Never owns tracker writes, Dolt/SQLite access, shell command strings or the
// Engram read path; both sources share only the presentation model.

const BEADS_BINARY_ENV: &str = "TERMAL_BEADS_BINARY";
const BEADS_READ_TIMEOUT: Duration = Duration::from_secs(20);
const BEADS_READ_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const BEADS_LIST_ROW_CAP: usize = 2_000;
// One `show` invocation per batch of blocker ids keeps argv well below the
// launch bound even with maximum-length identifiers.
const BEADS_STATUS_BATCH: usize = 80;
// Every batch launches bd and opens the Dolt store (~0.9 s on a small store),
// all under the one snapshot deadline. Beyond this many batches per read the
// remaining blockers are disclosed as not read instead of failing the snapshot.
const BEADS_MAX_BATCHES_PER_READ: usize = 8;
// A batch is only launched while at least this much of the snapshot deadline
// remains (a launch is ~0.9 s on a small store); otherwise the remaining
// batches are disclosed as not read instead of running the whole snapshot
// into its deadline.
const BEADS_LAUNCH_RESERVE: Duration = Duration::from_secs(3);
// Admission waits one read budget plus this margin: after a deadline the
// bounded reader still terminates the process tree and allows up to a second
// of pipe collection, so a permit can outlive the budget by that much. The
// margin keeps a queued read from a spurious "busy" while the previous one is
// being torn down.
const BEADS_ADMISSION_MARGIN: Duration = Duration::from_secs(3);

/// The time budget of one Beads snapshot or detail; it also sizes the
/// admission wait (one budget plus the teardown margin). Production uses the constants
/// above; the routes take the budget from a router-owned extension, never
/// from the request. Fixture
/// tests inject a zero reserve and a budget the interpreter launches cannot
/// exhaust, so the wall clock never decides a fixture's outcome; deadline and
/// reserve behaviour are tested only with explicit instants and reserves.
#[derive(Clone, Copy, Debug)]
struct BeadsReadOptions {
    timeout: Duration,
    reserve: Duration,
}

impl Default for BeadsReadOptions {
    fn default() -> Self {
        Self {
            timeout: BEADS_READ_TIMEOUT,
            reserve: BEADS_LAUNCH_RESERVE,
        }
    }
}

impl BeadsReadOptions {
    #[cfg(test)]
    fn for_tests() -> Self {
        Self {
            timeout: Duration::from_secs(600),
            reserve: Duration::ZERO,
        }
    }
}
// bd selects a store from these before cwd discovery; the child must only
// ever see the project's own `.beads`.
const BEADS_STORE_ENV_VARS: [&str; 2] = ["BEADS_DIR", "BEADS_DB"];

#[derive(Clone, Debug, PartialEq, Eq)]
struct BeadsReadTarget {
    binary_path: PathBuf,
    project_root: PathBuf,
}

/// Test-only stand-in for `TERMAL_BEADS_BINARY`: `Some(None)` is "unset",
/// `Some(Some(path))` a configured binary, `None` no override. Tests never
/// mutate the real process environment for it: that races with concurrent
/// `getenv`/spawn on other test threads (why edition 2024 makes `set_var`
/// unsafe) and would leak into every child those tests spawn.
#[cfg(test)]
static BEADS_BINARY_OVERRIDE: std::sync::Mutex<Option<Option<std::ffi::OsString>>> =
    std::sync::Mutex::new(None);

fn beads_binary_setting() -> Option<std::ffi::OsString> {
    #[cfg(test)]
    {
        let injected = BEADS_BINARY_OVERRIDE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(value) = injected {
            return value;
        }
    }
    std::env::var_os(BEADS_BINARY_ENV)
}

/// Locates the native `bd` executable. `TERMAL_BEADS_BINARY` wins; otherwise
/// the PATH entries are searched and an npm launcher is resolved to the native
/// binary it wraps. Shell and Node shims are never executed: a filter must
/// never pass through shell interpretation.
fn resolve_beads_binary(allow_fixtures: bool) -> Result<PathBuf, String> {
    if let Some(configured) = beads_binary_setting().filter(|v| !v.is_empty()) {
        let path = PathBuf::from(configured);
        // A relative path would resolve against the server's cwd, also
        // through the launcher fallback's canonicalisation: never searched.
        if !path.is_absolute() {
            return Err("Beads binary path must be absolute".into());
        }
        // A configured npm launcher resolves to the native binary beside it,
        // exactly as a launcher found on PATH does.
        return match validate_beads_binary(&path, allow_fixtures) {
            Ok(()) => Ok(path),
            Err(error) => beads_native_beside(&path, allow_fixtures).ok_or(error),
        };
    }
    resolve_beads_binary_on_path(std::env::var_os("PATH").as_deref(), allow_fixtures)
}

/// The PATH search, separated from the process environment so its order and
/// the launcher-to-native resolution are unit-tested.
fn resolve_beads_binary_on_path(
    path_var: Option<&std::ffi::OsStr>,
    allow_fixtures: bool,
) -> Result<PathBuf, String> {
    let Some(path_var) = path_var else {
        return Err(format!(
            "PATH is not set; install @beads/bd or set {BEADS_BINARY_ENV}"
        ));
    };
    let names: &[&str] = if cfg!(windows) {
        &["bd.exe", "bd.cmd", "bd"]
    } else {
        &["bd"]
    };
    for dir in std::env::split_paths(path_var) {
        // A relative PATH entry would resolve against the server's cwd.
        if !dir.is_absolute() {
            continue;
        }
        for name in names {
            let candidate = dir.join(name);
            if !candidate.is_file() {
                continue;
            }
            if validate_beads_binary(&candidate, allow_fixtures).is_ok() {
                return Ok(candidate);
            }
            if let Some(native) = beads_native_beside(&candidate, allow_fixtures) {
                return Ok(native);
            }
        }
    }
    Err(format!(
        "bd CLI not found on PATH; install @beads/bd or set {BEADS_BINARY_ENV}"
    ))
}

/// The npm package installs a launcher script beside `node_modules/@beads/bd/bin/`
/// (Windows) or as a symlink into that directory (Unix). Both wrap one native
/// binary that can be executed directly.
fn beads_native_beside(launcher: &FsPath, allow_fixtures: bool) -> Option<PathBuf> {
    let native_name = if cfg!(windows) { "bd.exe" } else { "bd" };
    let mut candidates = Vec::new();
    if let Some(dir) = launcher.parent() {
        candidates.push(
            dir.join("node_modules")
                .join("@beads")
                .join("bd")
                .join("bin")
                .join(native_name),
        );
    }
    if let Ok(resolved) = fs::canonicalize(launcher) {
        if let Some(dir) = resolved.parent() {
            candidates.push(dir.join(native_name));
        }
    }
    candidates
        .into_iter()
        .find(|path| path.is_file() && validate_beads_binary(path, allow_fixtures).is_ok())
}

/// `allow_fixtures` admits the interpreter-script test fixtures (`.ps1`/`.sh`)
/// and skips the shebang check. Production always passes `false`; the policy
/// is explicit so the guards themselves are unit-tested with `false`.
fn validate_beads_binary(path: &FsPath, allow_fixtures: bool) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("Beads binary path must be absolute".into());
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(extension.as_str(), "cmd" | "bat" | "js")
        || (!allow_fixtures && matches!(extension.as_str(), "ps1" | "sh"))
    {
        return Err("Beads reads require the native bd binary, not a shell or Node shim".into());
    }
    let metadata = fs::metadata(path).map_err(|e| format!("Beads binary unavailable: {e}"))?;
    if !metadata.is_file() {
        return Err("Beads binary path is not a file".into());
    }
    if !allow_fixtures {
        // A binary that cannot be opened or read is not usable either: the
        // shebang check never fails open.
        let mut head = [0u8; 2];
        let mut file = fs::File::open(path).map_err(|e| format!("Beads binary unreadable: {e}"))?;
        let read = io::Read::read(&mut file, &mut head)
            .map_err(|e| format!("Beads binary unreadable: {e}"))?;
        if read == head.len() && &head == b"#!" {
            return Err("Beads reads require the native bd binary, not a script shim".into());
        }
    }
    Ok(())
}

fn beads_read_target(project: &Project) -> Result<Option<BeadsReadTarget>, String> {
    let root = FsPath::new(&project.root_path);
    match fs::metadata(root.join(".beads")) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(".beads is not a directory".into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Cannot inspect .beads: {e}")),
    }
    // Only test builds may run the interpreter-script fixtures.
    let binary_path = resolve_beads_binary(cfg!(test))?;
    Ok(Some(BeadsReadTarget {
        binary_path,
        project_root: root.to_path_buf(),
    }))
}

fn is_beads_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// A read that ran to completion: either a JSON receipt or bd's own refusal
/// (nonzero exit with its diagnostic). Launch, deadline, output-bound and
/// non-JSON failures are `ApiError`s, never a refusal.
enum BeadsReadOutcome {
    Receipt(Value),
    Refused(BeadsRefusal),
}

/// bd's own refusal: the one-line summary reported to callers, and the raw
/// diagnostic (stderr, then stdout, line by line) it is classified from.
struct BeadsRefusal {
    summary: String,
    diagnostic: String,
}

/// Runs one read-only `bd` command under a deadline the caller shares across
/// every command of the same snapshot or detail, so a permit is never held
/// longer than one `BEADS_READ_TIMEOUT`.
fn run_beads_read_command(
    target: &BeadsReadTarget,
    args: &[String],
    deadline: std::time::Instant,
) -> Result<Value, ApiError> {
    match run_beads_read_process(target, args, deadline)? {
        BeadsReadOutcome::Receipt(value) => Ok(value),
        BeadsReadOutcome::Refused(refusal) => Err(ApiError::bad_gateway(refusal.summary)),
    }
}

/// The argv and environment of one read: `--readonly --json` first, then the
/// operation, in the project root, with the store selection variables removed
/// so the project's own `.beads` is the only store the child can open. The
/// launch itself goes through `engram_command`, which runs a native binary
/// directly and a test fixture (.ps1/.sh) through its interpreter by argv.
fn beads_read_command(target: &BeadsReadTarget, args: &[String]) -> std::process::Command {
    let mut command = engram_command(&target.binary_path);
    command
        .arg("--readonly")
        .arg("--json")
        .args(args)
        .current_dir(&target.project_root);
    for variable in BEADS_STORE_ENV_VARS {
        command.env_remove(variable);
    }
    command
}

fn run_beads_read_process(
    target: &BeadsReadTarget,
    args: &[String],
    deadline: std::time::Instant,
) -> Result<BeadsReadOutcome, ApiError> {
    // Same policy as resolution: only test builds may run script fixtures.
    validate_beads_binary(&target.binary_path, cfg!(test)).map_err(ApiError::conflict)?;
    let operation = match args.first().map(String::as_str) {
        Some("list") => "bd list",
        Some("show") => "bd show",
        Some("comments") => "bd comments",
        Some("memories") => "bd memories",
        Some("recall") => "bd recall",
        _ => return Err(ApiError::bad_request("Unsupported Beads read operation")),
    };
    // A deadline that has already passed launches nothing: the process could
    // not finish in time, and a receipt it happened to produce first must not
    // pass as evidence.
    if deadline <= std::time::Instant::now() {
        return Err(ApiError::bad_gateway(format!(
            "{operation}: Beads read deadline exhausted before launch"
        )));
    }
    let mut command = beads_read_command(target, args);
    let command_units: usize = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|arg| 2 * arg.to_string_lossy().encode_utf16().count() + 3)
        .sum();
    if command_units > 30_000 {
        // A source condition, not a failed request: a label this long is
        // something this source cannot serve, and the other source's page
        // must still arrive.
        return Err(ApiError::bad_gateway(
            "Beads read command exceeds the process launch bound; shorten the label filter",
        ));
    }
    let output = run_bounded_read_process(&mut command, deadline, BEADS_READ_OUTPUT_LIMIT, true)
        .map_err(|e| ApiError::bad_gateway(format!("{operation}: {e:#}")))?;
    let std::process::Output {
        status,
        stdout,
        stderr,
    } = output;
    if !status.success() {
        // Both streams are classified and reported: bd 1.2.2 prints the
        // unknown-id refusal of `show` on stderr but uses a JSON error on
        // stdout for other commands, and either stream may also carry an
        // unrelated notice that must not hide the other.
        let streams = [&stderr, &stdout]
            .into_iter()
            .map(|stream| {
                // bd may pretty-print one JSON error envelope on stdout
                // alongside per-id refusal lines on stderr. Preserve that
                // envelope as a single diagnostic record for classification.
                serde_json::from_slice::<Value>(stream)
                    .map(|value| value.to_string())
                    .unwrap_or_else(|_| String::from_utf8_lossy(stream).trim().to_owned())
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>();
        return Ok(BeadsReadOutcome::Refused(BeadsRefusal {
            summary: format!("{operation}: {status}: {}", streams.join(" | ")),
            diagnostic: streams.join("\n"),
        }));
    }
    serde_json::from_slice(&stdout)
        .map(BeadsReadOutcome::Receipt)
        .map_err(|e| ApiError::bad_gateway(format!("{operation}: invalid JSON: {e}")))
}

#[derive(Deserialize)]
struct BeadsListRow {
    id: String,
    title: String,
    status: String,
    priority: u8,
    issue_type: String,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    updated_at: String,
    // Required: the number of `blocks` edges bd recorded for the row (verified
    // on bd 1.2.2: parent-child and other edge types are not counted). It is
    // reconciled against the inline records; a receipt without it is a
    // changed contract, never "no dependencies".
    dependency_count: usize,
    // bd 1.2.2 lists every edge of the row inline (blocks, parent-child,
    // relates-to, discovered-from); absent when the row has none. Parsed per
    // record so one malformed record marks the row unread, not malformed.
    #[serde(default)]
    dependencies: Vec<Value>,
    // bd derives it from the parent-child edge; kept to reconcile that record.
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
}

#[derive(Deserialize)]
struct BeadsDependencyEdge {
    issue_id: String,
    depends_on_id: String,
    #[serde(rename = "type")]
    kind: String,
}

fn beads_rows_from_value<T: serde::de::DeserializeOwned>(
    value: Value,
    operation: &str,
) -> Result<Vec<T>, ApiError> {
    let rows = match value {
        Value::Array(rows) => Value::Array(rows),
        Value::Object(mut object) => {
            match object.remove("issues").or_else(|| object.remove("items")) {
                Some(rows) => rows,
                // A single-issue receipt (e.g. `show` with one id) is one row.
                None if object.contains_key("id") => Value::Array(vec![Value::Object(object)]),
                // Anything else (an empty object, an error envelope) is not a
                // known receipt and must never pass as an empty success.
                None => {
                    return Err(ApiError::bad_gateway(format!(
                        "{operation}: invalid receipt: unrecognized object shape"
                    )));
                }
            }
        }
        other => other,
    };
    if rows.is_null() {
        // A successful read with nothing to report is an empty list, never an
        // error (bd 1.2.2 prints `[]`; a nil slice would print `null`).
        return Ok(Vec::new());
    }
    serde_json::from_value(rows)
        .map_err(|e| ApiError::bad_gateway(format!("{operation}: invalid receipt: {e}")))
}

fn beads_id_batches(ids: &[String], batch: usize) -> Vec<Vec<String>> {
    ids.chunks(batch.max(1)).map(<[String]>::to_vec).collect()
}

/// The edges of one list row, from its inline records: only records naming
/// this row count, each edge once. The flag says whether a record did not
/// parse: the edges that did are kept (a blocker read is evidence of
/// blocking) and the row is disclosed as not fully read.
fn beads_row_edges(row: &BeadsListRow) -> (Vec<BeadsDependencyEdge>, bool) {
    let mut edges = Vec::with_capacity(row.dependencies.len());
    let mut seen = HashSet::new();
    let mut malformed = false;
    for record in &row.dependencies {
        // Deserialized from the borrowed value: no copy per record.
        let Ok(edge) = <BeadsDependencyEdge as serde::Deserialize>::deserialize(record) else {
            malformed = true;
            continue;
        };
        if edge.issue_id == row.id && seen.insert((edge.depends_on_id.clone(), edge.kind.clone())) {
            edges.push(edge);
        }
    }
    (edges, malformed)
}

/// Whether a row's inline records account for what the row declares: at least
/// `dependency_count` `blocks` edges, and exactly the `parent-child` edge bd's
/// `parent` names (none when it names none). Anything less means the receipt
/// did not carry every edge; such a row's readiness would be a guess.
fn beads_row_edges_complete(row: &BeadsListRow, edges: &[BeadsDependencyEdge]) -> bool {
    let blocks = edges.iter().filter(|edge| edge.kind == "blocks").count();
    let parent_edge = edges
        .iter()
        .find(|edge| edge.kind == "parent-child")
        .map(|edge| edge.depends_on_id.as_str());
    blocks >= row.dependency_count && parent_edge == row.parent.as_deref()
}

#[derive(Deserialize)]
struct BeadsStatusRow {
    id: String,
    status: String,
}

/// bd 1.2.2's own wording when it knows none of the requested ids: `Error
/// fetching <id>: no issue found matching "<id>"` (one line per id, on stderr
/// from `show`; other commands print the clause as a JSON error on stdout).
/// Only that refusal is evidence about the ids, and only when it accounts
/// for every requested id with nothing else failing.
fn beads_refusal_is_unknown_ids(diagnostic: &str, ids: &[String]) -> bool {
    // bd names each id it could not resolve on a line of its own; a batch it
    // refuses outright therefore names every requested id. Any other error
    // line (a locked store beside an unknown id) makes the refusal a failed
    // read: unread evidence must never hide a broken store. Notices are not
    // errors.
    let mut named: HashSet<&str> = HashSet::new();
    for line in diagnostic
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let lower = line.to_ascii_lowercase();
        // Only a benign notice (a version-update warning) is skipped; a
        // notice that reports an error, a failure or a lock is an error line.
        let notice = lower.starts_with("warning")
            || lower.starts_with("notice")
            || lower.starts_with("info");
        let reports_failure = ["error", "fail", "lock", "panic", "corrupt"]
            .iter()
            .any(|word| lower.contains(word));
        if notice && !reports_failure {
            continue;
        }
        // Decode the supported JSON envelope before matching, so escaped
        // quotes cannot mask a suffix (or another error field). Match the
        // complete refusal, never a substring beside a storage failure.
        let envelope;
        let message = if line.starts_with('{') {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                return false;
            };
            envelope = value;
            let Some(object) = envelope.as_object() else {
                return false;
            };
            if object
                .keys()
                .any(|key| key != "error" && key != "schema_version")
                || object
                    .get("schema_version")
                    .is_some_and(|value| value.as_u64() != Some(1))
            {
                return false;
            }
            let Some(message) = object.get("error").and_then(Value::as_str) else {
                return false;
            };
            // This aggregate stdout receipt names no ids. Only complete
            // per-id records on the other stream can prove the batch miss.
            if message == "no issues found matching the provided IDs" {
                continue;
            }
            message
        } else {
            line
        };
        let Some(id) = ids.iter().find(|id| {
            let clause = format!("no issue found matching \"{id}\"");
            message == clause
                || message == format!("Error fetching {id}: {clause}")
                || message == format!("resolving {id}: {clause}")
        }) else {
            return false;
        };
        named.insert(id.as_str());
    }
    !ids.is_empty() && ids.iter().all(|id| named.contains(id.as_str()))
}

/// `None` when bd refused the batch because it knew none of the ids; launch,
/// deadline, receipt-shape and every other refusal propagate.
fn read_beads_statuses_batch(
    target: &BeadsReadTarget,
    ids: &[String],
    deadline: std::time::Instant,
) -> Result<Option<Vec<BeadsStatusRow>>, ApiError> {
    let mut args = vec!["show".to_owned()];
    args.extend(ids.iter().cloned());
    match run_beads_read_process(target, &args, deadline)? {
        BeadsReadOutcome::Receipt(value) => beads_rows_from_value(value, "bd show").map(Some),
        BeadsReadOutcome::Refused(refusal)
            if beads_refusal_is_unknown_ids(&refusal.diagnostic, ids) =>
        {
            Ok(None)
        }
        BeadsReadOutcome::Refused(refusal) => Err(ApiError::bad_gateway(refusal.summary)),
    }
}

/// Blocker statuses gathered by `show`, plus the number of blocker ids whose
/// status could not be read: a batch bd rejects (it exits nonzero when it
/// knows none of the requested ids, and omits unknown ids from a mixed
/// receipt) or a batch beyond the per-read bound. Unread ids stay unsatisfied
/// and the count is disclosed in the page hint, never hidden.
#[derive(Debug)]
struct BeadsBlockerStatuses {
    statuses: HashMap<String, String>,
    unread: usize,
}

/// Positive evidence for satisfied prerequisites: `bd show` on every blocker
/// missing from the open snapshot (closed, a hidden category, or unknown).
/// Only bd's refusal of unknown ids is absorbed as unread evidence; it is not
/// retried per id (that would spend the deadline on launches that cannot
/// recover evidence). A store failure, a launch failure, an exhausted
/// deadline or a malformed receipt fails the snapshot: unread evidence must
/// never hide a broken store behind "unknown or deleted".
fn read_beads_blocker_statuses(
    target: &BeadsReadTarget,
    ids: &[String],
    deadline: std::time::Instant,
    reserve: Duration,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<BeadsBlockerStatuses, ApiError> {
    let mut result = BeadsBlockerStatuses {
        statuses: HashMap::new(),
        unread: 0,
    };
    for (index, batch) in beads_id_batches(ids, BEADS_STATUS_BATCH)
        .into_iter()
        .enumerate()
    {
        // Beyond the bound, past the reserve, or after the caller abandoned
        // the request: disclosed as unread, never launched.
        if index >= BEADS_MAX_BATCHES_PER_READ
            || !beads_launch_fits(deadline, reserve)
            || cancelled.load(std::sync::atomic::Ordering::Relaxed)
        {
            result.unread += batch.len();
            continue;
        }
        match read_beads_statuses_batch(target, &batch, deadline)? {
            Some(rows) => {
                let mut returned = HashSet::new();
                for row in rows {
                    returned.insert(row.id.clone());
                    result.statuses.insert(row.id, row.status);
                }
                // bd omits the ids it does not know from a mixed receipt.
                result.unread += batch.iter().filter(|id| !returned.contains(*id)).count();
            }
            None => result.unread += batch.len(),
        }
    }
    Ok(result)
}

fn beads_launch_fits(deadline: std::time::Instant, reserve: Duration) -> bool {
    deadline.saturating_duration_since(std::time::Instant::now()) >= reserve
}

/// One mistyped row is skipped and disclosed; a receipt where no row parses
/// at all is a changed contract, never an empty tracker.
fn parse_beads_list_rows(raw: Vec<Value>) -> Result<(Vec<BeadsListRow>, usize), ApiError> {
    let mut rows = Vec::with_capacity(raw.len());
    let mut malformed = 0usize;
    let mut first_error = None;
    for value in raw {
        match serde_json::from_value::<BeadsListRow>(value) {
            Ok(row) => rows.push(row),
            Err(error) => {
                malformed += 1;
                first_error.get_or_insert(error);
            }
        }
    }
    match first_error {
        Some(error) if rows.is_empty() => Err(ApiError::bad_gateway(format!(
            "bd list: invalid receipt: {error}"
        ))),
        _ => Ok((rows, malformed)),
    }
}

fn beads_lifecycle(status: &str) -> &'static str {
    if status == "closed" {
        "completed"
    } else {
        "open"
    }
}

/// Beads statuses map onto the shared availability axis. Only `open` depends
/// on the loaded `blocks` edges; bd's own `blocked`/`deferred` are honored as
/// stated, and an unknown status is disclosed verbatim rather than guessed
/// as ready.
fn beads_availability(status: &str, blocked: bool) -> String {
    match status {
        "closed" => "closed",
        "in_progress" => "active",
        "blocked" => "blocked",
        "deferred" => "deferred",
        "open" | "" => {
            if blocked {
                "blocked"
            } else {
                "ready"
            }
        }
        other => other,
    }
    .to_owned()
}

fn beads_item_view(
    id: String,
    title: String,
    status: &str,
    priority: u8,
    kind: String,
    assignee: Option<String>,
    labels: Vec<String>,
    updated_at: String,
    parent_id: Option<String>,
    prerequisites: Vec<WorkPrerequisiteView>,
) -> WorkItemView {
    let blocked_by = prerequisites
        .iter()
        .filter(|p| !p.satisfied)
        .map(|p| p.id.clone())
        .collect::<Vec<_>>();
    WorkItemView {
        source: "beads".to_owned(),
        short_ref: id.clone(),
        id,
        title,
        kind,
        lifecycle: beads_lifecycle(status).to_owned(),
        availability: beads_availability(status, !blocked_by.is_empty()),
        priority,
        labels,
        assigned_to: assignee,
        parent_id,
        updated_at,
        prerequisites,
        blocked_by,
    }
}

/// What the snapshot could not read, disclosed in the page hint.
#[derive(Default)]
struct BeadsSnapshotCoverage {
    /// Rows whose list receipt did not carry every edge they declare (fewer
    /// `blocks` records than `dependency_count`, a parent without its
    /// parent-child record, or a record that does not parse). Their
    /// availability is `unknown`, never a guessed `ready`.
    dependency_rows_unread: HashSet<String>,
    /// Blocker ids whose status could not be read; they stay unsatisfied.
    blockers_unread: usize,
    /// List rows that did not parse and were skipped.
    rows_malformed: usize,
}

/// Normalizes the full open snapshot: prerequisite state is decided against
/// every open row and the blocker statuses gathered by `show` before the
/// display cap removes anything. A blocker is satisfied only when it is not
/// open here and `show` reported it closed; absence is never completion. The
/// hierarchy comes from `parent-child` edges, never from the id spelling, so
/// reparented or orphaned dotted ids are shown as the tracker records them.
fn normalize_beads_work_page(
    rows: Vec<BeadsListRow>,
    edges: Vec<BeadsDependencyEdge>,
    blocker_statuses: &HashMap<String, String>,
    coverage: &BeadsSnapshotCoverage,
    query: &WorkListQuery,
    cap: usize,
) -> Result<WorkPage, ApiError> {
    let open_ids = rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<HashSet<String>>();
    if open_ids.len() != rows.len() {
        return Err(ApiError::bad_gateway("bd list: duplicate issue ids"));
    }
    let mut edges_by_issue: HashMap<&str, Vec<&BeadsDependencyEdge>> = HashMap::new();
    for edge in &edges {
        edges_by_issue
            .entry(edge.issue_id.as_str())
            .or_default()
            .push(edge);
    }
    let mut items = Vec::with_capacity(rows.len());
    let mut skipped = 0usize;
    for row in rows {
        if row.priority > 4 || !is_beads_identifier(&row.id) {
            // One malformed row must not hide the tracker; it is disclosed.
            skipped += 1;
            continue;
        }
        let row_edges = edges_by_issue
            .get(row.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let prerequisites = row_edges
            .iter()
            .filter(|edge| edge.kind == "blocks")
            .map(|edge| WorkPrerequisiteView {
                satisfied: !open_ids.contains(edge.depends_on_id.as_str())
                    && blocker_statuses
                        .get(&edge.depends_on_id)
                        .is_some_and(|status| status == "closed"),
                id: edge.depends_on_id.clone(),
            })
            .collect::<Vec<_>>();
        let parent_id = row_edges
            .iter()
            .find(|edge| edge.kind == "parent-child")
            .map(|edge| edge.depends_on_id.clone());
        let mut item = beads_item_view(
            row.id,
            row.title,
            &row.status,
            row.priority,
            row.issue_type,
            row.assignee,
            row.labels,
            row.updated_at,
            parent_id,
            prerequisites,
        );
        if coverage.dependency_rows_unread.contains(&item.id) && item.availability == "ready" {
            // Its edges were never read: readiness would be a guess.
            item.availability = "unknown".to_owned();
        }
        items.push(item);
    }
    let search = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    let mut notes = Vec::new();
    let skipped = skipped + coverage.rows_malformed;
    if skipped > 0 {
        notes.push(format!(
            "{skipped} Beads {} with malformed fields, ids or priorities were skipped",
            if skipped == 1 { "row" } else { "rows" }
        ));
    }
    if !coverage.dependency_rows_unread.is_empty() {
        let unread = coverage.dependency_rows_unread.len();
        notes.push(format!(
            "dependencies of {unread} {} were not fully read (bd's list receipt carried fewer dependency records than the row declares, or a record that could not be read); their availability is unknown and their parent is shown only where its record was read",
            if unread == 1 { "row" } else { "rows" }
        ));
    }
    if coverage.blockers_unread > 0 {
        notes.push(format!(
            "{} blocker {} could not be read (unknown, deleted, not a Beads identifier, or beyond the snapshot bound); they count as unsatisfied",
            coverage.blockers_unread,
            if coverage.blockers_unread == 1 { "status" } else { "statuses" }
        ));
    }
    let mut hint = (!notes.is_empty()).then(|| notes.join(". "));
    let mut items = items
        .into_iter()
        .filter(|item| {
            search.as_ref().is_none_or(|needle| {
                item.id.to_lowercase().contains(needle)
                    || item.title.to_lowercase().contains(needle)
            }) && query
                .availability
                .as_deref()
                .is_none_or(|wanted| item.availability == wanted)
        })
        .collect::<Vec<_>>();
    let total = items.len();
    let mut more = false;
    if items.len() > cap {
        // The cap is a display bound on the filtered snapshot; disclose it
        // instead of presenting the shown rows as the whole tracker.
        let omitted = items.len() - cap;
        items.truncate(cap);
        more = true;
        let capped = format!(
            "Beads shows the first {cap} matching open rows in source order; {omitted} more are not shown. Narrow the search, label or availability filter."
        );
        hint = Some(match hint {
            Some(existing) => format!("{existing}. {capped}"),
            None => capped,
        });
    }
    Ok(WorkPage {
        total,
        items,
        shown_before: 0,
        more,
        after: None,
        hint,
    })
}

fn read_beads_work_page(
    target: &BeadsReadTarget,
    query: &WorkListQuery,
    options: BeadsReadOptions,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<WorkPage, ApiError> {
    // One deadline covers the list and every blocker status batch of this
    // snapshot.
    read_beads_work_page_until(
        target,
        query,
        std::time::Instant::now() + options.timeout,
        options.reserve,
        cancelled,
    )
}

/// The list always runs and carries every row's edges; each blocker status
/// batch is launched only while `reserve` still fits before `deadline`,
/// otherwise it is disclosed as not read.
fn read_beads_work_page_until(
    target: &BeadsReadTarget,
    query: &WorkListQuery,
    deadline: std::time::Instant,
    reserve: Duration,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<WorkPage, ApiError> {
    // The caller may be gone by the time the permit was granted.
    work_read_not_abandoned(cancelled)?;
    let mut list_args = vec!["list".to_owned(), "--limit".to_owned(), "0".to_owned()];
    if let Some(label) = query
        .label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
    {
        // Keep source filtering in bd before the display cap. Labels returned
        // in list receipts also support independent loaded-row UI filtering.
        list_args.push(format!("--label={label}"));
    }
    let listed = run_beads_read_command(target, &list_args, deadline)?;
    let raw: Vec<Value> = beads_rows_from_value(listed, "bd list")?;
    let (rows, rows_malformed) = parse_beads_list_rows(raw)?;
    let mut edges: Vec<BeadsDependencyEdge> = Vec::new();
    let mut coverage = BeadsSnapshotCoverage {
        rows_malformed,
        ..BeadsSnapshotCoverage::default()
    };
    // bd 1.2.2 lists every edge of a row inline, so no per-row read follows.
    // A row whose records do not account for what it declares keeps the
    // records it has (a parent, a blocker) and is disclosed as unread: what
    // was read still shows, readiness is never guessed from a partial receipt.
    for row in &rows {
        let (row_edges, malformed) = beads_row_edges(row);
        if malformed || !beads_row_edges_complete(row, &row_edges) {
            coverage.dependency_rows_unread.insert(row.id.clone());
        }
        edges.extend(row_edges);
    }
    let open_ids = rows
        .iter()
        .map(|row| row.id.as_str())
        .collect::<HashSet<_>>();
    // A blocker reference that is not a Beads identifier (an external or
    // malformed reference) is never passed to bd: it is disclosed as unread
    // and stays unsatisfied like any other unread status, never dropped.
    let mut missing_blockers = Vec::new();
    let mut unsupported_blockers = HashSet::new();
    for edge in edges
        .iter()
        .filter(|edge| edge.kind == "blocks" && !open_ids.contains(edge.depends_on_id.as_str()))
    {
        if is_beads_identifier(&edge.depends_on_id) {
            missing_blockers.push(edge.depends_on_id.clone());
        } else {
            unsupported_blockers.insert(edge.depends_on_id.clone());
        }
    }
    missing_blockers.sort();
    missing_blockers.dedup();
    let blockers =
        read_beads_blocker_statuses(target, &missing_blockers, deadline, reserve, cancelled)?;
    coverage.blockers_unread = blockers.unread + unsupported_blockers.len();
    normalize_beads_work_page(
        rows,
        edges,
        &blockers.statuses,
        &coverage,
        query,
        BEADS_LIST_ROW_CAP,
    )
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BeadsDependencyView {
    id: String,
    title: String,
    status: String,
    priority: u8,
    #[serde(alias = "issue_type")]
    kind: String,
    #[serde(alias = "dependency_type")]
    dependency_type: String,
}

#[derive(Debug, Deserialize)]
struct BeadsShowReceipt {
    id: String,
    title: String,
    #[serde(default)]
    description: String,
    status: String,
    priority: u8,
    issue_type: String,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    updated_at: String,
    // Parsed one by one: a sparse relation record (an external reference)
    // is disclosed as unread instead of failing the whole drawer.
    #[serde(default)]
    dependencies: Vec<Value>,
    #[serde(default)]
    parent: Option<String>,
    // Required, like the list's: the number of relation records bd holds for
    // the issue. Verified on bd 1.2.2: `show` counts every relation type
    // (unlike `list`, whose count is `blocks` only), so it is reconciled
    // against all parsed records.
    dependency_count: usize,
    #[serde(default)]
    dependent_count: usize,
    #[serde(default)]
    comment_count: usize,
    #[serde(default)]
    labels: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BeadsCommentView {
    id: String,
    #[serde(alias = "issue_id", skip_serializing)]
    issue_id: String,
    #[serde(default)]
    author: Option<String>,
    text: String,
    #[serde(alias = "created_at")]
    created_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BeadsDetailResponse {
    item: WorkItemView,
    description: String,
    parent: Option<String>,
    dependencies: Vec<BeadsDependencyView>,
    /// Relation records the receipt declares but the host could not read
    /// (missing from the receipt, or present and unparseable); with any,
    /// readiness is unknown.
    dependencies_unread: usize,
    dependent_count: usize,
    comments: Vec<BeadsCommentView>,
    comment_count: usize,
    observed_at: String,
}

fn normalize_beads_detail(
    show: Value,
    comments: Value,
    issue_id: &str,
) -> Result<BeadsDetailResponse, ApiError> {
    let receipt: BeadsShowReceipt = match show {
        Value::Array(mut rows) if rows.len() == 1 => serde_json::from_value(rows.remove(0)),
        other => serde_json::from_value(other),
    }
    .map_err(|e| ApiError::bad_gateway(format!("bd show: invalid receipt: {e}")))?;
    // bd resolves prefixes and aliases; the drawer must never show another
    // issue under the requested id.
    if receipt.id != issue_id {
        return Err(ApiError::bad_gateway(format!(
            "bd show: receipt id {} does not match the requested {issue_id}",
            receipt.id
        )));
    }
    let comments: Vec<BeadsCommentView> = beads_rows_from_value(comments, "bd comments")?;
    if let Some(stray) = comments.iter().find(|comment| comment.issue_id != issue_id) {
        return Err(ApiError::bad_gateway(format!(
            "bd comments: receipt for {} does not match the requested {issue_id}",
            stray.issue_id
        )));
    }
    let mut dependencies: Vec<BeadsDependencyView> = Vec::with_capacity(receipt.dependencies.len());
    let mut dependencies_unread = 0usize;
    for record in receipt.dependencies {
        match serde_json::from_value(record) {
            Ok(dependency) => dependencies.push(dependency),
            Err(_) => dependencies_unread += 1,
        }
    }
    let prerequisites = dependencies
        .iter()
        .filter(|d| d.dependency_type == "blocks")
        .map(|d| WorkPrerequisiteView {
            id: d.id.clone(),
            satisfied: d.status == "closed",
        })
        .collect();
    if receipt.priority > 4 || !is_beads_identifier(&receipt.id) {
        return Err(ApiError::bad_gateway(
            "bd show: invalid issue id or priority",
        ));
    }
    // bd computes `parent` from the parent-child edge; a cleared parent must
    // not be recreated from the id spelling.
    let parent = receipt.parent.clone();
    // The list's rule, applied to the drawer: readiness is positive evidence
    // from a complete receipt. Every declared relation record parsed, and the
    // parent bd names has its record; otherwise `ready` becomes `unknown`.
    let parent_record = dependencies
        .iter()
        .find(|d| d.dependency_type == "parent-child")
        .map(|d| d.id.as_str());
    // Records the receipt declares but does not carry are unread too: the
    // drawer must never claim "no relations" for a relation it did not read.
    let dependencies_unread = dependencies_unread
        + receipt
            .dependency_count
            .saturating_sub(dependencies.len() + dependencies_unread);
    let complete = dependencies_unread == 0 && parent_record == parent.as_deref();
    let mut item = beads_item_view(
        receipt.id,
        receipt.title,
        &receipt.status,
        receipt.priority,
        receipt.issue_type,
        receipt.assignee,
        receipt.labels,
        receipt.updated_at,
        parent.clone(),
        prerequisites,
    );
    if !complete && item.availability == "ready" {
        item.availability = "unknown".to_owned();
    }
    Ok(BeadsDetailResponse {
        item,
        description: receipt.description,
        parent,
        dependencies,
        dependencies_unread,
        dependent_count: receipt.dependent_count,
        comment_count: receipt.comment_count.max(comments.len()),
        comments,
        observed_at: chrono::Utc::now().to_rfc3339(),
    })
}

impl AppState {
    #[cfg(test)]
    fn read_project_work_beads_detail(
        &self,
        project_id: &str,
        issue_id: &str,
    ) -> Result<BeadsDetailResponse, ApiError> {
        self.read_project_work_beads_detail_with_admission(project_id, issue_id, || Ok(()))
    }

    #[cfg(test)]
    fn read_project_work_beads_detail_with_admission<P>(
        &self,
        project_id: &str,
        issue_id: &str,
        admit: impl FnOnce() -> Result<P, ApiError>,
    ) -> Result<BeadsDetailResponse, ApiError> {
        self.read_project_work_beads_detail_with_options(
            project_id,
            issue_id,
            admit,
            BeadsReadOptions::for_tests(),
            &std::sync::atomic::AtomicBool::new(false),
        )
    }

    fn read_project_work_beads_detail_with_options<P>(
        &self,
        project_id: &str,
        issue_id: &str,
        admit: impl FnOnce() -> Result<P, ApiError>,
        options: BeadsReadOptions,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<BeadsDetailResponse, ApiError> {
        if !is_beads_identifier(issue_id) {
            return Err(ApiError::bad_request("Invalid Beads issue id"));
        }
        let project = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_project(project_id)
                .ok_or_else(|| ApiError::not_found("Work project not found"))?
                .clone()
        };
        if project.remote_id != LOCAL_REMOTE_ID {
            return Err(ApiError::conflict(
                "Remote project Work reads are not supported yet",
            ));
        }
        let target = beads_read_target(&project)
            .map_err(ApiError::conflict)?
            .ok_or_else(|| ApiError::conflict("No .beads directory in this project"))?;
        let _permit = admit()?;
        // The drawer may have been closed while this read waited for its
        // permit: nothing is launched on its behalf, before either command.
        work_read_not_abandoned(cancelled)?;
        // One deadline covers both detail commands.
        let deadline = std::time::Instant::now() + options.timeout;
        let show = match run_beads_read_process(
            &target,
            &["show".to_owned(), issue_id.to_owned()],
            deadline,
        )? {
            BeadsReadOutcome::Receipt(value) => value,
            // bd's own miss (an unknown or deleted id; a closed issue still
            // opens, `show` returns it) is not a broken CLI. The clause is bd
            // 1.2.2's exact wording, `Error fetching <id>: no issue found
            // matching "<id>"`, mirrored by the fixture; any other wording is
            // reported as a gateway failure.
            BeadsReadOutcome::Refused(refusal)
                if beads_refusal_is_unknown_ids(
                    &refusal.diagnostic,
                    std::slice::from_ref(&issue_id.to_owned()),
                ) =>
            {
                return Err(ApiError::not_found(format!(
                    "Beads issue {issue_id} not found"
                )));
            }
            BeadsReadOutcome::Refused(refusal) => {
                return Err(ApiError::bad_gateway(refusal.summary));
            }
        };
        work_read_not_abandoned(cancelled)?;
        let comments = run_beads_read_command(
            &target,
            &["comments".to_owned(), issue_id.to_owned()],
            deadline,
        )?;
        normalize_beads_detail(show, comments, issue_id)
    }
}

// Beads reads have their own admission: bd opens a Dolt store and may block
// far longer than an Engram read, so it must never consume Engram capacity
// and its wait is sized to its own snapshot deadline.
static BEADS_READ_PERMITS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

// Router-owned admission dependency, never supplied by HTTP query/body.
#[derive(Clone)]
struct BeadsReadLimiter(Arc<tokio::sync::Semaphore>);

async fn acquire_beads_read_permit_from(
    limiter: Arc<tokio::sync::Semaphore>,
    budget: Duration,
) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    // A permit is held for at most one snapshot/detail budget plus the
    // reader's teardown after a deadline; wait that long before reporting the
    // source busy. No process is spawned meanwhile.
    tokio::time::timeout(budget + BEADS_ADMISSION_MARGIN, limiter.acquire_owned())
        .await
        .map_err(|_| {
            ApiError::from_status(
                StatusCode::TOO_MANY_REQUESTS,
                "Beads reads busy; retry shortly",
            )
        })?
        .map_err(|_| ApiError::internal("Beads read limiter closed"))
}

async fn get_project_work_beads_detail(
    State(state): State<AppState>,
    AxumPath((project_id, issue_id)): AxumPath<(String, String)>,
    limiter: Option<axum::Extension<BeadsReadLimiter>>,
    options: Option<axum::Extension<BeadsReadOptions>>,
) -> Result<Json<BeadsDetailResponse>, ApiError> {
    let limiter = limiter.map_or_else(|| BEADS_READ_PERMITS.clone(), |limiter| limiter.0.0);
    let options = options.map_or_else(BeadsReadOptions::default, |options| options.0);
    let runtime = tokio::runtime::Handle::current();
    // A drawer the browser closed before the read started must not take a
    // Beads permit: dropping this future sets the flag admission checks.
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _abandoned = WorkReadAbandonGuard(cancelled.clone());
    run_blocking_api(move || {
        state
            .read_project_work_beads_detail_with_options(
                &project_id,
                &issue_id,
                || {
                    work_read_not_abandoned(&cancelled)?;
                    runtime.block_on(acquire_beads_read_permit_from(limiter, options.timeout))
                },
                options,
                &cancelled,
            )
            .map(Json)
    })
    .await
}
