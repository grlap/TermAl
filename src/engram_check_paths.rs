// Where a command ran and where it may write, as TermAl can tell without the
// runtime's help (Engram w-108a13d58018 criterion 2, tm-winf step 2). Owns
// path keys and resolution, worktree roots and their short-lived cache, the
// tracking of a shell that keeps a `cd` between commands, network-path
// guards, and the judgement of which worktree a recognised check tested.
// Does not own command recognition (`engram_check_recognition.rs`), overlap
// marking and the recorder hooks (`engram_turn_checks.rs`), or the toolchain
// label (`engram_check_toolchain.rs`). Split out of `engram_turn_checks.rs`.

/// `path` as paths of one file system compare in: forward slashes, no
/// Windows verbatim prefix, no trailing separator, and case-folded where the
/// platform's usual file system ignores case (Windows and macOS), which errs
/// towards two paths being the same. That suits overlap, where treating two
/// places as one only makes a check unknown; what a check is credited to is
/// judged on `engram_exact_path_key` instead.
fn engram_path_key(path: &FsPath) -> String {
    let text = engram_exact_path_key(path);
    if cfg!(any(windows, target_os = "macos")) {
        text.to_lowercase()
    } else {
        text
    }
}

/// `engram_path_key` without the case folding: on a case-sensitive volume
/// (which macOS and Windows allow) two worktrees may differ only in case,
/// and crediting one with the other's test would be wrong. Resolved paths
/// carry the case the file system stores, so the same place compares equal.
fn engram_exact_path_key(path: &FsPath) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = match text.strip_prefix("//?/UNC/") {
        Some(share) => format!("//{share}"),
        None => text.strip_prefix("//?/").unwrap_or(&text).to_owned(),
    };
    let trimmed = text.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The key (`engram_path_key`) of `path` with every link on its existing part
/// resolved (`engram_canonical_path`).
fn engram_canonical_path_key(path: &FsPath) -> String {
    engram_path_key(&engram_canonical_path(path))
}

/// `path` with every link on its existing part resolved: the longest leading
/// part that exists is resolved, and the rest, which cannot hold a link, is
/// appended with its `.` and `..` steps resolved by name. So a test selector
/// or a glob under a link (`linked/test_x.py::test`, `linked/test_*.py`)
/// resolves through the link, and a directory spelled through an alias
/// (`/var` for `/private/var`, a Windows short name) is its resolved form.
fn engram_canonical_path(path: &FsPath) -> PathBuf {
    let components = path.components().collect::<Vec<_>>();
    let append = |mut resolved: PathBuf, rest: &[std::path::Component]| {
        for component in rest {
            match component {
                std::path::Component::ParentDir => {
                    resolved.pop();
                }
                std::path::Component::CurDir => {}
                other => resolved.push(other),
            }
        }
        resolved
    };
    (1..=components.len())
        .rev()
        .find_map(|existing| {
            let prefix = components[..existing].iter().collect::<PathBuf>();
            std::fs::canonicalize(prefix)
                .ok()
                .map(|resolved| append(resolved, &components[existing..]))
        })
        .unwrap_or_else(|| append(PathBuf::new(), &components))
}

/// Whether the path with key `path` is `root` or lies beneath it.
fn engram_path_within(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || root.ends_with('/'))
}

