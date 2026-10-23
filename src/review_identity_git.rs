// Pinned plumbing and recursive repository enumeration for raw identities.
// Does not invoke diff/status/submodule porcelain, filters or transports.
fn identity_git_path(path: &FsPath) -> Result<String> {
    let value = identity_utf8_path(path)?;
    #[cfg(windows)]
    {
        // Git for Windows does not accept Rust's canonical verbatim spelling
        // in --git-dir/--work-tree. Keep that spelling for native FS identity;
        // translate only this argv boundary, refusing Win32 aliases.
        for component in path.components() {
            if let std::path::Component::Normal(name) = component {
                let name = name
                    .to_str()
                    .ok_or_else(|| identity_refusal("path_encoding_unsupported"))?;
                let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
                if name.ends_with(['.', ' '])
                    || matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                    || ["COM", "LPT"].iter().any(|prefix| {
                        base.strip_prefix(*prefix).is_some_and(|suffix| {
                            matches!(
                                suffix,
                                "1" | "2"
                                    | "3"
                                    | "4"
                                    | "5"
                                    | "6"
                                    | "7"
                                    | "8"
                                    | "9"
                                    | "¹"
                                    | "²"
                                    | "³"
                            )
                        })
                    })
                {
                    return Err(identity_refusal("path_encoding_unsupported"));
                }
            }
        }
        let ordinary = match path.components().next() {
            Some(std::path::Component::Prefix(prefix)) => match prefix.kind() {
                std::path::Prefix::VerbatimDisk(_) => value[4..].to_owned(),
                std::path::Prefix::VerbatimUNC(_, _) => format!("\\\\{}", &value[8..]),
                std::path::Prefix::Verbatim(_) | std::path::Prefix::DeviceNS(_) => {
                    return Err(identity_refusal("path_encoding_unsupported"));
                }
                _ => value,
            },
            _ => value,
        };
        if identity_canonical(FsPath::new(&ordinary))? != identity_canonical(path)? {
            return Err(identity_refusal("path_encoding_unsupported"));
        }
        return Ok(ordinary);
    }
    #[cfg(not(windows))]
    Ok(value)
}

// Each point of the git metadata walk where a concurrent writer can remove an
// entry the walk is about to read: before a queued directory's link check,
// before it is listed, and before a listed entry is inspected. Tests remove
// entries there.
#[cfg(not(test))]
fn identity_metadata_walk_point(_: &FsPath) {}
#[cfg(test)]
thread_local! { static IDENTITY_METADATA_WALK_HOOK: std::cell::RefCell<Option<Box<dyn FnMut(&FsPath)>>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
fn identity_metadata_walk_point(path: &FsPath) {
    IDENTITY_METADATA_WALK_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(path);
        }
    });
}

