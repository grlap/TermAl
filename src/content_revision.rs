// Owns the content revision TermAl reports to Engram as a source basis
// ("content-v1"): a fingerprint of the files present in a worktree and
// nothing else, so a commit, a `git add` or a fast-forward to the same
// content leaves it where it was, and a peer worktree holding the same content
// has the same revision.
// Does not own the review-freeze fingerprint (review_freeze.rs), which still
// hashes HEAD, the index and both diffs for review freezes, nor when a basis
// is taken (engram_turn_observations.rs, engram_turn_checks.rs,
// acceptance_evaluation_api.rs).
// New file. Reuses review_freeze.rs's pinned Git runner, its filter refusal
// and its checked file read; the path rule (`content_revision_path`) is its
// own, since it covers tracked paths too.

/// The scheme every content revision starts with. A revision taken by another
/// algorithm never equals one of these, so a mix is visible in the string.
const CONTENT_REVISION_SCHEME: &str = "content-v1";

/// At most this many listed paths, tracked and untracked together; a larger
/// worktree gets no revision.
const CONTENT_REVISION_PATH_LIMIT: usize = 200_000;

/// At most this many bytes are read in total, as the freeze bounds its
/// untracked input.
const CONTENT_REVISION_BYTE_BUDGET: usize = 512 * 1024 * 1024;

/// A capture that takes at least this long is logged with its size.
const CONTENT_REVISION_SLOW_CAPTURE: Duration = Duration::from_secs(2);

/// The content revision of the worktree at `root`, which must be a worktree
/// root, with the canonical root it was taken on. Under the freeze's pinned
/// Git configuration and its shared [`REVIEW_FREEZE_TIMEOUT`] budget.
///
/// What it covers: every path Git lists as tracked or as untracked and not
/// ignored, as it is in the worktree now. Each present file counts with its
/// content and its mode (the executable bit on Unix, `100644` on Windows,
/// which has none); a symlink with its link text and mode `120000`. A path
/// that is not there has no entry, exactly as if its deletion were
/// committed; so has a tracked file now replaced by a directory, whose own
/// files are listed as untracked. HEAD takes no part, and the index none in
/// content or mode: committing, staging or `update-index --chmod` do not move
/// it. The index decides only which ignored paths are listed: a force-added
/// ignored file counts, and `git rm --cached` of it removes it.
///
/// Line endings: a file with no line-ending contract counts with each CRLF
/// read as LF exactly when Git's `core.autocrlf` would convert it on the way
/// in, so Git rewriting it with the other line ending (checkout, reset,
/// stash) does not move it. A file with a contract counts byte for byte: one
/// whose `.gitattributes` set `eol=` or unset `text` (`binary` does), and on
/// Unix an executable one, whose interpreter line a CR would break. For
/// those, a line-ending rewrite is a real change. Git also leaves a file
/// alone when the index's copy holds a CR; the revision does not consult the
/// index for that. The price that remains: a file with no contract rewritten
/// from LF to CRLF keeps its revision. See [`content_revision_line_endings`].
///
/// Fails, leaving the caller without a basis, on a repository with a
/// submodule (what is inside it would not be covered, so a change there
/// would read as none), on a repository that configures Git filters, on an
/// untracked nested repository, on one path that is not UTF-8 or not safe to
/// join, when a listed file changes while it is read or cannot be read for
/// any reason but absence, and when a bound is exceeded, the deadline
/// included: a revision finished after it is not returned. One such path
/// costs the whole tree its basis; an obligation opened without a basis can
/// only be waived.
fn content_revision(root: &FsPath) -> Result<(PathBuf, String)> {
    // `ReviewFreezeGit::new` also refuses a repository that configures Git
    // filters. Nothing here runs a filter (ls-files runs none), so that
    // refusal is kept for uniformity with the freeze, not for safety.
    let git = ReviewFreezeGit::new(root)?;
    let revision = capture_content_revision(&git)?;
    Ok((git.root.clone(), revision))
}

