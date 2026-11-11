// Private evaluation source admission and immutable subprocess handoff.
// Identity remains the existing owner object; paths and summaries are not identities.

include!("engram_evaluation_checkpoint.rs");

const ACCEPTANCE_SOURCE_ID_MAX_BYTES: usize = 1_048_576;
const ACCEPTANCE_PRIVATE_PAGE_MAX_BYTES: usize = 2_113_536;
const ACCEPTANCE_PRIVATE_RESPONSE_MAX_BYTES: usize = ACCEPTANCE_PRIVATE_PAGE_MAX_BYTES + 26;
const ACCEPTANCE_VERIFICATION_FRAME_MAX_BYTES: usize = 2_162_688;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
enum AcceptanceSourceBasis {
    Legacy(AcceptanceLegacySourceBasis),
    Shared(AcceptanceSharedSourceBasis),
}

// The private verification reads return complete identities. Their workspace
// and root metadata remain separate from identity equality, as in Engram.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "AcceptanceVerificationSourceWire")]
struct AcceptanceVerificationSourceBasis {
    workspace_id: String,
    identity: AcceptanceSourceBasis,
    git_tree: Option<String>,
    source_root_generation: Option<i64>,
    source_root_state: Option<EngramSourceRootState>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceVerificationSourceWire {
    workspace_id: String,
    #[serde(default, deserialize_with = "deserialize_acceptance_present_revision")]
    source_revision: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_acceptance_optional_source_id"
    )]
    source_id: Option<EngramSourceId>,
    #[serde(default, deserialize_with = "deserialize_acceptance_present_revision")]
    git_tree: Option<String>,
    #[serde(default)]
    source_root_generation: Option<i64>,
    #[serde(default)]
    source_root_state: Option<EngramSourceRootState>,
}

fn deserialize_acceptance_present_revision<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<Option<String>, D::Error> {
    String::deserialize(decoder).map(Some)
}

impl TryFrom<AcceptanceVerificationSourceWire> for AcceptanceVerificationSourceBasis {
    type Error = &'static str;

    fn try_from(wire: AcceptanceVerificationSourceWire) -> std::result::Result<Self, Self::Error> {
        let identity = match (wire.source_revision, wire.source_id) {
            (Some(fingerprint), None) => {
                AcceptanceSourceBasis::Legacy(AcceptanceLegacySourceBasis {
                    workspace_id: Some(wire.workspace_id.clone()),
                    fingerprint,
                })
            }
            (None, Some(source_id)) if wire.git_tree.is_none() => {
                AcceptanceSourceBasis::Shared(AcceptanceSharedSourceBasis {
                    workspace_id: Some(wire.workspace_id.clone()),
                    source_id,
                })
            }
            _ => {
                return Err(
                    "verification source requires exactly one legacy revision or complete source_id",
                );
            }
        };
        if wire
            .git_tree
            .as_ref()
            .is_some_and(|tree| !acceptance_verification_tree_id(tree))
        {
            return Err("invalid historical verification tree");
        }
        Ok(Self {
            workspace_id: wire.workspace_id,
            identity,
            git_tree: wire.git_tree,
            source_root_generation: wire.source_root_generation,
            source_root_state: wire.source_root_state,
        })
    }
}

fn acceptance_verification_tree_id(tree: &str) -> bool {
    matches!(tree.len(), 40 | 64)
        && tree
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceVerificationApplicability {
    kind: String,
    tree: String,
}

fn deserialize_acceptance_verification_applicability<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<Option<AcceptanceVerificationApplicability>, D::Error> {
    let value = AcceptanceVerificationApplicability::deserialize(decoder)
        .map_err(|_| serde::de::Error::custom("invalid verification applicability"))?;
    if value.kind != "git_tree" || !acceptance_verification_tree_id(&value.tree) {
        return Err(serde::de::Error::custom(
            "invalid verification applicability",
        ));
    }
    Ok(Some(value))
}

fn acceptance_verification_basis_agrees(
    original: &AcceptanceVerificationSourceBasis,
    verification: &AcceptanceVerificationSourceBasis,
) -> bool {
    if matches!(original.identity, AcceptanceSourceBasis::Shared(_))
        || matches!(verification.identity, AcceptanceSourceBasis::Shared(_))
    {
        // Keep the existing shared binding's separate root/workspace guards.
        original == verification
    } else {
        original.identity.same_source(&verification.identity)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceLegacySourceBasis {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_acceptance_source_workspace"
    )]
    workspace_id: Option<String>,
    fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceSharedSourceBasis {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_acceptance_source_workspace"
    )]
    workspace_id: Option<String>,
    #[serde(deserialize_with = "deserialize_acceptance_source_id")]
    source_id: EngramSourceId,
}

