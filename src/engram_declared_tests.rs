// Declared test commands (docs/features/declared-test-commands.md): the
// repository's `termal-tests.toml`, the reading of a declared command line on
// the declared route, and the exact match that makes a command a declared
// test check at its start. Owns the declaration's load rules and caps (a file
// that breaks one is disabled whole), the declared route's word reading (an
// allow-list: a quoted `;` is argument text, and anything a shell could read
// differently, operators, substitution, redirection and ambiguous quoting
// included, is refused), the one program-word rule for a bare line and a
// wrapper's shell alike, the exact wrapper shapes, the candidate a run line
// gives, and the binding a check keeps from its start: the matched entry, the
// declaration's hash and the artifact's start image. Does not own the
// artifact's inspection or the TRX reading (`engram_trx_artifact.rs`), the
// built-in recognition, which wins and is unchanged
// (`engram_check_recognition.rs`), or the check lifecycle that calls in here
// (`engram_turn_checks.rs`). New module: the declared route is kept apart
// from the built-in recognition rather than growing it.

/// The declaration's file name, at the root of the worktree a check runs in.
const ENGRAM_DECLARATION_FILE: &str = "termal-tests.toml";
/// A larger declaration is disabled whole.
const ENGRAM_DECLARATION_MAX_BYTES: usize = 64 * 1024;
/// A declaration with more entries is disabled whole.
const ENGRAM_DECLARATION_MAX_ENTRIES: usize = 64;

/// One `[[test]]` entry of the declaration, as written.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramDeclaredEntry {
    command: String,
    cwd: String,
    artifact: String,
}

/// What the worktree's declaration says, read at one moment.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramDeclaration {
    /// No `termal-tests.toml` at the root.
    Absent,
    /// A file that breaks a load rule: no entry of it counts.
    Disabled { sha256: String, reason: String },
    Loaded {
        sha256: String,
        bytes: u64,
        entries: Vec<EngramDeclaredEntry>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EngramRawDeclaration {
    #[serde(default)]
    test: Vec<EngramRawDeclaredEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EngramRawDeclaredEntry {
    command: Option<String>,
    cwd: Option<String>,
    artifact: Option<String>,
}

/// The declaration at `root`, read once within its cap. Only the file read
/// fails as `Disabled` without a hash (`sha256` empty) when it cannot be read
/// at all; every other refusal carries the hash of the bytes it judged.
fn engram_load_declaration(root: &FsPath) -> EngramDeclaration {
    let path = root.join(ENGRAM_DECLARATION_FILE);
    let disabled = |sha256: String, reason: &str| EngramDeclaration::Disabled {
        sha256,
        reason: reason.to_owned(),
    };
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return EngramDeclaration::Absent,
        Err(_) => return disabled(String::new(), "it cannot be read"),
        Ok(metadata) if !metadata.file_type().is_file() => {
            return disabled(String::new(), "it is not a regular file");
        }
        Ok(_) => {}
    }
    let bytes = match engram_read_bounded(&path, ENGRAM_DECLARATION_MAX_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return disabled(String::new(), "it exceeds the size cap"),
        Err(_) => return disabled(String::new(), "it cannot be read"),
    };
    let sha256 = sha256_hex(&bytes);
    match engram_parse_declaration(&bytes, root) {
        Ok(entries) => EngramDeclaration::Loaded {
            sha256,
            bytes: bytes.len() as u64,
            entries,
        },
        Err(reason) => disabled(sha256, reason),
    }
}

/// The whole file at `path`, or `None` when it is larger than `max` bytes.
fn engram_read_bounded(path: &FsPath, max: usize) -> io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok((bytes.len() <= max).then_some(bytes))
}

