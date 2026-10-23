// Host-owned no-follow untracked discovery. Git classifies ignore rules only;
// it never enumerates directories. No project helper or policy code executes.
struct ReviewIdentityDirectory {
    relative: String,
    metadata: fs::Metadata,
    key: String,
}
struct ReviewIdentityCandidate {
    relative: Vec<u8>,
    full: PathBuf,
    metadata: fs::Metadata,
}

fn identity_ignore_matches(requests: &[Vec<u8>], output: &[u8]) -> Result<HashSet<Vec<u8>>> {
    if requests.is_empty() {
        if output.is_empty() {
            return Ok(HashSet::new());
        }
        return Err(identity_refusal("git_read_failed"));
    }
    let body = output
        .strip_suffix(&[0])
        .ok_or_else(|| identity_refusal("git_read_failed"))?;
    let fields: Vec<_> = body.split(|byte| *byte == 0).collect();
    if fields.len() != requests.len() * 4 {
        return Err(identity_refusal("git_read_failed"));
    }
    let mut remaining: HashSet<Vec<u8>> = requests.iter().cloned().collect();
    if remaining.len() != requests.len() {
        return Err(identity_refusal("git_read_failed"));
    }
    let mut ignored = HashSet::new();
    for record in fields.chunks_exact(4) {
        if !remaining.remove(record[3]) {
            return Err(identity_refusal("git_read_failed"));
        }
        if record[0].is_empty() {
            if !record[1].is_empty() || !record[2].is_empty() {
                return Err(identity_refusal("git_read_failed"));
            }
        } else {
            if record[1].is_empty()
                || !record[1].iter().all(u8::is_ascii_digit)
                || record[2].is_empty()
            {
                return Err(identity_refusal("git_read_failed"));
            }
            if !record[2].starts_with(b"!") {
                ignored.insert(record[3].to_vec());
            }
        }
    }
    if !remaining.is_empty() {
        return Err(identity_refusal("git_read_failed"));
    }
    Ok(ignored)
}

fn identity_ignored_candidates(
    git: &ReviewIdentityGit,
    paths: &[Vec<u8>],
) -> Result<HashSet<Vec<u8>>> {
    if paths.is_empty() {
        return Ok(HashSet::new());
    }
    let mut requests = Vec::with_capacity(paths.len());
    let mut input = Vec::new();
    for path in paths {
        // check-ignore parses pathspec magic even on stdin. Keep the
        // candidate literal without changing its directory-pattern meaning.
        let length = path
            .len()
            .checked_add(3)
            .and_then(|length| input.len().checked_add(length))
            .filter(|length| *length <= REVIEW_IDENTITY_INVENTORY_LIMIT)
            .ok_or_else(|| identity_refusal("input_limit_exceeded"))?;
        let mut request = b"./".to_vec();
        request.extend_from_slice(path);
        input.extend_from_slice(&request);
        input.push(0);
        debug_assert_eq!(input.len(), length);
        requests.push(request);
    }
    let ignored = identity_ignore_matches(
        &requests,
        &git.run(
            &[
                "check-ignore",
                "-z",
                "--stdin",
                "--non-matching",
                "-v",
                "--no-index",
            ],
            true,
            Some(input),
        )?,
    )?;
    // The strict decoder admitted only exact, unique query echoes. Map those
    // echoes back to raw relative names before any path or kind admission.
    Ok(paths
        .iter()
        .zip(requests)
        .filter(|(_, request)| ignored.contains(request))
        .map(|(path, _)| path.clone())
        .collect())
}

fn identity_discovery_metadata(path: &FsPath) -> Result<fs::Metadata> {
    fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            identity_refusal("concurrent_change")
        } else {
            identity_io_failure(path, error)
        }
    })
}

fn identity_discovery_directory(
    git: &ReviewIdentityGit,
    directory: &ReviewIdentityDirectory,
) -> Result<PathBuf> {
    git.check_deadline()?;
    let full = git.root.join(&directory.relative);
    let metadata = identity_discovery_metadata(&full)?;
    if !metadata.is_dir()
        || !review_freeze_same_entry(&directory.metadata, &metadata)
        || identity_physical_key(&full)? != directory.key
    {
        return Err(identity_refusal("concurrent_change"));
    }
    identity_plain_metadata(&metadata)?;
    Ok(full)
}

fn identity_discovery_name(name: &std::ffi::OsStr) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(name.as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    {
        name.to_str()
            .map(|name| name.as_bytes().to_vec())
            .ok_or_else(|| identity_refusal("path_encoding_unsupported"))
    }
}