fn deserialize_acceptance_source_id<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<EngramSourceId, D::Error> {
    let source = EngramSourceId::deserialize(decoder)
        .map_err(|_| serde::de::Error::custom("invalid evaluation source_id"))?;
    acceptance_source_bytes(&source).map_err(serde::de::Error::custom)?;
    Ok(source)
}

fn deserialize_acceptance_source_workspace<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<Option<String>, D::Error> {
    let workspace = String::deserialize(decoder)
        .map_err(|_| serde::de::Error::custom("invalid evaluation source workspace"))?;
    acceptance_source_workspace(&workspace)
        .map(Some)
        .ok_or_else(|| serde::de::Error::custom("invalid evaluation source workspace"))
}

fn deserialize_acceptance_optional_source_id<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<Option<EngramSourceId>, D::Error> {
    // `default` covers absence. A present null is not an undeclared identity.
    deserialize_acceptance_source_id(decoder).map(Some)
}

fn acceptance_source_bytes(source: &EngramSourceId) -> std::result::Result<Vec<u8>, &'static str> {
    source
        .validate(std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET)
        .map_err(|_| "invalid evaluation source_id")?;
    let bytes = serde_json::to_vec(source).map_err(|_| "cannot encode evaluation source_id")?;
    if bytes.len() > ACCEPTANCE_SOURCE_ID_MAX_BYTES {
        return Err("evaluation source_id exceeds the supported capacity");
    }
    Ok(bytes)
}

fn acceptance_attempt_source_label(
    source_id: Option<&EngramSourceId>,
    legacy: Option<&str>,
) -> String {
    if let Some(source) = source_id {
        format!(
            "source-id:{} ({} changes, {} submodules)",
            source.commit.get(..12).unwrap_or("unavailable"),
            source.changes.len(),
            source.submodules.len()
        )
    } else {
        acceptance_brief_text(legacy.unwrap_or("unmeasured"), 160)
    }
}

impl AcceptanceSourceBasis {
    fn same_source(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Legacy(a), Self::Legacy(b)) => a.fingerprint == b.fingerprint,
            (Self::Shared(a), Self::Shared(b)) => a.source_id == b.source_id,
            _ => false,
        }
    }
}

// The complete object and its immutable file locator are saved in the existing
// persisted attempt. Recovery reopens that same file and verifies its bytes;
// it never republishes the object. The payload digest excludes the pathname.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceSourcePayload {
    #[serde(deserialize_with = "deserialize_acceptance_source_id")]
    source_id: EngramSourceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_file: Option<PathBuf>,
}

struct AcceptanceSourceFile {
    lease: Arc<AcceptanceSourceFileLease>,
}

struct AcceptanceSourceFileLease {
    path: PathBuf,
    file: Mutex<Option<fs::File>>,
    terminal: AtomicBool,
}

// Every consumer in this host shares the same lease, including retries that
// reopen a persisted attempt. Terminal disposition cannot remove its file
// while another subprocess still holds a lease.
static ACCEPTANCE_SOURCE_FILE_LEASES: LazyLock<
    Mutex<HashMap<PathBuf, std::sync::Weak<AcceptanceSourceFileLease>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