/// The entries of a declaration's `bytes`, or why the whole file is disabled.
fn engram_parse_declaration(
    bytes: &[u8],
    root: &FsPath,
) -> Result<Vec<EngramDeclaredEntry>, &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "it is not UTF-8")?;
    let raw = toml_edit::de::from_str::<EngramRawDeclaration>(text)
        .map_err(|_| "it does not parse as a declaration")?;
    if raw.test.len() > ENGRAM_DECLARATION_MAX_ENTRIES {
        return Err("it exceeds the entry cap");
    }
    let mut entries = Vec::with_capacity(raw.test.len());
    for raw in raw.test {
        let command = raw
            .command
            .filter(|command| !command.trim().is_empty())
            .ok_or("an entry is missing `command`")?;
        let artifact = raw
            .artifact
            .filter(|artifact| !artifact.is_empty())
            .ok_or("an entry is missing `artifact`")?;
        let cwd = raw.cwd.unwrap_or_else(|| ".".to_owned());
        let words = engram_declared_words(&command)?;
        if engram_declared_command_forbidden(&words) {
            return Err(
                "an entry's command holds `--no-build`, `--list-tests` or a `.dll` path, in one \
                 of their spellings",
            );
        }
        if !engram_declared_path_inside(root, &cwd, true) {
            return Err("an entry's `cwd` does not resolve inside the worktree");
        }
        if !engram_declared_path_inside(root, &artifact, false) {
            return Err("an entry's `artifact` does not resolve inside the worktree");
        }
        entries.push(EngramDeclaredEntry {
            command,
            cwd,
            artifact,
        });
    }
    Ok(entries)
}

/// Whether a declared command's words make it something other than a test
/// run of a built project: a run that skips the build, a listing, or a
/// prebuilt assembly named directly, in the spellings dotnet accepts
/// (`--no-build`, `--no-build:true`, `--no-build=true`; for `dotnet` also
/// `-t` and the MSBuild properties `VSTestNoBuild` and `VSTestListTests`),
/// and a `.dll` path even with the trailing dots or spaces Windows strips.
/// A guard against honest mistakes in a trusted declaration, not a proof.
fn engram_declared_command_forbidden(words: &[String]) -> bool {
    let dotnet = words
        .first()
        .is_some_and(|program| engram_program_name(program) == "dotnet");
    words.iter().any(|word| {
        let lower = word.to_ascii_lowercase();
        let option = lower.split([':', '=']).next().unwrap_or("");
        option == "--no-build"
            || option == "--list-tests"
            || (dotnet
                && (lower == "-t"
                    || lower.contains("vstestnobuild")
                    || lower.contains("vstestlisttests")))
            || lower.split(['=', ':', ';', ',']).any(|piece| {
                piece
                    .trim_end_matches(['.', ' '])
                    .ends_with(".dll")
            })
    })
}

/// Whether `relative`, a root-relative path from the declaration, stays
/// inside the worktree at `root` once its links are resolved. It must be
/// relative and name no drive, root or network prefix; a `cwd` must also be
/// an existing directory.
fn engram_declared_path_inside(root: &FsPath, relative: &str, directory: bool) -> bool {
    let path = FsPath::new(relative);
    if relative.is_empty()
        || relative.contains(['\0', '\n', '\r'])
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::Prefix(_) | std::path::Component::RootDir
            )
        })
    {
        return false;
    }
    let root = engram_canonical_path(root);
    let resolved = engram_canonical_path(&root.join(path));
    if directory && !resolved.is_dir() {
        return false;
    }
    engram_path_within(
        &engram_exact_path_key(&resolved),
        &engram_exact_path_key(&root),
    )
}