/// The worktree `workdir` lies in: the nearest ancestor of its resolved path
/// holding a `.git` entry, itself resolved, or the resolved path outside a
/// worktree. Sessions in one worktree share its content whichever
/// subdirectory each works in, however their paths are spelled, and even
/// when a workdir does not exist (yet): the root is resolved on its own.
fn engram_worktree_root_path(workdir: &FsPath) -> PathBuf {
    let path = std::fs::canonicalize(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    let root = path
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .unwrap_or(&path);
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

/// How long a worktree key is remembered, and how many are.
const ENGRAM_WORKTREE_KEY_TTL: Duration = Duration::from_secs(30);
const ENGRAM_WORKTREE_KEY_CACHE_LIMIT: usize = 256;

/// Worktree keys (`engram_worktree_root`) by workdir, with when each was
/// resolved.
fn engram_worktree_keys() -> &'static Mutex<HashMap<PathBuf, (String, std::time::Instant)>> {
    static KEYS: std::sync::OnceLock<Mutex<HashMap<PathBuf, (String, std::time::Instant)>>> =
        std::sync::OnceLock::new();
    KEYS.get_or_init(Default::default)
}

/// The key (`engram_path_key`) of the worktree `workdir` lies in
/// (`engram_worktree_root_path`), resolved on the file system, so never under
/// the state lock, and remembered for a short while so a busy session does
/// not walk the file system for every command. Nothing depends on this cache
/// keeping a key: a session's own key is kept on its record
/// (`workdir_worktree`), refreshed whenever it reports a command or an edit.
/// A worktree created or removed around a workdir is seen once the remembered
/// key expires. A network path
/// (`engram_network_path`) is keyed as written, unresolved: resolving it can
/// block for a network timeout on the runtime's event reader, and no check
/// runs there (`engram_check_worktree` takes such a path to lead out).
fn engram_worktree_root(workdir: &FsPath) -> String {
    let keys = engram_worktree_keys();
    let now = std::time::Instant::now();
    if let Some((key, remembered_at)) = keys
        .lock()
        .expect("Engram worktree key cache mutex poisoned")
        .get(workdir)
        && now.duration_since(*remembered_at) < ENGRAM_WORKTREE_KEY_TTL
    {
        return key.clone();
    }
    let key = if engram_network_path(&workdir.to_string_lossy()) {
        engram_path_key(workdir)
    } else {
        engram_path_key(&engram_worktree_root_path(workdir))
    };
    let mut keys = keys
        .lock()
        .expect("Engram worktree key cache mutex poisoned");
    if keys.len() >= ENGRAM_WORKTREE_KEY_CACHE_LIMIT {
        keys.retain(|_, (_, remembered_at)| {
            now.duration_since(*remembered_at) < ENGRAM_WORKTREE_KEY_TTL
        });
        if keys.len() >= ENGRAM_WORKTREE_KEY_CACHE_LIMIT {
            keys.clear();
        }
    }
    keys.insert(workdir.to_path_buf(), (key.clone(), now));
    key
}

/// Where the shell of one runtime is presumed to be, if it keeps a `cd`
/// between commands (`engram_shell_move`). A runtime other than `runtime`
/// starts its shell afresh in the workdir.
#[derive(Clone, Debug)]
struct EngramShellDirectory {
    runtime: Option<RuntimeToken>,
    /// The presumed directory, or `None` once a command moved the shell
    /// where TermAl cannot follow.
    directory: Option<String>,
    /// A move a running command makes, by its key: where its `cd` leads, or
    /// `None` when that cannot be resolved. It settles when the command ends
    /// (`engram_settle_shell_move`): a denied command never ran, and one
    /// that failed may have stopped before or after its `cd`. At most one is
    /// pending; a second loses the shell.
    pending: Option<(String, Option<String>)>,
}

impl EngramShellDirectory {
    /// The record of `record`'s current runtime, which starts in the workdir
    /// when the runtime has none yet.
    fn current(record: &mut SessionRecord) -> &mut Self {
        let runtime = record.runtime.runtime_token();
        let workdir = record.session.workdir.clone();
        let shell = record.engram.shell_directory.get_or_insert_with(|| Self {
            runtime: runtime.clone(),
            directory: Some(workdir.clone()),
            pending: None,
        });
        if shell.runtime != runtime {
            *shell = Self {
                runtime,
                directory: Some(workdir),
                pending: None,
            };
        }
        shell
    }
}

/// What a command line does to the directory of the shell running it, as far
/// as TermAl can follow (`engram_shell_move`).
#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramShellMove {
    /// No command of the line changes directory.
    Stays,
    /// The line starts by changing to this literal directory, and changes it
    /// nowhere else.
    To(String),
    /// A command changes directory where TermAl cannot follow.
    Lost,
}