impl AcceptanceSourcePayload {
    fn reopen(
        &self,
        private_directory: &FsPath,
    ) -> std::result::Result<AcceptanceSourceFile, ApiError> {
        let path = self.source_file.as_ref().ok_or_else(|| {
            ApiError::conflict("the persisted evaluation has no immutable source file")
        })?;
        let directory = fs::canonicalize(private_directory)
            .map_err(|_| ApiError::conflict("private evaluation source storage is unavailable"))?;
        let valid_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.strip_prefix("source-")
                    .and_then(|name| name.strip_suffix(".json"))
                    .is_some_and(|name| Uuid::parse_str(name).is_ok())
            });
        if !path.is_absolute() || path.parent() != Some(directory.as_path()) || !valid_name {
            return Err(ApiError::conflict(
                "the evaluation source locator is outside its private storage",
            ));
        }
        let lease = {
            let mut leases = ACCEPTANCE_SOURCE_FILE_LEASES
                .lock()
                .expect("evaluation source lease registry poisoned");
            leases.retain(|_, lease| lease.strong_count() > 0);
            if let Some(lease) = leases.get(path).and_then(std::sync::Weak::upgrade) {
                lease
            } else {
                let mut options = fs::OpenOptions::new();
                options.read(true);
                #[cfg(windows)]
                {
                    use std::os::windows::fs::OpenOptionsExt;
                    options.share_mode(1);
                }
                let file = options.open(path)
                    .map_err(|_| ApiError::conflict("the immutable evaluation source file is unavailable; recover the existing attempt before submitting"))?;
                let lease = Arc::new(AcceptanceSourceFileLease {
                    path: path.clone(),
                    file: Mutex::new(Some(file)),
                    terminal: AtomicBool::new(false),
                });
                leases.insert(path.clone(), Arc::downgrade(&lease));
                lease
            }
        };
        let guard = AcceptanceSourceFile { lease };
        guard.verify(&acceptance_source_bytes(&self.source_id).map_err(ApiError::conflict)?)?;
        Ok(guard)
    }

    fn publish(
        &self,
        private_directory: &FsPath,
    ) -> std::result::Result<AcceptanceSourceFile, ApiError> {
        let bytes = acceptance_source_bytes(&self.source_id).map_err(ApiError::conflict)?;
        if !private_directory.is_absolute() {
            return Err(ApiError::conflict(
                "evaluation source storage is not an absolute host directory",
            ));
        }
        fs::create_dir_all(private_directory)
            .map_err(|_| ApiError::internal("cannot create private evaluation source storage"))?;
        let directory = fs::canonicalize(private_directory)
            .map_err(|_| ApiError::internal("cannot resolve private evaluation source storage"))?;
        let path = directory.join(format!("source-{}.json", Uuid::new_v4()));
        let mut options = fs::OpenOptions::new();
        options.create_new(true).read(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Other readers are admitted; writing and deletion are denied
            // while this consumer owns the handle.
            options.share_mode(1);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .map_err(|_| ApiError::internal("cannot publish private evaluation source file"))?;
        let guard = AcceptanceSourceFile {
            lease: Arc::new(AcceptanceSourceFileLease {
                path,
                file: Mutex::new(Some(file)),
                terminal: AtomicBool::new(true),
            }),
        };
        {
            let mut handle = guard
                .lease
                .file
                .lock()
                .expect("evaluation source file mutex poisoned");
            let file = handle
                .as_mut()
                .ok_or_else(|| ApiError::internal("evaluation source handle is unavailable"))?;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| ApiError::internal("cannot persist private evaluation source file"))?;
        }
        #[cfg(windows)]
        guard.release_write_access()?;
        guard.verify(&bytes)?;
        // Pending must not become durable before the directory entry does.
        #[cfg(unix)]
        fs::File::open(&directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| {
                ApiError::internal("cannot persist private evaluation source directory")
            })?;
        #[cfg(unix)]
        if let Some(parent) = directory.parent() {
            fs::File::open(parent)
                .and_then(|parent| parent.sync_all())
                .map_err(|_| {
                    ApiError::internal("cannot persist private evaluation storage directory")
                })?;
        }
        guard.lease.terminal.store(false, Ordering::Release);
        let mut leases = ACCEPTANCE_SOURCE_FILE_LEASES
            .lock()
            .expect("evaluation source lease registry poisoned");
        leases.retain(|_, lease| lease.strong_count() > 0);
        leases.insert(guard.path().to_path_buf(), Arc::downgrade(&guard.lease));
        Ok(guard)
    }
}

impl AcceptanceSourceFile {
    #[cfg(windows)]
    fn release_write_access(&self) -> std::result::Result<(), ApiError> {
        use std::os::windows::fs::OpenOptionsExt;
        let mut handle = self
            .lease
            .file
            .lock()
            .expect("evaluation source file mutex poisoned");
        // This read-only bridge permits the existing writer, but denies
        // deletion throughout the swap. Once the writer closes, the final
        // reader also denies writes and admits the consumer's read-only share.
        let bridge = fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(self.path())
            .map_err(|_| ApiError::internal("cannot retain private evaluation source custody"))?;
        drop(handle.take());
        *handle = Some(
            fs::OpenOptions::new()
                .read(true)
                .share_mode(1)
                .open(self.path())
                .map_err(|_| ApiError::internal("cannot protect private evaluation source file"))?,
        );
        drop(bridge);
        // publish verifies the exact bytes again through this final handle.
        Ok(())
    }