fn identity_discovery_policy(
    git: &ReviewIdentityGit,
    prefix: &str,
    directory: &str,
    capture: &mut ReviewIdentityCapture,
) -> Result<()> {
    let relative = identity_join(directory, ".gitignore");
    let full = identity_safe_path(&git.root, &relative)?;
    match fs::symlink_metadata(&full) {
        Ok(metadata) => {
            identity_plain_metadata(&metadata)?;
            if metadata.is_file() {
                let (size, hash, _) = identity_hash_file(&full, git.runner.deadline, capture)?;
                let mut entry =
                    ReviewIdentityEntry::new(identity_join(prefix, &relative), "policy", "ignore");
                entry.size = Some(size);
                entry.content_sha256 = Some(hash);
                capture.add(entry)?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(identity_io_failure(&full, error)),
    }
    Ok(())
}

fn identity_discover_untracked(
    git: &ReviewIdentityGit,
    prefix: &str,
    tracked: &HashMap<String, bool>,
    files: &mut BTreeSet<String>,
    links: &mut BTreeSet<String>,
    capture: &mut ReviewIdentityCapture,
) -> Result<()> {
    let mut level = vec![ReviewIdentityDirectory {
        relative: String::new(),
        metadata: identity_discovery_metadata(&git.root)?,
        key: identity_physical_key(&git.root)?,
    }];
    while !level.is_empty() {
        let mut candidates = Vec::new();
        let mut candidate_bytes = 0usize;
        for directory in &level {
            let full = identity_discovery_directory(git, directory)?;
            // Validate/hash the admitted ancestor's policy before Git can read it.
            identity_discovery_policy(git, prefix, &directory.relative, capture)?;
            for entry in identity_read_dir(&full)? {
                git.check_deadline()?;
                capture.visit()?;
                let entry = entry.map_err(|error| identity_io_failure(&full, error))?;
                let name = entry.file_name();
                let metadata = identity_discovery_metadata(&entry.path())?;
                if directory.relative.is_empty()
                    && name.to_str().is_some_and(|name| {
                        if cfg!(windows) {
                            name.eq_ignore_ascii_case(".git")
                        } else {
                            name == ".git"
                        }
                    })
                {
                    continue;
                }
                let mut relative = directory.relative.as_bytes().to_vec();
                if !relative.is_empty() {
                    relative.push(b'/');
                }
                relative.extend(identity_discovery_name(&name)?);
                let tracked_kind = std::str::from_utf8(&relative)
                    .ok()
                    .and_then(|path| tracked.get(&identity_tracked_key(path)));
                // A tracked blob replaced by a plain directory still needs
                // untracked child discovery. Gitlinks are repository boundaries.
                if tracked_kind.is_some_and(|gitlink| {
                    *gitlink || !metadata.is_dir() || metadata.file_type().is_symlink()
                }) {
                    continue;
                }
                candidate_bytes += relative.len() + 1;
                if candidate_bytes > REVIEW_IDENTITY_INVENTORY_LIMIT {
                    return Err(identity_refusal("input_limit_exceeded"));
                }
                candidates.push(ReviewIdentityCandidate {
                    relative,
                    full: entry.path(),
                    metadata,
                });
            }
            identity_discovery_directory(git, directory)?;
        }
        let paths: Vec<_> = candidates
            .iter()
            // A trailing slash can match a child-only ignore pattern and
            // incorrectly prune a directory with re-included descendants.
            .map(|candidate| candidate.relative.clone())
            .collect();
        let ignored = identity_ignored_candidates(git, &paths)?;
        let mut next = Vec::new();
        let mut next_bytes = 0usize;
        for candidate in candidates {
            git.check_deadline()?;
            if ignored.contains(&candidate.relative) {
                continue;
            }
            let path = identity_relative(&candidate.relative)?;
            let metadata = identity_discovery_metadata(&candidate.full)?;
            if !review_freeze_same_entry(&candidate.metadata, &metadata) {
                return Err(identity_refusal("concurrent_change"));
            }
            #[cfg(windows)]
            let symlink = identity_reparse_kind(&candidate.full, &metadata)? == Some("symlink");
            #[cfg(not(windows))]
            let symlink = metadata.file_type().is_symlink();
            if symlink {
                files.insert(path);
            } else if metadata.is_dir() {
                identity_plain_metadata(&metadata)?;
                if fs::symlink_metadata(candidate.full.join(".git")).is_ok() {
                    links.insert(path);
                } else {
                    next_bytes += path.len();
                    if next_bytes > REVIEW_IDENTITY_INVENTORY_LIMIT {
                        return Err(identity_refusal("input_limit_exceeded"));
                    }
                    next.push(ReviewIdentityDirectory {
                        relative: path,
                        key: identity_physical_key(&candidate.full)?,
                        metadata,
                    });
                }
            } else if metadata.is_file() {
                files.insert(path);
            } else {
                return Err(identity_refusal("special_file"));
            }
        }
        level = next;
    }
    Ok(())
}