/// Commands that change the directory of the shell running them: `cd`,
/// `chdir`, `pushd`, `popd`, and PowerShell's `Set-Location` (`sl`),
/// `Push-Location` and `Pop-Location`.
const ENGRAM_DIRECTORY_CHANGES: [&str; 8] = [
    "cd",
    "chdir",
    "pushd",
    "popd",
    "set-location",
    "sl",
    "push-location",
    "pop-location",
];

/// What `line` does to the directory of the shell that runs it. Only the
/// line's own commands count: a script it hands to another shell, quoted
/// (`bash -lc 'cd x && …'`), runs there, and that directory ends with it. A
/// line that starts with `cd DIR` (`Set-Location DIR`, `pushd DIR`) for a
/// literal DIR, and changes directory nowhere else, moves the shell to DIR.
/// Any other change is one TermAl cannot follow: one with no directory or a
/// computed one (`cd`, `cd -`, `cd ~/x`, `cd $DIR`), one back through a
/// stack (`popd`), one after another command, one in a group, subshell,
/// pipeline or background job (which may or may not persist), and `eval`. A
/// change made by a script the line runs or sources is not seen.
fn engram_shell_move(line: &str) -> EngramShellMove {
    let mut segments = vec![String::new()];
    let mut grouped = false;
    let mut quote = None;
    let mut previous = None;
    let mut chars = line.chars().peekable();
    while let Some(character) = chars.next() {
        let current = segments.last_mut().expect("a segment");
        match (quote, character) {
            (None, '\'' | '"') => {
                quote = Some(character);
                current.push(character);
            }
            (Some(open), close) if open == close => {
                quote = None;
                current.push(close);
            }
            (Some(_), other) => current.push(other),
            // A redirection (`2>&1`, `&>`), not an operator.
            (None, '&') if matches!(previous, Some('>' | '<')) || chars.peek() == Some(&'>') => {
                current.push('&');
            }
            (None, '&' | '|') if chars.peek() == Some(&character) => {
                chars.next();
                segments.push(String::new());
            }
            // A background job or a pipeline element runs apart from the
            // shell in bash, but not in PowerShell.
            (None, '&' | '|' | '(' | ')' | '{' | '}' | '`') => {
                grouped = true;
                segments.push(String::new());
            }
            (None, ';' | '\n' | '\r') => segments.push(String::new()),
            (None, other) => current.push(other),
        }
        previous = Some(character);
    }
    if quote.is_some() {
        return EngramShellMove::Lost;
    }
    // `builtin cd` and `command cd` are `cd`.
    let command_words = |words: Vec<String>| {
        let skip = words
            .iter()
            .take_while(|word| matches!(word.as_str(), "builtin" | "command"))
            .count();
        words[skip..].to_vec()
    };
    let commands = segments
        .iter()
        .filter_map(|segment| engram_shell_words(segment))
        .map(command_words)
        .filter(|words| !words.is_empty())
        .collect::<Vec<_>>();
    // The words as bash reads them, backslash escapes taken away: a target
    // bash reads otherwise than it is written (an unquoted backslash) names
    // a directory TermAl cannot be sure of, and an escaped name (`c\d`) is a
    // directory change all the same.
    let bash_commands = segments
        .iter()
        .filter_map(|segment| engram_shell_words_as(segment, true))
        .map(command_words)
        .filter(|words| !words.is_empty())
        .collect::<Vec<_>>();
    let as_bash = bash_commands.first();
    let changes_directory = |word: &String| {
        let program = engram_program_name(word);
        program == "eval" || ENGRAM_DIRECTORY_CHANGES.contains(&program.as_str())
    };
    // Every word counts, not only a command's first: a keyword (`then cd x`,
    // `do cd x`), a wrapper (`xargs cd`) or anything else may stand before
    // a command that changes directory. Both readings count.
    let count = |commands: &[Vec<String>]| {
        commands
            .iter()
            .map(|words| words.iter().filter(|word| changes_directory(word)).count())
            .sum::<usize>()
    };
    let changes = count(&commands).max(count(&bash_commands));
    match (changes, commands.first()) {
        (0, _) => EngramShellMove::Stays,
        (1, Some(first)) if !grouped && changes_directory(&first[0]) => match first.as_slice() {
            [program, directory]
                if !matches!(
                    engram_program_name(program).as_str(),
                    "popd" | "pop-location" | "eval"
                ) && engram_literal_directory(directory)
                    && as_bash == Some(first) =>
            {
                EngramShellMove::To(directory.clone())
            }
            _ => EngramShellMove::Lost,
        },
        _ => EngramShellMove::Lost,
    }
}