    fn verify(&self, expected: &[u8]) -> std::result::Result<(), ApiError> {
        use std::io::{Read, Seek, SeekFrom};
        let mut handle = self
            .lease
            .file
            .lock()
            .expect("evaluation source file mutex poisoned");
        let file = handle
            .as_mut()
            .ok_or_else(|| ApiError::conflict("evaluation source handle is unavailable"))?;
        let path_metadata = fs::symlink_metadata(self.path())
            .map_err(|_| ApiError::conflict("evaluation source file is unavailable"))?;
        if !path_metadata.is_file() {
            return Err(ApiError::conflict("evaluation source file was replaced"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let opened = file
                .metadata()
                .map_err(|_| ApiError::conflict("cannot inspect evaluation source handle"))?;
            if opened.dev() != path_metadata.dev() || opened.ino() != path_metadata.ino() {
                return Err(ApiError::conflict("evaluation source file was replaced"));
            }
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|_| ApiError::conflict("cannot verify evaluation source file"))?;
        let mut actual = Vec::new();
        file.take((ACCEPTANCE_SOURCE_ID_MAX_BYTES + 1) as u64)
            .read_to_end(&mut actual)
            .map_err(|_| ApiError::conflict("cannot verify evaluation source file"))?;
        if actual != expected {
            return Err(ApiError::conflict(
                "evaluation source file changed before submission",
            ));
        }
        Ok(())
    }

    fn path(&self) -> &FsPath {
        &self.lease.path
    }

    #[cfg(test)]
    fn consumer(&self) -> Self {
        Self {
            lease: Arc::clone(&self.lease),
        }
    }

    // This only authorizes removal; each in-flight consumer still owns the
    // handle. Unknown outcomes do not call it and retain their saved locator.
    fn terminal(&self) {
        self.lease.terminal.store(true, Ordering::Release);
    }

    fn retain(&self) {
        self.lease.terminal.store(false, Ordering::Release);
    }
}

impl Drop for AcceptanceSourceFileLease {
    fn drop(&mut self) {
        self.file
            .get_mut()
            .expect("evaluation source file mutex poisoned")
            .take();
        if self.terminal.load(Ordering::Acquire) {
            if let Err(error) = fs::remove_file(&self.path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("evaluation source file cleanup failed: {error}");
                }
            }
        }
    }
}

impl AcceptanceEvaluationOpenWrite {
    fn digest(&self) -> std::result::Result<String, ApiError> {
        let Some(payload) = &self.source_payload else {
            // Preserve historical open-write hashes byte for byte.
            if self.args.iter().any(|arg| arg == "--source-id-file") {
                return Err(ApiError::conflict(
                    "shared evaluation command has no persisted source object",
                ));
            }
            return Ok(acceptance_evaluation_payload_digest(&self.args));
        };
        let source = acceptance_source_bytes(&payload.source_id).map_err(ApiError::conflict)?;
        let source_index = self
            .args
            .iter()
            .position(|arg| arg == "--source-id-file")
            .filter(|index| self.args.get(index + 1).is_some())
            .ok_or_else(|| {
                ApiError::conflict("shared evaluation payload has no source file argument")
            })?;
        if self
            .args
            .iter()
            .filter(|arg| arg.as_str() == "--source-id-file")
            .count()
            != 1
            || self.args.iter().any(|arg| arg == "--source-fingerprint")
        {
            return Err(ApiError::conflict(
                "evaluation payload has conflicting source arguments",
            ));
        }
        let mut canonical = self.args.clone();
        canonical[source_index + 1] = "<host-private-source-id>".into();
        let mut digest = Sha256::new();
        digest.update(b"termal-evaluation-source-id\0");
        digest.update(acceptance_evaluation_payload_digest(&canonical).as_bytes());
        digest.update([0]);
        digest.update(source);
        Ok(format!("{:x}", digest.finalize()))
    }

