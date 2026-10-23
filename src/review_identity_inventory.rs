// Recursive layered inventory assembly for review identity. Owns repository
// traversal and record selection, not Git execution or caller authority.
fn identity_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64)
        && oid
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn identity_relative(bytes: &[u8]) -> Result<String> {
    let path =
        std::str::from_utf8(bytes).map_err(|_| identity_refusal("path_encoding_unsupported"))?;
    if path.is_empty()
        || path.contains(['\\', ':', '\0'])
        || path
            .split('/')
            .any(|part| matches!(part, "" | "." | "..") || part.eq_ignore_ascii_case(".git"))
    {
        return Err(identity_refusal("unsafe_path"));
    }
    Ok(path.to_owned())
}
fn identity_join(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        path.to_owned()
    } else {
        format!("{prefix}/{path}")
    }
}
fn identity_tracked_key(path: &str) -> String {
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.to_owned()
    }
}

fn identity_index_records(bytes: &[u8]) -> Result<Vec<(&[u8], u32)>> {
    let mut records = Vec::new();
    let mut remaining = bytes;
    while !remaining.is_empty() {
        let end = remaining
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| identity_refusal("git_read_failed"))?;
        let header = &remaining[..end];
        remaining = &remaining[end + 1..];
        let marker = b"\tflags: ";
        let flags = remaining
            .windows(marker.len())
            .position(|part| part == marker)
            .filter(|position| *position < 2048)
            .ok_or_else(|| identity_refusal("git_read_failed"))?
            + marker.len();
        let end = remaining[flags..]
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| identity_refusal("git_read_failed"))?
            + flags;
        let flags = u32::from_str_radix(std::str::from_utf8(&remaining[flags..end])?, 16)?;
        records.push((header, flags));
        remaining = &remaining[end + 1..];
        if records.len() > REVIEW_IDENTITY_RECORD_LIMIT {
            return Err(identity_refusal("input_limit_exceeded"));
        }
    }
    Ok(records)
}
fn identity_repository(
    git: &ReviewIdentityGit,
    prefix: &str,
    depth: usize,
    capture: &mut ReviewIdentityCapture,
) -> Result<()> {
    if depth > 8 || capture.repositories >= 64 {
        return Err(identity_refusal("input_limit_exceeded"));
    }
    git.validate_location()?;
    for path in [&git.root, &git.gitdir] {
        if !capture
            .seen_repositories
            .insert(identity_physical_key(path)?)
        {
            return Err(identity_refusal("repository_cycle"));
        }
    }
    capture.repositories += 1;
    let head = git.run(&["rev-parse", "--verify", "--quiet", "HEAD"], true, None)?;
    let head = std::str::from_utf8(&head)?.trim();
    if !head.is_empty() && !identity_oid(head) {
        return Err(identity_refusal("missing_local_object"));
    }
    let mut repository = ReviewIdentityEntry::new(prefix.to_owned(), "repository", "repository");
    repository.oid = Some(if head.is_empty() {
        "UNBORN".to_owned()
    } else {
        head.to_owned()
    });
    repository.object_format = Some(git.object_format.clone());
    capture.add(repository)?;
    let index = git.run(
        &["ls-files", "--stage", "--sparse", "--debug", "-z"],
        false,
        None,
    )?;
    let tree = if head.is_empty() {
        Vec::new()
    } else {
        git.run(&["ls-tree", "-r", "-z", head], false, None)?
    };
    let mut entries = Vec::new();
    let mut files = BTreeSet::new();
    let mut links = BTreeSet::new();
    let mut objects = BTreeSet::new();
    let mut folded = HashMap::new();
    let mut tracked = HashMap::new();
    let head_records: Vec<_> = tree
        .split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        .map(|r| (r, 0))
        .collect();
    let index_records = identity_index_records(&index)?;
    for (layer, records) in [("head", head_records), ("index", index_records)] {
        for (record, flags) in records {
            let tab = record
                .iter()
                .position(|b| *b == b'\t')
                .ok_or_else(|| identity_refusal("git_read_failed"))?;
            let words: Vec<_> = std::str::from_utf8(&record[..tab])?.split(' ').collect();
            if words.len() != 3 {
                return Err(identity_refusal("git_read_failed"));
            }
            let (mode, oid, stage) = if layer == "head" {
                (words[0], words[2], None)
            } else {
                (words[0], words[1], Some(words[2].parse::<u8>()?))
            };
            if mode == "040000" {
                return Err(identity_refusal("sparse_index_unsupported"));
            }
            let path = identity_relative(&record[tab + 1..])?;
            if !matches!(mode, "100644" | "100755" | "120000" | "160000")
                || !identity_oid(oid)
                || oid.len() != if git.object_format == "sha1" { 40 } else { 64 }
                || stage.is_some_and(|s| s > 3)
            {
                return Err(identity_refusal("git_read_failed"));
            }
            if layer == "index" {
                tracked.insert(identity_tracked_key(&path), mode == "160000");
                if let Some(old) = folded.insert(path.to_lowercase(), path.clone())
                    && old != path
                {
                    let same_entry = identity_physical_key(&git.root.join(&old))
                        .ok()
                        .zip(identity_physical_key(&git.root.join(&path)).ok())
                        .is_some_and(|(a, b)| a == b);
                    if cfg!(windows)
                        || (same_entry
                            && !(identity_exact_spelling(git, &old)?
                                && identity_exact_spelling(git, &path)?))
                    {
                        return Err(identity_refusal("path_collision"));
                    }
                }
            }
            let mut entry = ReviewIdentityEntry::new(
                identity_join(prefix, &path),
                layer,
                if mode == "160000" { "gitlink" } else { "blob" },
            );
            entry.stage = stage;
            entry.mode = Some(mode.to_owned());
            entry.oid = Some(oid.to_owned());
            if layer == "index" {
                entry.flags = Some(format!("{flags:08x}"));
            }
            if mode == "160000" {
                links.insert(path);
            } else {
                files.insert(path);
                objects.insert(oid.to_owned());
            }
            entries.push(entry);
            if entries.len() > REVIEW_IDENTITY_RECORD_LIMIT {
                return Err(identity_refusal("input_limit_exceeded"));
            }
        }
    }
    let blobs = git.blobs(&objects)?;
    for mut entry in entries {
        if entry.kind == "blob" {
            let size = blobs
                .get(
                    entry
                        .oid
                        .as_ref()
                        .ok_or_else(|| identity_refusal("git_read_failed"))?,
                )
                .ok_or_else(|| identity_refusal("missing_local_object"))?;
            entry.size = Some(*size);
        }
        capture.add(entry)?;
    }
    // Repository-local exclusion rules affect which raw files participate.
    // Their own bytes are part of the inventory, even if they ignore themselves.
    for (tag, path) in [("info/exclude", git.common.join("info/exclude"))] {
        let mut entry = ReviewIdentityEntry::new(prefix.to_owned(), "policy", tag);
        if path.is_file() {
            let (size, hash, _) = identity_hash_file(&path, git.runner.deadline, capture)?;
            entry.size = Some(size);
            entry.content_sha256 = Some(hash);
        }
        capture.add(entry)?;
    }
    identity_discover_untracked(git, prefix, &tracked, &mut files, &mut links, capture)?;
    // Refuse an already-visible fanout before spending the shared deadline
    // entering every child. Recursive admission still enforces the bound if
    // a repository appears after this enumeration.
    let mut initialized = 0usize;
    for path in &links {
        git.check_deadline()?;
        let full = identity_safe_path(&git.root, path)?;
        let metadata = match fs::symlink_metadata(&full) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(identity_io_failure(&full, error)),
        };
        // A gitlink replaced by a symlink is not an initialized repository.
        // Refuse before looking for .git through the link's target.
        identity_plain_metadata(&metadata)?;
        if metadata.is_dir() && fs::symlink_metadata(full.join(".git")).is_ok() {
            initialized += 1;
        }
    }
    if capture.repositories + initialized > 64 {
        return Err(identity_refusal("input_limit_exceeded"));
    }
    for path in files {
        // HEAD may describe a file now replaced by an initialized repository.
        if links.contains(&path) {
            continue;
        }
        identity_worktree_entry(git, &path, prefix, capture)?;
    }
    for path in links {
        git.check_deadline()?;
        let full = identity_safe_path(&git.root, &path)?;
        match fs::symlink_metadata(&full) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                capture.add(ReviewIdentityEntry::new(
                    identity_join(prefix, &path),
                    "worktree",
                    "gitlink-uninitialized",
                ))?;
            }
            Err(e) => return Err(identity_io_failure(&full, e)),
            Ok(metadata) => {
                identity_plain_metadata(&metadata)?;
                if !metadata.is_dir() {
                    return Err(identity_refusal("uninitialized_submodule_contents"));
                }
                if fs::symlink_metadata(full.join(".git")).is_ok() {
                    let child = git.child(full)?;
                    capture.add(ReviewIdentityEntry::new(
                        identity_join(prefix, &path),
                        "worktree",
                        "directory-boundary",
                    ))?;
                    identity_repository(&child, &identity_join(prefix, &path), depth + 1, capture)?;
                } else if identity_read_dir(&full)?.next().is_none() {
                    capture.add(ReviewIdentityEntry::new(
                        identity_join(prefix, &path),
                        "worktree",
                        "gitlink-uninitialized",
                    ))?;
                } else {
                    return Err(identity_refusal("uninitialized_submodule_contents"));
                }
            }
        }
    }
    git.validate_location()?;
    Ok(())
}