/// Whether a `cd` argument names one directory as written: not a flag, a
/// home `~`, a variable or a glob, which the shell would expand, and not a
/// network path, which TermAl does not resolve (`engram_network_path`).
fn engram_literal_directory(directory: &str) -> bool {
    !directory.is_empty()
        && !directory.starts_with(['-', '~'])
        && !directory.contains(['$', '%', '!', '`', '*', '?', '['])
        && !engram_network_path(directory)
}

/// Whether `path` names a network location (`\\server\share`,
/// `//server/share`, `\\?\UNC\server\share`). Resolving one can block for a
/// network timeout, and TermAl resolves paths on the runtime's event reader,
/// so such a path is taken to lead out of any worktree without being
/// resolved. A Windows verbatim path to a drive (`\\?\C:\…`, the form a
/// resolved path takes) is local.
fn engram_network_path(path: &str) -> bool {
    let path = path.replace('\\', "/");
    match path.strip_prefix("//?/") {
        Some(verbatim) => verbatim
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("UNC/")),
        None => path.starts_with("//"),
    }
}

/// `path` as Windows names it when Git Bash spells a drive the MSYS way
/// (`/c/github/x` for `C:/github/x`), as Claude's shell on Windows does;
/// unchanged on other systems and for any other path.
fn engram_msys_drive_path(path: &str) -> std::borrow::Cow<'_, str> {
    let bytes = path.as_bytes();
    if cfg!(windows)
        && bytes.len() >= 2
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && (bytes.len() == 2 || bytes[2] == b'/')
    {
        let drive = char::from(bytes[1]).to_ascii_uppercase();
        let rest = &path[2..];
        return std::borrow::Cow::Owned(format!(
            "{drive}:{}",
            if rest.is_empty() { "/" } else { rest }
        ));
    }
    std::borrow::Cow::Borrowed(path)
}

/// The directory a shell at `from` is in after `cd TO`: `from` is a known
/// place, or `None` when TermAl lost the shell, which only an absolute TO
/// leaves (on Windows, Git Bash's `/c/…` for `C:/…` is absolute). The
/// resolved path, when it is an existing directory; `None` otherwise, or for
/// a network place, which is never resolved, since then TermAl cannot say
/// where the shell is. A shell takes a `..` in a `cd` against the path as
/// written (bash's logical `cd`), where the file system takes it after
/// following a link (`/other/link/..` is `/other` to bash, but the link
/// target's parent on disk): where the two disagree, TermAl cannot say which
/// the shell did, so `None`.
fn engram_resolve_shell_move(from: Option<&str>, to: &str) -> Option<String> {
    let to = engram_msys_drive_path(to);
    let to = FsPath::new(to.as_ref());
    let target = if to.is_absolute() {
        to.to_path_buf()
    } else {
        FsPath::new(from?).join(to)
    };
    // A network place is never resolved (`engram_network_path`).
    if engram_network_path(&target.to_string_lossy()) {
        return None;
    }
    let climbs = target
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir));
    let physical = std::fs::canonicalize(&target).ok()?;
    if climbs && std::fs::canonicalize(engram_lexical_path(&target)).ok()? != physical {
        return None;
    }
    physical
        .is_dir()
        .then(|| physical.to_string_lossy().into_owned())
}