    fn source_file_args(
        &self,
        file: &AcceptanceSourceFile,
    ) -> std::result::Result<Vec<String>, ApiError> {
        self.digest()?;
        let payload = self
            .source_payload
            .as_ref()
            .ok_or_else(|| ApiError::conflict("evaluation has no persisted source payload"))?;
        file.verify(&acceptance_source_bytes(&payload.source_id).map_err(ApiError::conflict)?)?;
        let mut args = self.args.clone();
        let index = args
            .iter()
            .position(|arg| arg == "--source-id-file")
            .ok_or_else(|| ApiError::conflict("evaluation has no source file argument"))?;
        args[index + 1] = file
            .path()
            .to_str()
            .ok_or_else(|| ApiError::conflict("evaluation source file path is not UTF-8"))?
            .to_owned();
        Ok(args)
    }
}

fn acceptance_source_candidate(
    declared: &DelegationAcceptanceEvaluation,
    is_new: bool,
    private_directory: &FsPath,
) -> std::result::Result<
    (
        Option<AcceptanceSourcePayload>,
        Option<AcceptanceSourceFile>,
    ),
    ApiError,
> {
    let mut payload = declared
        .source_id
        .clone()
        .map(|source_id| AcceptanceSourcePayload {
            source_id,
            source_file: None,
        });
    let file = if is_new {
        payload
            .as_ref()
            .map(|payload| payload.publish(private_directory))
            .transpose()?
    } else {
        None
    };
    if let (Some(payload), Some(file)) = (&mut payload, &file) {
        payload.source_file = Some(file.path().to_path_buf());
        // Pending can become durable after an enqueued acknowledgement times
        // out. Its unique locator must survive that uncertain boundary.
        file.retain();
    }
    Ok((payload, file))
}

fn acceptance_source_replay_file(
    original: &AcceptanceEvaluationOpenWrite,
    digest: &str,
    mut file: Option<AcceptanceSourceFile>,
    private_directory: &FsPath,
) -> std::result::Result<Option<AcceptanceSourceFile>, ApiError> {
    match &original.source_payload {
        None => {
            if let Some(unused) = file.take() {
                unused.terminal();
            }
        }
        Some(payload) => {
            if original.digest()? != digest {
                return Err(ApiError::conflict(
                    "the persisted evaluation source payload does not match its original digest; nothing was submitted",
                ));
            }
            if file
                .as_ref()
                .is_none_or(|file| payload.source_file.as_deref() != Some(file.path()))
            {
                if let Some(unused) = file.take() {
                    unused.terminal();
                }
                file = Some(payload.reopen(private_directory)?);
            }
        }
    }
    Ok(file)
}

fn run_acceptance_source_payload(
    connection: &EngramConnectionConfig,
    original: &AcceptanceEvaluationOpenWrite,
    file: Option<&AcceptanceSourceFile>,
    run: &impl Fn(
        &EngramConnectionConfig,
        &[String],
        Duration,
    ) -> std::result::Result<EngramCliOutput, EngramTransportError>,
) -> AcceptanceEvaluationRunOutcome {
    let args =
        match (file, original.source_payload.is_some()) {
            (Some(file), true) => match original.source_file_args(file) {
                Ok(args) => args,
                Err(_) => return AcceptanceEvaluationRunOutcome::NeverStarted(
                    "the immutable evaluation source file failed verification before submission"
                        .into(),
                ),
            },
            (None, false) => original.args.clone(),
            _ => {
                return AcceptanceEvaluationRunOutcome::NeverStarted(
                    "the saved evaluation source payload has no matching file custody".into(),
                );
            }
        };
    if validate_acceptance_evaluation_command_size(connection, &args).is_err() {
        return AcceptanceEvaluationRunOutcome::NeverStarted(
            "the evaluation command including its private source path exceeds this host's command-line capacity".into());
    }
    classify_acceptance_evaluation_run(run(connection, &args, ENGRAM_WORK_BINDING_COMMAND_TIMEOUT))
}

impl AppState {
    fn acceptance_owner_judged_source(
        &self,
        owner: &str,
        place: &EngramBasisPlace,
        admission: Option<&AcceptanceCommittedInputAdmission>,
    ) -> Result<
        (
            Option<AcceptanceSourceBasis>,
            Option<String>,
            Option<String>,
        ),
        ApiError,
    > {
        let checkpoint_scope = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            evaluation_checkpoint_scope_locked(&inner, owner, place)
        };
        let Some(source_id) = self.acceptance_shared_source_capture(owner, place) else {
            if admission.is_some_and(|admission| admission.native_source.is_some()) {
                return Err(ApiError::conflict(
                    "the admitted native source could not be captured; no evaluator was started",
                ));
            }
            return Ok((None, None, None));
        };
        if admission.is_some_and(|admission| {
            admission.commit != source_id.commit
                || admission
                    .native_source
                    .as_ref()
                    .is_some_and(|expected| expected != &source_id)
        }) {
            return Err(ApiError::conflict(
                "the named source changed after committed-input admission; request a fresh evaluation",
            ));
        }
        let workspace = match place {
            EngramBasisPlace::Named { root, .. } => acceptance_source_workspace(root),
            EngramBasisPlace::Workdir(_) => None,
        };
        if let Some(scope) = checkpoint_scope {
            self.retain_evaluation_checkpoint(owner, place, &scope, &source_id);
        }
        let label = format!(
            "source-id:{} ({} changes, {} submodules)",
            &source_id.commit[..12],
            source_id.changes.len(),
            source_id.submodules.len()
        );
        Ok((
            Some(AcceptanceSourceBasis::Shared(AcceptanceSharedSourceBasis {
                workspace_id: workspace.clone(),
                source_id,
            })),
            Some(label),
            workspace,
        ))
    }