/// The words of a command line on the declared route, quotes taken away, or
/// why the line is refused. The reader is an allow-list, so a line that bash
/// and PowerShell could read differently is refused rather than guessed at:
/// - the line is printable ASCII, and words are separated by spaces and tabs;
/// - an unquoted word holds only letters, digits and `. _ / : = @ + -`, and
///   starts with neither `@` (PowerShell splatting) nor `=` (zsh expands an
///   `=word` to a command path); one that starts with `-` holds no `.`,
///   since PowerShell splits such a native argument in two;
/// - a quoted word is one whole token: its quote opens at a separator or the
///   start of the line and closes at a separator or the end, and it is not
///   empty (PowerShell may drop an empty argument). It holds the same
///   characters plus space, `;` and `=`: nothing a shell inside the quotes,
///   Windows PowerShell's legacy argument passing or a batch file's cmd could
///   read as syntax (no `"`, `\`, `&`, `|`, `<`, `>`, `^`, `$`, `` ` ``, `%`
///   or `!`, which the placement check reads as a cmd variable);
/// - the first word, the program, passes `engram_declared_program_word`, the
///   same rule a wrapper's shell word passes.
/// Everything else (shell operators, redirection, substitution, globbing,
/// commas, mixed quoting) is refused. Not a shell evaluator.
fn engram_declared_words(line: &str) -> Result<Vec<String>, &'static str> {
    const REFUSED: &str = "a command uses shell syntax the declared route does not take";
    // bash reads a backslash as an escape and PowerShell as a path separator.
    if line.contains('\\') {
        return Err("a command holds a backslash: use forward slashes in declared paths");
    }
    if !line
        .chars()
        .all(|character| character == '\t' || (' '..='~').contains(&character))
    {
        return Err("a command holds a character outside printable ASCII");
    }
    if !engram_declared_program_word(engram_raw_first_word(line)) {
        return Err(REFUSED);
    }
    let unquoted =
        |character: char| character.is_ascii_alphanumeric() || "._/:=@+-".contains(character);
    let quoted = |character: char| unquoted(character) || " ;=".contains(character);
    let separator = |character: Option<&char>| matches!(character, None | Some(' ' | '\t'));
    let mut words = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while matches!(chars.peek(), Some(' ' | '\t')) {
            chars.next();
        }
        let Some(&first) = chars.peek() else {
            break;
        };
        let mut word = String::new();
        if first == '\'' || first == '"' {
            chars.next();
            loop {
                match chars.next() {
                    None => return Err("a command has an unbalanced quote"),
                    Some(close) if close == first => break,
                    Some(other) if quoted(other) => word.push(other),
                    Some(_) => return Err(REFUSED),
                }
            }
            if word.is_empty() || !separator(chars.peek()) {
                return Err(REFUSED);
            }
        } else {
            while !separator(chars.peek()) {
                let character = chars.next().expect("peeked a character");
                if !unquoted(character) {
                    return Err(REFUSED);
                }
                word.push(character);
            }
            if word.starts_with(['@', '=']) || (word.starts_with('-') && word.contains('.')) {
                return Err(REFUSED);
            }
        }
        words.push(word);
    }
    if words.is_empty() {
        return Err("a command is empty");
    }
    Ok(words)
}

/// The first word of `line` exactly as written, quotes and all: everything
/// before the first space or tab after any leading ones.
fn engram_raw_first_word(line: &str) -> &str {
    line.trim_start_matches([' ', '\t'])
        .split([' ', '\t'])
        .next()
        .unwrap_or("")
}

/// Whether `raw`, the program word of a line exactly as written, is one the
/// declared route takes, the single rule for a bare declared line's program
/// and a wrapper's shell alike: unquoted (PowerShell reads a quoted first
/// word as a string, not a command), only letters, digits and
/// `. _ / : @ + -` (so no operator, redirection, substitution, escape or
/// non-ASCII character can make a shell run something else under the
/// name), no `=` (bash reads `X=…` as an assignment), not starting with `@`
/// (PowerShell splatting), and holding no `.` if it starts with `-`.
fn engram_declared_program_word(raw: &str) -> bool {
    !raw.is_empty()
        && raw
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._/:@+-".contains(character))
        && !raw.starts_with('@')
        && !(raw.starts_with('-') && raw.contains('.'))
}

/// A run line that may be a declared test: one the declared route can read,
/// alone or as the test of the one-call form (`pushd "DIR" && TEST`, read as
/// a bare line, as the built-in route reads it), with its words. The command
/// it gives is the line TermAl fingerprints, as for a built-in test. The
/// whole line, as the runtime reported it, is printable ASCII (a tab
/// allowed), and only ASCII spaces and tabs are trimmed from its ends.
/// All other controls, including CR and LF, are refused before trimming,
/// so no unsupported whitespace can be cut away before it is read. The
/// declared route assumes the line runs under bash or PowerShell 7 (`pwsh`):
/// cmd is never its program, at any wrapper level, since cmd takes no single
/// quotes as quotes and expands `%VAR%` inside double quotes, and Windows
/// PowerShell 5.1 (`powershell`) is refused for its legacy passing of
/// native arguments. A wrapper is read from its raw bytes alone
/// (`engram_declared_wrapper_script`). Asked only of a line the built-in
/// recognition took no test from.
fn engram_declared_candidate(command: &str) -> Option<(EngramCheckCommand, Vec<String>)> {
    if !command
        .bytes()
        .all(|byte| matches!(byte, b'\t' | b' '..=b'~'))
    {
        return None;
    }
    let command = command.trim_matches([' ', '\t']);
    let (line, dialect, directory) = match engram_one_call_prefix(command) {
        Some((directory, test)) => {
            let (_, dialect) = engram_unwrap_shell_command(test);
            if dialect != EngramShellDialect::Unknown {
                return None;
            }
            (test.to_owned(), dialect, Some(directory))
        }
        None => {
            // Which shell, if any, the line wraps its test in is recognised as
            // the built-in route does; what it runs is read from raw bytes.
            let (_, dialect) = engram_unwrap_shell_command(command);
            match dialect {
                EngramShellDialect::Cmd => return None,
                EngramShellDialect::Bash | EngramShellDialect::PowerShell => (
                    engram_declared_wrapper_script(command)?.to_owned(),
                    dialect,
                    None,
                ),
                EngramShellDialect::Unknown => (command.to_owned(), dialect, None),
            }
        }
    };
    let words = engram_declared_words(&line).ok()?;
    if matches!(
        engram_program_name(&words[0]).as_str(),
        "cmd" | "powershell"
    ) {
        return None;
    }
    Some((
        EngramCheckCommand {
            normalized: engram_collapse_whitespace(&line),
            simple: true,
            program: engram_program_name(&words[0]),
            dialect,
            directory,
            kind: EngramVerificationKind::Test,
        },
        words,
    ))
}