/// `path` with its `.` and `..` steps taken by name, as a shell's logical
/// `cd` takes them, without following any link.
fn engram_lexical_path(path: &FsPath) -> PathBuf {
    let mut lexical = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                lexical.pop();
            }
            std::path::Component::CurDir => {}
            other => lexical.push(other),
        }
    }
    lexical
}

/// Where a command of `record` with runtime key `key` may run, as far as
/// TermAl knows, each directory `None` for the workdir: the one its runtime
/// reported (`cwd`, or the one an earlier start of the same key reported);
/// else the workdir, and where its runtime's shell is presumed to be, since
/// TermAl cannot tell whether that shell keeps a `cd` (Claude's does, unless
/// set to return to its project; an ACP runtime's may not). `None` when a
/// command moved that shell where TermAl cannot follow, or is moving it now.
fn engram_command_directories(
    record: &SessionRecord,
    key: &str,
    cwd: Option<&str>,
) -> Option<Vec<Option<String>>> {
    let engram = &record.engram;
    if let Some(reported) = cwd
        .map(str::to_owned)
        .or_else(|| engram.running_command_keys.get(key).cloned().flatten())
    {
        return Some(vec![Some(reported)]);
    }
    match &engram.shell_directory {
        Some(shell) if shell.runtime == record.runtime.runtime_token() => {
            if shell
                .pending
                .as_ref()
                .is_some_and(|(moving, _)| moving != key)
            {
                return None;
            }
            shell
                .directory
                .clone()
                .map(|directory| vec![None, Some(directory)])
        }
        _ => Some(vec![None]),
    }
}

/// Where `record`'s runtime shell is presumed to be, as a place to resolve a
/// relative `cd` of the command `key` from: `None` when TermAl lost it or
/// another command's move is pending. A repeated report of the command whose
/// move is pending resolves from where that move starts.
fn engram_shell_position(record: &SessionRecord, key: &str) -> Option<String> {
    match &record.engram.shell_directory {
        Some(shell) if shell.runtime == record.runtime.runtime_token() => {
            if shell
                .pending
                .as_ref()
                .is_some_and(|(moving, _)| moving != key)
            {
                None
            } else {
                shell.directory.clone()
            }
        }
        _ => Some(record.session.workdir.clone()),
    }
}

/// Records what a command line the runtime of `record` reported for its
/// command `key`, with no directory, does to its shell (`engram_shell_move`):
/// a move to `moving_to`, resolved from where the shell was, settles when the
/// command ends (`engram_settle_shell_move`); one TermAl cannot follow, or a
/// second move at once, loses the shell whether the command runs or not.
/// Nothing when the runtime changed since `runtime` was read.
fn engram_note_shell_move(
    record: &mut SessionRecord,
    key: &str,
    runtime: &Option<RuntimeToken>,
    shell_move: EngramShellMove,
    moving_to: Option<String>,
) {
    if shell_move == EngramShellMove::Stays || record.runtime.runtime_token() != *runtime {
        return;
    }
    let shell = EngramShellDirectory::current(record);
    match shell_move {
        // A repeated report of the same command is the same move.
        EngramShellMove::To(_)
            if shell
                .pending
                .as_ref()
                .is_none_or(|(moving, _)| moving == key) =>
        {
            shell.pending = Some((key.to_owned(), moving_to));
        }
        _ => {
            shell.directory = None;
            shell.pending = None;
        }
    }
}

/// The command `key` of `record` ended: a `cd` it made settles. It moved
/// the shell only when the command succeeded; a failure or an unknown end
/// may have come before or after its `cd`, which loses the shell.
fn engram_settle_shell_move(
    record: &mut SessionRecord,
    key: &str,
    exit: Option<EngramCommandExit>,
) {
    let runtime = record.runtime.runtime_token();
    let Some(shell) = record.engram.shell_directory.as_mut() else {
        return;
    };
    if shell.runtime != runtime
        || !shell
            .pending
            .as_ref()
            .is_some_and(|(moving, _)| moving == key)
    {
        return;
    }
    let (_, target) = shell.pending.take().expect("a pending move");
    shell.directory = match exit {
        Some(EngramCommandExit::Code(0) | EngramCommandExit::ReportedSuccess) => target,
        _ => None,
    };
}