    fn acceptance_shared_source_capture(
        &self,
        owner: &str,
        place: &EngramBasisPlace,
    ) -> Option<EngramSourceId> {
        let budget = REVIEW_FREEZE_TIMEOUT.min(ENGRAM_SOURCE_COMMAND_BUDGET);
        let deadline = std::time::Instant::now() + budget;
        let (target, live) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            (
                Self::engram_source_root_target_locked(&inner, owner)
                    .ok()
                    .flatten()?,
                inner.acceptance_source_captures_live.clone(),
            )
        };
        let place = place.clone();
        let state = self.clone();
        bounded_source_capture(
            &live,
            &self.capture_lifetime,
            ACCEPTANCE_SOURCE_CAPTURE_LIMIT,
            budget,
            move || {
                let (root, common_dir_key) = match place {
                    EngramBasisPlace::Named {
                        root,
                        common_dir_key,
                    } => (root, common_dir_key),
                    EngramBasisPlace::Workdir(workdir) => {
                        if !fs::metadata(&workdir).is_ok_and(|metadata| metadata.is_dir()) {
                            return None;
                        }
                        let root =
                            fs::canonicalize(engram_worktree_root_path(FsPath::new(&workdir)))
                                .ok()?;
                        let common = engram_source_root_common_dir(&root)?;
                        (root.to_str()?.to_owned(), engram_exact_path_key(&common))
                    }
                };
                state
                    .engram_naming_source_basis_until(&target, &root, &common_dir_key, deadline)
                    .ok()
                    .and_then(|basis| basis.source_id)
            },
        )
    }
}

impl DelegationAcceptanceEvaluation {
    fn private_source_basis(&self) -> Option<AcceptanceSourceBasis> {
        match (&self.source_id, &self.source_fingerprint) {
            (Some(source_id), None) => {
                Some(AcceptanceSourceBasis::Shared(AcceptanceSharedSourceBasis {
                    workspace_id: None,
                    source_id: source_id.clone(),
                }))
            }
            (None, Some(fingerprint)) => {
                Some(AcceptanceSourceBasis::Legacy(AcceptanceLegacySourceBasis {
                    workspace_id: None,
                    fingerprint: fingerprint.clone(),
                }))
            }
            _ => None,
        }
    }
}

fn acceptance_private_control_read(request: &EngramControlRequest) -> bool {
    matches!(
        request,
        EngramControlRequest::AcceptanceBindingRead { .. }
            | EngramControlRequest::AcceptanceVerificationRead { .. }
    )
}

fn engram_control_request_frame_limit(request: &EngramControlRequest) -> usize {
    match request {
        EngramControlRequest::ExecutionObserve { .. } => ENGRAM_OBSERVATION_MAX_REQUEST_BYTES,
        EngramControlRequest::VerificationRecord { .. } => ACCEPTANCE_VERIFICATION_FRAME_MAX_BYTES,
        _ => ENGRAM_CONTROL_MAX_FRAME_BYTES,
    }
}

fn encode_engram_control_request(
    request: &EngramControlRequest,
) -> std::result::Result<Vec<u8>, EngramTransportError> {
    let encoded = serde_json::to_vec(request).map_err(|_| {
        EngramTransportError::protocol("failed encoding Engram request").request_never_sent()
    })?;
    // exchange_engram_control_frame_with_limit writes exactly one LF.
    let framed_len = encoded.len().saturating_add(usize::from(matches!(
        request,
        EngramControlRequest::VerificationRecord { .. }
    )));
    if framed_len > engram_control_request_frame_limit(request) {
        return Err(EngramTransportError::protocol(
            "Engram request exceeds the maximum control frame",
        )
        .request_never_sent());
    }
    Ok(encoded)
}

fn engram_control_response_frame_limit(request: &EngramControlRequest) -> usize {
    if acceptance_private_control_read(request) {
        ACCEPTANCE_PRIVATE_RESPONSE_MAX_BYTES
    } else {
        ENGRAM_CONTROL_MAX_FRAME_BYTES
    }
}
