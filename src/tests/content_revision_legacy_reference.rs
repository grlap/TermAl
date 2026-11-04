// Pre-change ordinary capture and parent rule, copied unchanged except function names.
// It shares unchanged framing, byte conversion and checked IO helpers with production.
fn legacy_capture_content_revision(git: &ReviewFreezeGit) -> Result<String> {
    // `-z` lists paths verbatim: no quoting, and a newline is part of a path
    // rather than a separator. A merge lists a path once per stage. The staged
    // listing carries each index entry's mode, which is read only to find a
    // submodule (160000).
    let started = std::time::Instant::now();
    let staged = git.run(&["ls-files", "-z", "--stage"], false)?;
    for entry in staged.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        // A submodule's content lives in another repository, which this
        // revision does not read: a checkout, an edit or a deinit inside it
        // would leave the revision equal, a false "unchanged". So it fails
        // closed, as the freeze does.
        if entry.starts_with(b"160000 ") {
            bail!("a content revision does not cover submodules (Git gitlinks are unsupported)");
        }
    }
    // Every path, tracked or untracked and not ignored, with its line-ending
    // attributes: "i/<eol> w/<eol> attr/<attributes>\t<path>".
    let listed = git.run(
        &[
            "ls-files",
            "-z",
            "--eol",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
        false,
    )?;
    let mut paths = Vec::new();
    for entry in listed.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let tab = entry
            .iter()
            .position(|b| *b == b'\t')
            .context("malformed eol listing")?;
        paths.push((
            &entry[tab + 1..],
            content_revision_keeps_bytes(&entry[..tab]),
        ));
    }
    paths.sort_unstable_by(|left, right| left.0.cmp(right.0));
    paths.dedup_by(|left, right| left.0 == right.0);
    if paths.len() > CONTENT_REVISION_PATH_LIMIT {
        bail!("too many paths for a content revision");
    }
    let mut hash = Sha256::new();
    review_freeze_chunk(&mut hash, "schema", CONTENT_REVISION_SCHEME.as_bytes());
    let mut total = 0usize;
    let mut entries = 0usize;
    let mut verified_parents = std::collections::HashSet::new();
    for (bytes, keeps_bytes) in paths {
        check_capture_cancellation()?;
        if std::time::Instant::now() >= git.deadline {
            bail!("content revision deadline exceeded");
        }
        let path = std::str::from_utf8(bytes).context("non-UTF8 path in a content revision")?;
        if path.contains('\n') {
            bail!("a path with a newline in a content revision");
        }
        // A path that is not there (its file or a directory above it gone,
        // or a directory above it now a file) has no entry: a tracked file
        // deleted in the worktree counts as if its deletion were committed.
        // Every other failure fails the capture, a tracked directory renamed
        // in case only among them.
        let present = match legacy_content_revision_path(&git.root, path, &mut verified_parents)? {
            Some(full) => match fs::symlink_metadata(&full) {
                Ok(metadata) => Some((full, metadata)),
                Err(error) if content_revision_io_not_found(&error) => None,
                Err(error) => return Err(error.into()),
            },
            None => None,
        };
        let Some((full, metadata)) = present else {
            // A tracked .gitattributes missing from the worktree makes Git
            // take that file's attributes from the index, so staging its
            // deletion alone would move which files count byte for byte.
            // Fail closed rather than let the index decide.
            if path == ".gitattributes" || path.ends_with("/.gitattributes") {
                bail!(
                    "the tracked {path} is missing from the worktree, so its attributes \
                     would come from the index"
                );
            }
            continue;
        };
        if metadata.is_dir() {
            // A tracked file replaced by a directory: the path is no longer a
            // file, and the directory's own files are listed on their own.
            continue;
        }
        entries += 1;
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(&full)?;
            let target = target.to_str().context("non-UTF8 symlink target")?;
            review_freeze_chunk(
                &mut hash,
                &format!("entry:{path}:120000"),
                target.as_bytes(),
            );
            if !review_freeze_same_entry(&metadata, &fs::symlink_metadata(&full)?) {
                bail!("symlink changed while it was read");
            }
        } else {
            let (content, read) = review_freeze_file_versioned(&full, REVIEW_FREEZE_OUTPUT_LIMIT)?;
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                read.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = {
                let _ = &read;
                false
            };
            total += content.len();
            if total > CONTENT_REVISION_BYTE_BUDGET {
                bail!("content revision byte budget exceeded");
            }
            let mode = if executable { "100755" } else { "100644" };
            // A file with an explicit line-ending contract, and an executable
            // one, whose interpreter line a CR would break, count byte for
            // byte: rewriting their line endings is a real change.
            let content = if keeps_bytes || executable {
                content
            } else {
                content_revision_line_endings(content)
            };
            review_freeze_chunk(&mut hash, &format!("entry:{path}:{mode}"), &content);
        }
    }
    // A read cannot be interrupted, so the last one may end past the budget;
    // a revision finished after it is not returned.
    if std::time::Instant::now() >= git.deadline {
        bail!("content revision deadline exceeded");
    }
    // Every file is read at each capture, with no cache, so a slow one is
    // logged with what it cost where it ran.
    let elapsed = started.elapsed();
    if elapsed >= CONTENT_REVISION_SLOW_CAPTURE {
        eprintln!(
            "engram> content revision of {}: {entries} entries, {total} bytes read in {elapsed:?}",
            git.root.display()
        );
    }
    Ok(format!("{CONTENT_REVISION_SCHEME}:{:x}", hash.finalize()))
}

fn legacy_content_revision_path(
    root: &FsPath,
    path: &str,
    verified: &mut std::collections::HashSet<PathBuf>,
) -> Result<Option<PathBuf>> {
    if path.ends_with('/') {
        bail!("a content revision does not cover the untracked nested repository {path:?}");
    }
    if path.is_empty()
        || (cfg!(windows) && (path.contains('\\') || path.contains(':')))
        || FsPath::new(path).is_absolute()
        || path
            .split('/')
            .any(|part| part.is_empty() || part == ".." || part == ".")
    {
        bail!("a content revision refuses the unsafe path {path:?}");
    }
    let full = root.join(path);
    let parent = full
        .parent()
        .context("a content revision path has no parent")?
        .to_path_buf();
    if !verified.contains(&parent) {
        match fs::canonicalize(&parent) {
            Ok(canonical) if canonical == parent => {
                verified.insert(parent);
            }
            Ok(_) => bail!(
                "a directory above {path:?} leads elsewhere or differs in case from the listing"
            ),
            Err(error) if content_revision_io_not_found(&error) => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Some(full))
}