/// `engram_check_worktree` from every directory the command may run in
/// (`engram_command_directories`): the target from the last, the presumed
/// place of its runtime's shell, when every one names the session's
/// worktree and none names a place outside it.
fn engram_check_worktree_from(
    check: &EngramCheckCommand,
    workdir: &FsPath,
    directories: &[Option<String>],
) -> Option<EngramCheckTarget> {
    let mut target = None;
    for directory in directories {
        target = Some(engram_check_worktree(check, workdir, directory.as_deref())?);
    }
    target
}

/// Whether `line` holds, outside quotes, syntax a shell evaluates rather
/// than passing on as written: a group, block or expression (`(…)`, `{a,b}`,
/// PowerShell's `(…)`), an escape (`` ` ``, cmd's `^`), or a PowerShell splat
/// or array (`@` starting a word). TermAl cannot read such an argument the
/// way the shell will.
fn engram_has_unquoted_expression(line: &str) -> bool {
    let mut quote = None;
    let mut word_start = true;
    for character in line.chars() {
        match (quote, character) {
            (None, '\'' | '"') => quote = Some(character),
            (Some(open), close) if open == close => quote = None,
            (Some(_), _) => {}
            (None, '(' | ')' | '{' | '}' | '`' | '^') => return true,
            (None, '@') if word_start => return true,
            _ => {}
        }
        word_start = quote.is_none() && character.is_whitespace();
    }
    false
}

/// Where a recognised check ran and what it is credited to: the worktree
/// root, and the directory the command ran in.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramCheckTarget {
    root: PathBuf,
    directory: PathBuf,
}

