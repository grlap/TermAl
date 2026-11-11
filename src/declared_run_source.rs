// Owns native S0 source/declaration/artifact preimages and source endpoints.
// Uses the pinned producer-owned source_id; it never derives a file/index
// inventory or measures local policy metadata separately from source_id.
// Measurements run off StateInner locks within the caller's deadline.

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct DeclaredRunSource {
    root: PathBuf,
    source_id: EngramSourceId,
    source_owner: EngramSourceOwnerPin,
    measured_at: String,
}

// Process-local opening evidence. It is published only before the prompt is
// handed to the provider, and is never recovered as authority after restart.
#[derive(Clone, Debug)]
struct DeclaredRunSnapshot {
    source: DeclaredRunSource,
    git_dir: PathBuf,
    entries: Vec<DeclaredRunOpeningEntry>,
}

#[derive(Clone, Debug)]
struct DeclaredRunOpeningEntry {
    binding: EngramDeclaredBinding,
    directory_key: String,
}

fn declared_run_snapshot_with_owner(root: &FsPath, pin: &EngramSourceOwnerPin, deadline: std::time::Instant,
    timings: Option<Arc<SourceReadTimings>>,
    source: impl FnOnce(&EngramSourceOwnerPin, &FsPath, std::time::Instant) -> Result<EngramSourceId>,
) -> Result<DeclaredRunSnapshot> {
    let preimages = timings.as_ref().map(|timings| timings.begin("opening_preimages"));
    let root = fs::canonicalize(root)?;
    let EngramDeclaration::Loaded { sha256, bytes, entries } = engram_load_declaration(&root) else {
        anyhow::bail!("no usable native test declaration");
    };
    let mut captured = Vec::new();
    for entry in entries {
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("native opening snapshot deadline elapsed");
        }
        // Every declared command uses its artifact, including commands the
        // legacy console recognizer also knows. Matching never rereads S0.
        let started = std::time::SystemTime::now();
        let artifact_start = engram_artifact_start_image(&root, &entry.artifact);
        let artifact_canonical = engram_exact_path_key(&engram_canonical_path(&root.join(&entry.artifact)));
        let directory_key = engram_exact_path_key(&fs::canonicalize(root.join(&entry.cwd))?);
        captured.push(DeclaredRunOpeningEntry {
            binding: EngramDeclaredBinding {
                root: root.clone(), environment_missing: entry.env.first().cloned(),
                entry, declaration_sha256: sha256.clone(),
                declaration_bytes: bytes, artifact_start, artifact_canonical, started,
            },
            directory_key,
        });
    }
    let first = captured.first().ok_or_else(|| anyhow!("no native declared entries"))?;
    drop(preimages);
    let source = declared_run_source_with_owner(&first.binding, pin, deadline, timings.clone(), source)?;
    if std::time::Instant::now() >= deadline {
        anyhow::bail!("native opening snapshot deadline elapsed");
    }
    // Reuse the run owner's filesystem locator; source was already captured
    // above. Resolving metadata does not need another Git/source capture.
    let _directory = timings.as_ref().map(|timings| timings.begin("run_directory"));
    let runs = EngramHost::run_artifact_directory(&root).context("native Git run directory unavailable")?;
    let git_dir = fs::canonicalize(runs.parent().context("native Git directory unavailable")?)?;
    Ok(DeclaredRunSnapshot { source, git_dir, entries: captured })
}

impl DeclaredRunSnapshot {
    fn match_command(&self, ran: &str, cwd: &FsPath) -> Option<(EngramCheckCommand, EngramDeclaredBinding)> {
        let (command, words) = engram_declared_candidate(ran)?;
        let directory = command.directory.as_deref().map_or(cwd, FsPath::new);
        let directory_key = engram_exact_path_key(&fs::canonicalize(directory).ok()?);
        let mut entries = self.entries.iter().filter(|entry| {
            entry.directory_key == directory_key
                && engram_declared_words(&entry.binding.entry.command).ok().as_ref() == Some(&words)
        });
        let (Some(entry), None) = (entries.next(), entries.next()) else { return None; };
        Some((command, entry.binding.clone()))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum DeclaredRunFirstTool {
    #[default]
    Waiting,
    Command(String),
    Ineligible,
}

impl DeclaredRunFirstTool {
    fn observe(&mut self, key: Option<&str>, is_command: bool) {
        let Some(key) = key.filter(|key| !key.is_empty()) else {
            *self = Self::Ineligible;
            return;
        };
        let identity = sha256_hex(key.as_bytes());
        match self {
            Self::Waiting if is_command => *self = Self::Command(identity),
            Self::Command(original) if is_command && original == &identity => {}
            _ => *self = Self::Ineligible,
        }
    }

    fn accepts(&self, key: &str) -> bool {
        matches!(self, Self::Command(original) if *original == sha256_hex(key.as_bytes()))
    }
}

fn declared_run_source(
    binding: &EngramDeclaredBinding,
    pin: &EngramSourceOwnerPin,
    deadline: impl Into<EngramSourcePhasePlan>,
) -> Result<DeclaredRunSource> {
    declared_run_source_with_timings(binding, pin, deadline, None)
}

fn declared_run_source_with_timings(binding: &EngramDeclaredBinding, pin: &EngramSourceOwnerPin, deadline: impl Into<EngramSourcePhasePlan>,
    timings: Option<Arc<SourceReadTimings>>) -> Result<DeclaredRunSource> {
    let plan = deadline.into();
    declared_run_source_with_owner(binding, pin, plan.completion_deadline, timings,
        |pin, root, _| engram_source_id_until(pin, root, plan))
}

fn declared_run_source_with_owner(binding: &EngramDeclaredBinding, pin: &EngramSourceOwnerPin, deadline: std::time::Instant,
    timings: Option<Arc<SourceReadTimings>>,
    source: impl FnOnce(&EngramSourceOwnerPin, &FsPath, std::time::Instant) -> Result<EngramSourceId>,
) -> Result<DeclaredRunSource> {
    let source_capture = timings.as_ref().map(|timings| timings.begin("source_owner_capture"));
    let source_id = source(pin, &binding.root, deadline)?;
    drop(source_capture);
    // This checks only declaration-path membership, not source cleanliness.
    // The producer's full object is the sole source-state measurement.
    let git = ReviewFreezeGit::new_with_deadline_and_timings(&binding.root, Some(deadline), timings)?;
    // An ignored/untracked declaration would otherwise be invisible to status.
    // Require this one committed path, rather than listing index entries.
    // The owner's identity owns line-ending normalization; the worktree
    // declaration's bytes can legitimately differ from its committed blob.
    let object = format!("{}:{ENGRAM_DECLARATION_FILE}", source_id.commit);
    git.run(&["cat-file", "-e", &object], false)
        .context("declaration_not_committed: the native declaration must be a committed path")?;
    if !engram_declaration_unchanged(binding) {
        anyhow::bail!("declared execution requires its unchanged committed declaration");
    }
    Ok(DeclaredRunSource { root: git.root, source_id, source_owner: pin.clone(), measured_at: test_run_timestamp() })
}