fn capture_content_revision(git: &ReviewFreezeGit) -> Result<String> {
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
        let present = match content_revision_path(&git.root, path, &mut verified_parents)? {
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

/// `path`, as Git lists it, joined to the worktree `root`; `None` when a
/// directory above it is gone or is now a file. The lexical rules are the
/// freeze's (`review_freeze_untracked_path`), except that `:` and `\` are
/// refused only on Windows, where they would change what the path names; on
/// Unix a tracked file may carry either in its name. A directory above the
/// path must canonicalise to itself: no link leads it elsewhere, and its case
/// matches the listing. Each directory is checked once per capture, in
/// `verified`.
fn content_revision_path(
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

fn content_revision_io_not_found(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// Runs `capture` on its own thread and waits at most `budget` for it. A file
/// read cannot be interrupted, so a capture that overruns is left to finish
/// and its result dropped; its thread lives until its read returns. `live`
/// counts the threads alive, healthy ones included, and no thread starts
/// while it is at `limit`, so a worktree whose reads stall costs at most
/// `limit` threads however often it is asked. `None` when the capture fails,
/// overruns or panics, is refused for the limit, or its thread cannot start.
fn bounded_content_revision_capture<T: Send + 'static>(
    live: &Arc<std::sync::atomic::AtomicUsize>,
    limit: usize,
    budget: Duration,
    capture: impl FnOnce() -> Option<T> + Send + 'static,
) -> Option<T> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    /// Lowers the count when the thread ends, on a panic too.
    struct Release(Arc<AtomicUsize>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    if live
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
            (count < limit).then_some(count + 1)
        })
        .is_err()
    {
        eprintln!("engram> content revision not taken: {limit} captures still reading");
        return None;
    }
    // Owned by the worker from here; dropped with the closure if the thread
    // cannot start.
    let release = Release(live.clone());
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("termal-content-revision".to_owned())
        .spawn(move || {
            let _release = release;
            let _ = sender.send(capture());
        });
    // A thread the system refuses leaves the revision not taken, never a
    // panicked caller.
    if let Err(error) = spawned {
        eprintln!("engram> no thread for a content revision: {error}");
        return None;
    }
    receiver.recv_timeout(budget).ok().flatten()
}

/// Whether the `.gitattributes` a `ls-files --eol` entry reports give the
/// file an explicit line-ending contract: `eol=` names its line ending, and
/// `-text` (which `binary` sets) says it is not text. Such a file counts byte
/// for byte, never with CRLF read as LF.
fn content_revision_keeps_bytes(eol_info: &[u8]) -> bool {
    let Some(start) = eol_info.windows(5).position(|window| window == b"attr/") else {
        return false;
    };
    let attributes = String::from_utf8_lossy(&eol_info[start + 5..]);
    attributes
        .split_whitespace()
        .any(|attribute| attribute == "-text" || attribute.starts_with("eol="))
}

/// The content as Git's `core.autocrlf` would take it in: each CRLF read as
/// LF when Git takes the file for text and it holds a CRLF, else unchanged.
/// Git's rule (`gather_stats` and `convert_is_binary` in its convert.c), over
/// the whole file: the file is binary when it holds a lone CR, or a NUL, or
/// when its printable bytes divided by 128 are fewer than its non-printable
/// ones. Printable are bytes from 32 up except DEL, and BS, HT, ESC and FF;
/// non-printable are the other bytes below 32 (NUL included) and DEL, where a
/// final ^Z does not count. CR and LF count as neither. Git also declines a
/// file whose index copy holds a CR; that rule consults the index and is left
/// out. The tests compare this with Git's own conversion.
fn content_revision_line_endings(content: Vec<u8>) -> Vec<u8> {
    let (mut crlf, mut lone_cr, mut nul) = (0usize, 0usize, 0usize);
    let (mut printable, mut non_printable) = (0usize, 0usize);
    let mut index = 0;
    while index < content.len() {
        let byte = content[index];
        index += 1;
        match byte {
            b'\r' if content.get(index) == Some(&b'\n') => {
                crlf += 1;
                index += 1;
            }
            b'\r' => lone_cr += 1,
            b'\n' => {}
            0x7f => non_printable += 1,
            0x08 | b'\t' | 0x1b | 0x0c => printable += 1,
            0 => {
                nul += 1;
                non_printable += 1;
            }
            byte if byte < 32 => non_printable += 1,
            _ => printable += 1,
        }
    }
    if content.last() == Some(&0x1a) {
        non_printable -= 1;
    }
    let binary = lone_cr > 0 || nul > 0 || (printable >> 7) < non_printable;
    if binary || crlf == 0 {
        return content;
    }
    // Text with no lone CR: every CR belongs to a CRLF.
    content.into_iter().filter(|byte| *byte != b'\r').collect()
}