/// Where a recognised check ran, when TermAl can tell it tested the
/// session's own worktree: the command runs in that worktree (in `cwd` when
/// the runtime reports one, relative to the session's `workdir`), and no
/// argument names a place outside it. Every argument, what follows each `=`
/// in it (`--flag=value`, `--override-ini=testpaths=../x`) and the value
/// attached to a one-letter option (`-c../x.ini`) is resolved as a path from
/// the command's directory (`engram_argument_values`); one
/// that leads out of the worktree, as an absolute path, a `..` climb or a
/// symbolic link, names what the command tested there, and so may one
/// starting at a home `~` or holding a variable anywhere (`tests/${SUITE}`,
/// `%SUITE%`, cmd's delayed `!SUITE!`, `$env:SUITE`), whose value TermAl
/// cannot see. Each argument is
/// judged as the shell running the line passes it on (bash drops its
/// backslash escapes; a line no wrapper names is read both ways), and so is
/// each piece of a comma list (a PowerShell array passes each as its own
/// argument); a line holding syntax a shell evaluates rather than passing on as
/// written (`engram_has_unquoted_expression`) is not read at all. `None` for
/// any of these: the result would be credited to a worktree the command may
/// not have tested, so no check is kept. A runner that tests something it finds by name (an import
/// path, an installed package) is taken to test the worktree it runs in. The
/// test launcher's `--engram-binary` names the pinned binary a live run
/// uses, not what it tests, so its value may lie anywhere. This touches the
/// file system, so it runs off the state lock.
fn engram_check_worktree(
    check: &EngramCheckCommand,
    workdir: &FsPath,
    cwd: Option<&str>,
) -> Option<EngramCheckTarget> {
    // A network place is never resolved (`engram_network_path`), whether the
    // runtime reported it or the session works there: the command runs in
    // `directory`, relative to `workdir`.
    let directory = cwd.map_or_else(|| workdir.to_path_buf(), |cwd| workdir.join(cwd));
    if engram_network_path(&workdir.to_string_lossy())
        || engram_network_path(&directory.to_string_lossy())
    {
        return None;
    }
    // What a check is credited to is compared exactly: a case-sensitive
    // volume may hold two worktrees whose paths differ only in case.
    let root = engram_worktree_root_path(workdir);
    let root_key = engram_exact_path_key(&root);
    if engram_exact_path_key(&engram_worktree_root_path(&directory)) != root_key {
        return None;
    }
    let line = engram_first_command(&check.normalized);
    if engram_has_unquoted_expression(line) {
        return None;
    }
    // Each argument as the shell running the line passes it on, which is
    // what the runner resolves: bash takes its backslash escapes away
    // (`lin\ked` is `linked`), PowerShell and cmd keep them (Windows paths).
    // A line no wrapper names may run under either, so both readings count.
    let readings = match check.dialect {
        EngramShellDialect::Bash => vec![engram_shell_words_as(line, true)?],
        EngramShellDialect::PowerShell | EngramShellDialect::Cmd => {
            vec![engram_shell_words(line)?]
        }
        EngramShellDialect::Unknown => vec![
            engram_shell_words(line)?,
            engram_shell_words_as(line, true)?,
        ],
    };
    let leads_out = |value: &str| {
        value.starts_with('~')
            // A variable: bash's or PowerShell's `$`, cmd's `%VAR%` and its
            // delayed `!VAR!` (under `/v:on`).
            || value.contains(['$', '%', '!'])
            || engram_network_path(value)
            || (!value.is_empty()
                && !engram_path_within(
                    &engram_exact_path_key(&engram_canonical_path(&directory.join(value))),
                    &root_key,
                ))
    };
    // A node selector (`tests/test_x.py::test_ok`) names its file before the
    // `::`, which may itself be a link; PowerShell passes each piece of an
    // array (`a,b`) as its own argument.
    let escapes = |value: &str| {
        std::iter::once(value).chain(value.split(',')).any(|value| {
            leads_out(value)
                || value
                    .split_once("::")
                    .is_some_and(|(file, _)| leads_out(file))
        })
    };
    const PINNED_BINARY_FLAG: &str = "--engram-binary";
    let launcher = check.program == "node";
    let named_elsewhere = readings.iter().any(|words| {
        words.iter().enumerate().skip(1).any(|(index, word)| {
            let pinned_binary = launcher
                && (words[index - 1] == PINNED_BINARY_FLAG
                    || word.starts_with(&format!("{PINNED_BINARY_FLAG}=")));
            !pinned_binary && engram_argument_values(word).into_iter().any(escapes)
        })
    });
    (!named_elsewhere).then_some(EngramCheckTarget { root, directory })
}

/// Every value `word` may hand a runner: the word itself, the value attached
/// to a one-letter option (`engram_attached_option_value`), what follows each
/// `=` in either, since an option may carry a setting whose value is itself a
/// path (`--override-ini=testpaths=../other`), and each item of any of them
/// read as a list, since a setting may hold several paths (pytest splits
/// `testpaths` on whitespace; a path list may use `;`).
fn engram_argument_values(word: &str) -> Vec<&str> {
    let mut values = Vec::new();
    for value in std::iter::once(word).chain(engram_attached_option_value(word)) {
        values.push(value);
        values.extend(value.match_indices('=').map(|(at, _)| &value[at + 1..]));
    }
    let items = values
        .iter()
        .flat_map(|value| {
            value.split(|character: char| character.is_whitespace() || character == ';')
        })
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    values.extend(items);
    values
}

/// The value `word` attaches to a one-letter option (`../x.ini` in
/// `-c../x.ini`, pytest's `-c ../x.ini`), which a runner may read as that
/// option's argument; `None` for any other word. A cluster of flags (`-vx`)
/// reads as one too, which only ever makes a check less likely.
fn engram_attached_option_value(word: &str) -> Option<&str> {
    let rest = word.strip_prefix('-')?;
    let mut chars = rest.chars();
    chars
        .next()
        .filter(char::is_ascii_alphabetic)
        .map(|_| chars.as_str())
        .filter(|value| !value.is_empty())
}