/// The script a wrapper line runs, read from the line's raw bytes alone, or
/// `None` unless every word of the line is exactly one of the shapes the
/// declared route takes. No quote is joined or removed before a word is
/// judged: pwsh splits `'-'c` into `-` and `c` where bash joins it, so a
/// word read only after joining could name a flag the shell never saw.
/// - The shell word passes `engram_declared_program_word`, names `bash`,
///   `sh`, `zsh` or `pwsh` (an `.exe` ending allowed), is a bare name or an
///   absolute path (a relative one may be anything the agent wrote), and
///   does not end in `.cmd`, `.bat` or `.ps1` (a shim runs under cmd or
///   another PowerShell).
/// - bash, sh and zsh take exactly one flag word, `-c` or `-lc`; pwsh takes
///   any of `-NoProfile`, `-NonInteractive` and `-NoLogo`, then `-Command`
///   or `-c`, each spelled exactly (case aside).
/// - The script is the rest of the line: one single-quoted word with no `'`
///   inside, which every shell passes on literally.
fn engram_declared_wrapper_script(line: &str) -> Option<&str> {
    let shell = engram_raw_first_word(line);
    let name = engram_program_name(shell);
    let lower = shell.to_ascii_lowercase();
    let absolute = shell.starts_with('/')
        || (shell.len() > 2
            && shell.as_bytes()[0].is_ascii_alphabetic()
            && &shell[1..3] == ":/");
    if !engram_declared_program_word(shell)
        || !matches!(name.as_str(), "bash" | "sh" | "zsh" | "pwsh")
        || [".cmd", ".bat", ".ps1"]
            .iter()
            .any(|ending| lower.ends_with(ending))
        || (shell.contains('/') && !absolute)
    {
        return None;
    }
    let mut rest = &line[line.find(shell)? + shell.len()..];
    let mut options = Vec::new();
    loop {
        rest = rest.trim_start_matches([' ', '\t']);
        if rest.starts_with('\'') || rest.is_empty() {
            break;
        }
        let end = rest.find([' ', '\t']).unwrap_or(rest.len());
        options.push(&rest[..end]);
        rest = &rest[end..];
    }
    let script_ok = rest.len() >= 2
        && rest.starts_with('\'')
        && rest.ends_with('\'')
        && !rest[1..rest.len() - 1].contains('\'');
    let shape_ok = match name.as_str() {
        "pwsh" => options.split_last().is_some_and(|(flag, switches)| {
            (flag.eq_ignore_ascii_case("-Command") || flag.eq_ignore_ascii_case("-c"))
                && switches.iter().all(|switch| {
                    ["-NoProfile", "-NonInteractive", "-NoLogo"]
                        .iter()
                        .any(|allowed| switch.eq_ignore_ascii_case(allowed))
                })
        }),
        _ => matches!(options.as_slice(), ["-c"] | ["-lc"]),
    };
    (script_ok && shape_ok).then(|| &rest[1..rest.len() - 1])
}

/// The artifact as a declared check's start found it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramArtifactStart {
    Absent,
    Present { sha256: String },
    /// The start inspection itself failed: not the same as absent.
    Failed,
}