struct ReviewIdentityGit {
    root: PathBuf,
    gitdir: PathBuf,
    common: PathBuf,
    object_format: String,
    runner: ReviewFreezeGit,
}
impl ReviewIdentityGit {
    #[cfg(test)]
    fn new(root: &FsPath) -> Result<Self> {
        Self::new_until(root, std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT)
    }
    fn new_until(root: &FsPath, deadline: std::time::Instant) -> Result<Self> {
        // A review root that cannot be resolved (a workdir not created yet)
        // names itself before the shared Git runner resolves it again. An IO
        // failure inside that runner's own setup names the review root.
        identity_canonical(root)?;
        let runner = identity_at(root, ReviewFreezeGit::new_with_deadline(root, Some(deadline)))
            .map_err(|error| {
                if format!("{error:#}").contains("configured Git filters") {
                    identity_refusal("configured_filter_unsupported")
                } else {
                    error
                }
            })?;
        let gitdir = runner.run(&["rev-parse", "--absolute-git-dir"], false)?;
        let gitdir = identity_canonical(FsPath::new(std::str::from_utf8(&gitdir)?.trim()))?;
        let common = runner.run(
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            false,
        )?;
        let common = identity_canonical(FsPath::new(std::str::from_utf8(&common)?.trim()))?;
        let format = runner.run(&["rev-parse", "--show-object-format"], false)?;
        let object_format = identity_object_format(&format)?;
        let result = Self {
            root: runner.root.clone(),
            gitdir,
            common,
            object_format,
            runner,
        };
        result.admit()?;
        Ok(result)
    }
    fn child(&self, root: PathBuf) -> Result<Self> {
        let dotgit = root.join(".git");
        let meta = identity_metadata(&dotgit)?;
        identity_plain_metadata(&meta)?;
        let gitdir = if meta.is_dir() {
            identity_canonical(&dotgit)?
        } else if meta.is_file() {
            let bytes = identity_read_file(&dotgit, 4096)?.0;
            let pointer = std::str::from_utf8(&bytes)
                .map_err(|_| identity_refusal("path_encoding_unsupported"))?
                .trim()
                .strip_prefix("gitdir: ")
                .ok_or_else(|| identity_refusal("git_metadata_escape"))?;
            let lexical = root.join(pointer);
            identity_no_links(&lexical)?;
            let target = identity_canonical(&lexical)?;
            let modules = self.gitdir.join("modules");
            if !target.starts_with(&modules) {
                return Err(identity_refusal("git_metadata_escape"));
            }
            target
        } else {
            return Err(identity_refusal("git_metadata_escape"));
        };
        let runner = ReviewFreezeGit {
            root: root.clone(),
            binary: self.runner.binary.clone(),
            deadline: self.runner.deadline,
        };
        let mut child = Self {
            root,
            gitdir: gitdir.clone(),
            common: gitdir,
            object_format: String::new(),
            runner,
        };
        let common = child.run(
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            false,
            None,
        )?;
        child.common = identity_canonical(FsPath::new(std::str::from_utf8(&common)?.trim()))?;
        child.object_format = identity_object_format(&child.run(
            &["rev-parse", "--show-object-format"],
            false,
            None,
        )?)?;
        if child.common != child.gitdir {
            return Err(identity_refusal("git_metadata_escape"));
        }
        child.admit()?;
        Ok(child)
    }
    fn check_deadline(&self) -> Result<()> {
        if std::time::Instant::now() >= self.runner.deadline {
            return Err(identity_refusal("deadline_exceeded"));
        }
        Ok(())
    }
    fn admit(&self) -> Result<()> {
        self.check_deadline()?;
        identity_no_links(&self.gitdir)?;
        identity_no_links(&self.common)?;
        // Include files can redirect configuration reads outside the admitted
        // metadata tree. v1 deliberately refuses them instead of interpreting
        // a repository-controlled include graph.
        if !self
            .run(
                &[
                    "config",
                    "--no-includes",
                    "--null",
                    "--get-regexp",
                    r"^(include\.path|includeif\..*\.path)$",
                ],
                true,
                None,
            )?
            .is_empty()
        {
            return Err(identity_refusal("git_config_include_unsupported"));
        }
        // A main checkout's own/common directories are identical. Validate
        // that metadata tree once per observation, rather than scanning every
        // loose object twice; linked worktrees still validate both directories.
        for directory in [&self.gitdir, &self.common]
            .into_iter()
            .collect::<BTreeSet<_>>()
        {
            let alternates = directory.join("objects/info/alternates");
            if fs::symlink_metadata(&alternates).is_ok() {
                return Err(identity_refusal("alternates_unsupported"));
            }
            for path in [
                directory.join("objects"),
                directory.join("config"),
                directory.join("info/exclude"),
                directory.join("index"),
                directory.join("HEAD"),
                directory.join("commondir"),
                directory.join("packed-refs"),
            ] {
                if fs::symlink_metadata(&path).is_ok() {
                    identity_no_links(&path)?;
                }
            }
            for name in ["objects", "refs"] {
                let path = directory.join(name);
                if path.is_dir() {
                    self.metadata_tree(&path)?;
                }
            }
        }
        if !self
            .run(
                &[
                    "config",
                    "--null",
                    "--get-regexp",
                    r"^filter\..*\.(clean|smudge|process)$",
                ],
                true,
                None,
            )?
            .is_empty()
        {
            return Err(identity_refusal("configured_filter_unsupported"));
        }
        Ok(())
    }
    fn metadata_tree(&self, root: &FsPath) -> Result<()> {
        let mut pending = vec![root.to_path_buf()];
        let mut queued_bytes = root.as_os_str().len();
        let mut count = 0usize;
        while let Some(directory) = pending.pop() {
            queued_bytes -= directory.as_os_str().len();
            self.check_deadline()?;
            // Every Git process of every worktree sharing these trees creates
            // and removes temporary entries in them (objects/**/tmp_obj_*,
            // objects/incoming-*, refs/**/*.lock). One removed after the walk
            // listed or queued it no longer exists: it cannot be a link or an
            // escape, so the walk skips it. The walked tree itself must still
            // exist, or every queued directory would vanish with it. Any other
            // failure names the path it could not read.
            let queued = directory.as_path() != root;
            let root_stands = || fs::symlink_metadata(root).is_ok();
            identity_metadata_walk_point(&directory);
            match identity_no_links(&directory) {
                Err(error) if queued && identity_vanished(&error) && root_stands() => continue,
                result => result?,
            }
            identity_metadata_walk_point(&directory);
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) if queued && identity_gone(&error) && root_stands() => continue,
                Err(error) => return Err(identity_io_failure(&directory, error)),
            };
            for entry in entries {
                self.check_deadline()?;
                count += 1;
                if count > REVIEW_IDENTITY_RECORD_LIMIT {
                    return Err(identity_refusal("input_limit_exceeded"));
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) if identity_gone(&error) && root_stands() => continue,
                    Err(error) => return Err(identity_io_failure(&directory, error)),
                };
                let path = entry.path();
                identity_metadata_walk_point(&path);
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if identity_gone(&error) && root_stands() => continue,
                    Err(error) => return Err(identity_io_failure(&path, error)),
                };
                identity_plain_metadata(&metadata)?;
                if metadata.is_dir() {
                    queued_bytes += path.as_os_str().len();
                    if queued_bytes > REVIEW_IDENTITY_INVENTORY_LIMIT {
                        return Err(identity_refusal("input_limit_exceeded"));
                    }
                    pending.push(path);
                } else if !metadata.is_file() {
                    return Err(identity_refusal("git_metadata_escape"));
                }
            }
        }
        Ok(())
    }
    fn validate_location(&self) -> Result<()> {
        if identity_object_format(&self.run(
            &["rev-parse", "--show-object-format"],
            false,
            None,
        )?)? != self.object_format
        {
            return Err(identity_refusal("concurrent_change"));
        }
        identity_no_links(&self.root)?;
        let dotgit = self.root.join(".git");
        let metadata = identity_metadata(&dotgit)?;
        identity_plain_metadata(&metadata)?;
        let actual = if metadata.is_file() {
            let bytes = identity_read_file(&dotgit, 4096)?.0;
            let pointer = std::str::from_utf8(&bytes)?
                .trim()
                .strip_prefix("gitdir: ")
                .ok_or_else(|| identity_refusal("git_metadata_escape"))?;
            let path = self.root.join(pointer);
            identity_no_links(&path)?;
            identity_canonical(&path)?
        } else {
            identity_canonical(&dotgit)?
        };
        if actual != self.gitdir {
            return Err(identity_refusal("concurrent_change"));
        }
        let common = self.run(
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            false,
            None,
        )?;
        if identity_canonical(FsPath::new(std::str::from_utf8(&common)?.trim()))? != self.common {
            return Err(identity_refusal("concurrent_change"));
        }
        self.admit()
    }
    fn run(&self, args: &[&str], absent: bool, input: Option<Vec<u8>>) -> Result<Vec<u8>> {
        self.check_deadline()?;
        let mut command = self.runner.command(&[]);
        command
            .arg("--no-replace-objects")
            .arg(format!("--git-dir={}", identity_git_path(&self.gitdir)?))
            .arg(format!("--work-tree={}", identity_git_path(&self.root)?))
            .args(args);
        let output = if let Some(input) = input {
            run_bounded_read_input(
                &mut command,
                self.runner.deadline,
                if args.first() == Some(&"check-ignore") {
                    REVIEW_IDENTITY_INVENTORY_LIMIT
                } else {
                    REVIEW_IDENTITY_PAYLOAD_LIMIT + 1024 * 1024
                },
                input,
            )?
        } else {
            run_bounded_read_command(
                &mut command,
                self.runner.deadline,
                REVIEW_FREEZE_OUTPUT_LIMIT,
                false,
            )?
        };
        if !output.status.success() && !(absent && output.status.code() == Some(1)) {
            return Err(identity_refusal(
                if matches!(args.first(), Some(&"cat-file") | Some(&"ls-tree")) {
                    "missing_local_object"
                } else {
                    "git_read_failed"
                },
            ));
        }
        Ok(output.stdout)
    }
    fn blobs(&self, oids: &BTreeSet<String>) -> Result<HashMap<String, u64>> {
        if oids.is_empty() {
            return Ok(HashMap::new());
        }
        let input = oids
            .iter()
            .map(|oid| format!("{oid}\n"))
            .collect::<String>()
            .into_bytes();
        let sizes = self.run(&["cat-file", "--batch-check"], false, Some(input))?;
        let mut result = HashMap::new();
        for line in std::str::from_utf8(&sizes)?.lines() {
            let words: Vec<_> = line.split(' ').collect();
            if words.len() != 3 || words[1] != "blob" || !identity_oid(words[0]) {
                return Err(identity_refusal("missing_local_object"));
            }
            let size: usize = words[2].parse()?;
            if size > REVIEW_IDENTITY_PAYLOAD_LIMIT {
                return Err(identity_refusal("input_limit_exceeded"));
            }
            if !oids.contains(words[0]) || result.insert(words[0].to_owned(), size as u64).is_some()
            {
                return Err(identity_refusal("missing_local_object"));
            }
        }
        if result.len() != oids.len() {
            return Err(identity_refusal("missing_local_object"));
        }
        Ok(result)
    }
}

fn identity_object_format(bytes: &[u8]) -> Result<String> {
    match std::str::from_utf8(bytes)?.trim() {
        format @ ("sha1" | "sha256") => Ok(format.to_owned()),
        _ => Err(identity_refusal("object_format_unsupported")),
    }
}