/// What a declared check keeps from its start, verified again at its end.
#[derive(Clone, Debug)]
struct EngramDeclaredBinding {
    /// The worktree root the declaration was read in.
    root: PathBuf,
    entry: EngramDeclaredEntry,
    declaration_sha256: String,
    declaration_bytes: u64,
    artifact_start: EngramArtifactStart,
    /// Where the declared artifact path led at the start, its links
    /// resolved (`engram_canonical_path`, which resolves the part that
    /// exists): the end must find the artifact at exactly this path, so a
    /// link retargeted during the run cannot bring in another file.
    artifact_canonical: String,
    /// Taken before the artifact's start image, so a file the run writes
    /// after it is never older than this.
    started: std::time::SystemTime,
}

/// What a declared candidate found at `target`'s root: a binding when the
/// candidate's `words`, run in `target.directory`, match exactly one entry;
/// the host line to give when the declaration there is disabled; or neither.
/// A candidate whose words match no entry, or two, is no check. Reads the
/// file system, so it runs off the state lock.
fn engram_declared_bind(
    words: &[String],
    target: &EngramCheckTarget,
) -> (Option<EngramDeclaredBinding>, Option<(String, String)>) {
    let (sha256, bytes, entries) = match engram_load_declaration(&target.root) {
        EngramDeclaration::Absent => return (None, None),
        EngramDeclaration::Disabled { sha256, reason } => {
            return (None, Some((sha256, engram_declaration_disabled_line(target, &reason))));
        }
        EngramDeclaration::Loaded {
            sha256,
            bytes,
            entries,
        } => (sha256, bytes, entries),
    };
    let directory = engram_exact_path_key(&engram_canonical_path(&target.directory));
    let root = engram_canonical_path(&target.root);
    let mut matched = entries.into_iter().filter(|entry| {
        engram_declared_words(&entry.command).is_ok_and(|declared| declared == words)
            // Built-in recognition wins: such an entry never applies.
            && engram_check_command(&entry.command).is_none()
            && engram_exact_path_key(&engram_canonical_path(&root.join(&entry.cwd))) == directory
    });
    let (Some(entry), None) = (matched.next(), matched.next()) else {
        return (None, None);
    };
    let started = std::time::SystemTime::now();
    let artifact_start = engram_artifact_start_image(&target.root, &entry.artifact);
    let artifact_canonical =
        engram_exact_path_key(&engram_canonical_path(&target.root.join(&entry.artifact)));
    (
        Some(EngramDeclaredBinding {
            root: target.root.clone(),
            entry,
            declaration_sha256: sha256,
            declaration_bytes: bytes,
            artifact_start,
            artifact_canonical,
            started,
        }),
        None,
    )
}

/// A declared test check a command starts, when it is one: the line's
/// candidate (`engram_declared_candidate`), where it runs
/// (`engram_check_worktree_from`, as for a built-in test) and its binding
/// there (`engram_declared_bind`); and, apart from that, the declaration
/// hash and host line of a disabled declaration where it runs. Only a
/// worktree holding a declaration is read past that file's presence.
fn engram_declared_start(
    ran: Option<&str>,
    workdir: &FsPath,
    places: Option<&[Option<String>]>,
    credit_root: Option<(&FsPath, &str)>,
) -> (
    Option<(EngramCheckCommand, EngramCheckTarget, EngramDeclaredBinding)>,
    Option<(String, String)>,
) {
    let Some((command, words)) = ran.and_then(engram_declared_candidate) else {
        return (None, None);
    };
    let root = credit_root.map_or_else(|| engram_worktree_root_path(workdir), |(root, _)| {
        root.to_path_buf()
    });
    if fs::symlink_metadata(root.join(ENGRAM_DECLARATION_FILE)).is_err() {
        return (None, None);
    }
    let Some(target) = engram_check_worktree_from(&command, workdir, places, credit_root) else {
        return (None, None);
    };
    let (binding, line) = engram_declared_bind(&words, &target);
    (binding.map(|binding| (command, target, binding)), line)
}

/// Whether the declaration at the binding's root still has the hash and the
/// entry the check started with.
fn engram_declaration_unchanged(binding: &EngramDeclaredBinding) -> bool {
    match engram_load_declaration(&binding.root) {
        EngramDeclaration::Loaded {
            sha256, entries, ..
        } => sha256 == binding.declaration_sha256 && entries.contains(&binding.entry),
        _ => false,
    }
}

/// The host line a disabled declaration gives, once per declaration content.
fn engram_declaration_disabled_line(target: &EngramCheckTarget, reason: &str) -> String {
    format!(
        "[TermAl] {ENGRAM_DECLARATION_FILE} in {} is disabled: {reason}. No declared test command \
         is recognised there until it is fixed.",
        target.root.display()
    )
}
