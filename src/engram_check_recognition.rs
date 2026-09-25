// Recognition of a test command a runtime reported, and what its end may
// claim (Engram w-108a13d58018 criterion 2, tm-winf step 2). Owns the shell
// wrapper and word reading of a command line, which commands count as a test
// and their check fingerprint, the exit a runtime's end reports and the
// outcome it may honestly claim, the result lines kept from a test's output,
// and the summary and references a verification record carries. Does not own
// where a check ran (`engram_check_paths.rs`), the check lifecycle, overlap
// marking and turn report (`engram_turn_checks.rs`), or the toolchain label
// (`engram_check_toolchain.rs`). Split out of `engram_turn_checks.rs`.

/// A command TermAl recognises as a test run.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramCheckCommand {
    /// The command line with one shell wrapper removed and whitespace runs
    /// collapsed; its SHA-256 is the check fingerprint.
    normalized: String,
    /// Whether the exit status is the check's own: one simple command with
    /// no pipe, list, redirection or substitution that could mask it.
    simple: bool,
    /// The program, lower-cased and without an extension, which names the
    /// toolchain.
    program: String,
    /// The shell the wrapper named, which decides how the line's arguments
    /// reach the runner.
    dialect: EngramShellDialect,
}

/// The shell a recognised line runs under, as its wrapper names it: bash
/// takes a backslash as an escape, PowerShell and cmd keep it (a Windows
/// path). A line with no wrapper may run under either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramShellDialect {
    Bash,
    PowerShell,
    Cmd,
    Unknown,
}

/// Recognises a test run the way an author can predict: one shell wrapper
/// (`bash -lc 'X'`, `pwsh -Command X`, `cmd /c X`) is removed, and the
/// program with its first words must name a test runner. A test inside a
/// longer line (`cd x && cargo test`) is not recognised; a recognised one
/// with a pipe or a list is recorded, but only as unknown.
fn engram_check_command(command: &str) -> Option<EngramCheckCommand> {
    let (line, dialect) = engram_unwrap_shell_command(command.trim());
    // A script of several lines is several commands; read as one, the
    // lines after the first would pass for the test's arguments.
    if engram_has_unquoted_line_break(&line) {
        return None;
    }
    let simple =
        !(line.contains(['|', '&', ';', '<', '>', '`', '\n', '\r']) || line.contains("$("));
    let normalized = engram_collapse_whitespace(&line);
    // The first command of the line decides what runs as the test; what
    // follows it can only mask its exit status, which `simple` records.
    let words = engram_shell_words(engram_first_command(&normalized))?;
    let program = engram_program_name(&words[0]);
    engram_is_test_command(&program, &words[1..]).then_some(EngramCheckCommand {
        normalized,
        simple,
        program,
        dialect,
    })
}

/// Whether `line` breaks onto another line outside quotes.
fn engram_has_unquoted_line_break(line: &str) -> bool {
    let mut quote = None;
    line.chars().any(|character| {
        match (quote, character) {
            (None, '\'' | '"') => quote = Some(character),
            (Some(open), close) if open == close => quote = None,
            (None, '\n' | '\r') => return true,
            _ => {}
        }
        false
    })
}

/// The part of `line` before its first unquoted pipe, list or redirection
/// operator: the command a shell would start first.
fn engram_first_command(line: &str) -> &str {
    let mut quote = None;
    for (index, character) in line.char_indices() {
        match (quote, character) {
            (None, '\'' | '"') => quote = Some(character),
            (Some(open), close) if open == close => quote = None,
            (None, ';' | '|' | '&' | '<' | '>' | '`') => return &line[..index],
            _ => {}
        }
    }
    line
}

/// `line` with its ends trimmed and each run of whitespace outside quotes
/// collapsed to one space. Quoted text is kept as written: the shell reads
/// it as one word (`engram_shell_words`), spaces and all.
fn engram_collapse_whitespace(line: &str) -> String {
    let mut collapsed = String::with_capacity(line.len());
    let mut quote = None;
    let mut space = false;
    let mut chars = line.trim().chars();
    while let Some(character) = chars.next() {
        match quote {
            Some(open) => {
                collapsed.push(character);
                if open == '"' && character == '\\' {
                    // `\"` and `\\` stay inside the quotes; any other
                    // character after a backslash changes nothing.
                    collapsed.extend(chars.next());
                } else if character == open {
                    quote = None;
                }
            }
            None if character.is_whitespace() => space = true,
            None => {
                if std::mem::take(&mut space) {
                    collapsed.push(' ');
                }
                if matches!(character, '\'' | '"') {
                    quote = Some(character);
                }
                collapsed.push(character);
            }
        }
    }
    collapsed
}

/// The check fingerprint an author pins with `--bind N=test:FINGERPRINT`:
/// the lowercase hex SHA-256 of the normalised command line's UTF-8 bytes.
fn engram_check_fingerprint(check: &EngramCheckCommand) -> String {
    sha256_hex(check.normalized.as_bytes())
}

/// Splits a command line into words the way a shell would for the cases
/// recognition needs: single quotes are literal, and a backslash is kept
/// (Windows paths, a UNC `\\server\share` quoted or not) except where a run
/// of them meets a double quote inside double quotes, where, as a Windows
/// program splits its command line, `2n` of them before a closing quote are
/// `n`, and `2n + 1` are `n` and a literal quote. `None` for an unbalanced
/// quote or an empty line.
fn engram_shell_words(line: &str) -> Option<Vec<String>> {
    engram_shell_words_as(line, false)
}

/// `engram_shell_words`, or with `bash` the words bash passes on: outside
/// quotes a backslash escapes the character after it (`lin\ked` is
/// `linked`), and inside double quotes it escapes `$`, `` ` ``, `"` and `\`.
fn engram_shell_words_as(line: &str, bash: bool) -> Option<Vec<String>> {
    Some(
        engram_shell_word_spans_as(line, bash)?
            .into_iter()
            .map(|(_, word)| word)
            .collect(),
    )
}

/// `engram_shell_words`, each with the byte offset in `line` it starts at.
fn engram_shell_word_spans(line: &str) -> Option<Vec<(usize, String)>> {
    engram_shell_word_spans_as(line, false)
}

/// `engram_shell_words_as`, each word with the byte offset it starts at.
fn engram_shell_word_spans_as(line: &str, bash: bool) -> Option<Vec<(usize, String)>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut start = None;
    let mut chars = line.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        match character {
            '\'' => {
                start.get_or_insert(index);
                loop {
                    match chars.next()?.1 {
                        '\'' => break,
                        other => current.push(other),
                    }
                }
            }
            '"' => {
                start.get_or_insert(index);
                loop {
                    match chars.next()?.1 {
                        '"' => break,
                        '\\' if bash => match chars.peek() {
                            Some((_, '"' | '\\' | '$' | '`')) => {
                                current.push(chars.next().expect("peeked character").1);
                            }
                            _ => current.push('\\'),
                        },
                        '\\' => {
                            // A run of backslashes is literal unless it meets
                            // a quote (a Windows program's own splitting).
                            let mut run = 1;
                            while matches!(chars.peek(), Some((_, '\\'))) {
                                chars.next();
                                run += 1;
                            }
                            if matches!(chars.peek(), Some((_, '"'))) {
                                current.extend(std::iter::repeat_n('\\', run / 2));
                                if run % 2 == 1 {
                                    current.push(chars.next().expect("peeked quote").1);
                                }
                            } else {
                                current.extend(std::iter::repeat_n('\\', run));
                            }
                        }
                        other => current.push(other),
                    }
                }
            }
            '\\' if bash => {
                start.get_or_insert(index);
                if let Some((_, escaped)) = chars.next() {
                    current.push(escaped);
                }
            }
            whitespace if whitespace.is_whitespace() => {
                if let Some(start) = start.take() {
                    words.push((start, std::mem::take(&mut current)));
                }
            }
            other => {
                start.get_or_insert(index);
                current.push(other);
            }
        }
    }
    if let Some(start) = start {
        words.push((start, current));
    }
    (!words.is_empty()).then_some(words)
}

/// A program's name as recognition compares it: the last path component,
/// lower-cased, without a Windows executable extension.
fn engram_program_name(word: &str) -> String {
    let name = word
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(word)
        .to_ascii_lowercase();
    [".exe", ".cmd", ".bat", ".ps1"]
        .iter()
        .find_map(|extension| name.strip_suffix(extension).map(str::to_owned))
        .unwrap_or(name)
}

/// The script a shell wrapper runs, with the shell that runs it, or the line
/// itself when it is not one of the wrappers runtimes use. A script given as
/// one word (quoted whole) is that word; `pwsh -Command` and `cmd /c` also
/// take the rest of the line as written, its quoting kept, so each argument
/// stays the one the runner saw.
fn engram_unwrap_shell_command(command: &str) -> (String, EngramShellDialect) {
    let unwrapped = || (command.to_owned(), EngramShellDialect::Unknown);
    let Some(words) = engram_shell_word_spans(command) else {
        return unwrapped();
    };
    let rest_after = |flag: usize| match &words[flag + 1..] {
        [] => None,
        [(_, script)] => Some(script.clone()),
        [(start, _), ..] => Some(command[*start..].to_owned()),
    };
    let script = match engram_program_name(&words[0].1).as_str() {
        "bash" | "sh" | "zsh" => words
            .iter()
            .position(|(_, word)| {
                word.len() > 1
                    && word.starts_with('-')
                    && !word.starts_with("--")
                    && word[1..].chars().all(|flag| flag.is_ascii_alphabetic())
                    && word.contains('c')
            })
            .filter(|position| position + 2 == words.len())
            .map(|position| (words[position + 1].1.clone(), EngramShellDialect::Bash)),
        "pwsh" | "powershell" => engram_powershell_script_position(&words)
            // A script that starts elsewhere (`-WorkingDirectory`) runs
            // there, not where the runtime says: it is not read as run in
            // place.
            .filter(|position| !engram_powershell_sets_directory(&words[1..*position]))
            .and_then(rest_after)
            .map(|script| (script, EngramShellDialect::PowerShell)),
        "cmd" => words
            .iter()
            .position(|(_, word)| word.eq_ignore_ascii_case("/c"))
            .and_then(rest_after)
            .map(|script| (script, EngramShellDialect::Cmd)),
        _ => None,
    };
    script.unwrap_or_else(unwrapped)
}

/// Where PowerShell's `-Command` (`-c`) flag stands among a wrapper's words;
/// the script follows it.
fn engram_powershell_script_position(words: &[(usize, String)]) -> Option<usize> {
    words.iter().position(|(_, word)| {
        word.eq_ignore_ascii_case("-command") || word.eq_ignore_ascii_case("-c")
    })
}

/// Whether PowerShell parameters `words` (those before its script) set the
/// directory it starts in: `-WorkingDirectory`, its alias `-wd`, or an
/// abbreviation of it, spelled with `-`, `--` or `/`, its value apart or after
/// `:`.
fn engram_powershell_sets_directory(words: &[(usize, String)]) -> bool {
    words.iter().any(|(_, word)| {
        let Some(name) = word
            .strip_prefix("--")
            .or_else(|| word.strip_prefix(['-', '/']))
        else {
            return false;
        };
        let name = name
            .split(':')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        name == "wd" || (name.starts_with("wo") && "workingdirectory".starts_with(&name))
    })
}

/// Whether `line` is a PowerShell wrapper that sets the directory its script
/// starts in (`engram_powershell_sets_directory`): where it runs, and may
/// write, is then not where its runtime says.
fn engram_wrapper_sets_directory(line: &str) -> bool {
    let Some(words) = engram_shell_word_spans(line) else {
        return false;
    };
    let Some((_, program)) = words.first() else {
        return false;
    };
    matches!(engram_program_name(program).as_str(), "pwsh" | "powershell")
        && engram_powershell_sets_directory(
            &words[1..engram_powershell_script_position(&words).unwrap_or(words.len())],
        )
}

/// Whether `program` with `args` runs a test suite, as far as TermAl can say
/// with confidence. Flags that make a runner list or build tests without
/// running them (`--no-run`, `--list`, `--collect-only` and pytest's `--co`)
/// disqualify it: such a run exits 0 having tested nothing. The test launcher counts only in
/// full or live mode, or focused on a command that is itself a test, since a
/// focused run executes whatever follows `--`.
fn engram_is_test_command(program: &str, args: &[String]) -> bool {
    let arg = |index: usize| args.get(index).map(String::as_str);
    if args.iter().any(|word| {
        matches!(
            word.as_str(),
            "--no-run" | "--list" | "--collect-only" | "--co"
        )
    }) {
        return false;
    }
    match program {
        "cargo" => {
            let rest = args
                .iter()
                .map(String::as_str)
                .skip_while(|word| word.starts_with('+'))
                .collect::<Vec<_>>();
            matches!(rest.as_slice(), ["test", ..] | ["nextest", "run", ..])
        }
        "npm" => {
            matches!(arg(0), Some("test" | "t"))
                || (arg(0) == Some("run") && arg(1) == Some("test"))
        }
        "pnpm" | "yarn" => {
            arg(0) == Some("test") || (arg(0) == Some("run") && arg(1) == Some("test"))
        }
        "npx" => matches!(arg(0), Some("vitest" | "jest")),
        "pytest" => true,
        "python" | "python3" | "py" => arg(0) == Some("-m") && arg(1) == Some("pytest"),
        // `-list` names tests and `-c` only compiles, each exiting 0.
        "go" => {
            arg(0) == Some("test")
                && !args.iter().any(|word| {
                    word == "-c" || word.starts_with("-list") || word.starts_with("--list")
                })
        }
        "node" => {
            let launcher = arg(0)
                .is_some_and(|script| script.replace('\\', "/").ends_with("test-launcher.mjs"));
            if !launcher || args.iter().any(|word| word == "--detach") {
                return false;
            }
            match arg(1) {
                Some("full" | "live") => true,
                Some("focused") => args
                    .iter()
                    .position(|word| word == "--")
                    .and_then(|separator| args.get(separator + 1..))
                    .filter(|focused| !focused.is_empty())
                    .is_some_and(|focused| {
                        engram_is_test_command(&engram_program_name(&focused[0]), &focused[1..])
                    }),
                _ => false,
            }
        }
        _ => false,
    }
}

/// What a runtime said about how a finished command ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramCommandExit {
    /// The process's own exit status.
    Code(i64),
    /// No exit status, but the runtime reported success (Claude's Bash
    /// result without `is_error`).
    ReportedSuccess,
    /// Nothing to rely on: a failure without an exit status, a command that
    /// was declined or interrupted, or one that may never have run.
    Unknown,
    /// The result marks the command's launch, not its end: a background run.
    NotFinished,
}

/// Claude's Bash result has no exit status of its own. A non-zero exit sets
/// `is_error` and starts the result text with `Exit code N`; `is_error`
/// without it may mean a denial or a timeout, and an interrupted command may
/// never have finished.
fn engram_claude_command_exit(
    is_error: bool,
    interrupted: bool,
    background: bool,
    detail: &str,
) -> EngramCommandExit {
    if background {
        return EngramCommandExit::NotFinished;
    }
    if interrupted {
        return EngramCommandExit::Unknown;
    }
    if !is_error {
        return EngramCommandExit::ReportedSuccess;
    }
    detail
        .trim_start()
        .strip_prefix("Exit code ")
        .and_then(|rest| {
            rest.split(|character: char| !character.is_ascii_digit())
                .next()
                .filter(|digits| !digits.is_empty())
        })
        .and_then(|digits| digits.parse().ok())
        .map_or(EngramCommandExit::Unknown, EngramCommandExit::Code)
}

/// Codex's `commandExecution` item carries the process's exit status. A
/// command declined, or ended without one, gives nothing to rely on; an item
/// still in progress has not ended.
fn engram_codex_command_exit(item: &Value) -> EngramCommandExit {
    let code = item.get("exitCode").and_then(Value::as_i64);
    match (item.get("status").and_then(Value::as_str), code) {
        (Some("completed" | "failed"), Some(code)) => EngramCommandExit::Code(code),
        (Some("completed" | "failed" | "declined"), _) => EngramCommandExit::Unknown,
        _ => EngramCommandExit::NotFinished,
    }
}

/// An ACP tool update carries a shell exit status only in `rawOutput`, and
/// only for tools that report one.
fn engram_acp_command_exit(update: &Value) -> EngramCommandExit {
    update
        .pointer("/rawOutput/exitCode")
        .and_then(Value::as_i64)
        .map_or(EngramCommandExit::Unknown, EngramCommandExit::Code)
}

/// The outcome a finished check may claim, or `None` for a result that does
/// not mark the check's end. Only a simple command's own exit status can
/// say it succeeded or failed; a pipe or list may have masked it.
fn engram_check_outcome(exit: EngramCommandExit, simple: bool) -> Option<EngramExecutionOutcome> {
    match exit {
        EngramCommandExit::NotFinished => None,
        _ if !simple => Some(EngramExecutionOutcome::Unknown),
        EngramCommandExit::Code(0) | EngramCommandExit::ReportedSuccess => {
            Some(EngramExecutionOutcome::Succeeded)
        }
        EngramCommandExit::Code(_) => Some(EngramExecutionOutcome::Failed),
        EngramCommandExit::Unknown => Some(EngramExecutionOutcome::Unknown),
    }
}

/// The longest prefix of `text` within `max` bytes, cut at a character
/// boundary.
fn engram_truncate_utf8(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// At most this many result lines are kept from a check's output.
const ENGRAM_CHECK_RESULT_LINE_LIMIT: usize = 32;

/// `text` without terminal escape sequences or control characters other than
/// newlines and tabs, so a summary carries what the runner printed, not how.
fn engram_strip_terminal_codes(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\u{1b}' => match chars.next() {
                // A control sequence ends at its final byte.
                Some('[') => {
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                // An operating-system command ends at BEL or ESC \.
                Some(']') => {
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\n' | '\t' => clean.push(character),
            control if control.is_control() => {}
            other => clean.push(other),
        }
    }
    clean
}

/// The lines of a check's output in which its runner states the result
/// (`test result: …` from cargo, pytest's closing summary, the vitest and
/// jest totals, go's per-package lines, the test launcher's verdict), with
/// terminal codes stripped. Only these reach the summary: raw test output can
/// carry secrets and paths, and a summary Engram's redactor refuses drops the
/// whole turn's report.
fn engram_check_result_lines(program: &str, output: &str) -> Vec<String> {
    let clean = engram_strip_terminal_codes(output);
    let mut lines = Vec::new();
    let mut results = 0;
    for raw in clean.lines() {
        let line = raw.trim();
        // The size-inventory lines are kept whole: a partial inventory would
        // misstate which paths the check covered.
        if program == "cargo" && engram_is_inventory_line(raw) {
            lines.push(raw.to_owned());
        } else if results < ENGRAM_CHECK_RESULT_LINE_LIMIT && engram_is_result_line(program, line) {
            results += 1;
            let line = match program {
                "node" => engram_launcher_stage_line(line).unwrap_or_else(|| line.to_owned()),
                _ => line.to_owned(),
            };
            lines.push(engram_truncate_utf8(&line, ENGRAM_CHECK_REF_MAX_BYTES).to_owned());
        }
    }
    lines
}

/// A line of a file-size test's inventory, `PATH: N physical lines (limit
/// M)`, the shape Engram's size test prints for every guarded file so the
/// evidence can list what it covered.
fn engram_is_inventory_line(line: &str) -> bool {
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let Some(first) = line.chars().next() else {
        return false;
    };
    if first.is_whitespace() {
        return false;
    }
    let Some((before_limit, limit)) = line
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once(" physical lines (limit "))
    else {
        return false;
    };
    let Some((path, count)) = before_limit.rsplit_once(": ") else {
        return false;
    };
    !path.is_empty() && digits(count) && digits(limit)
}

/// Whether `line`, trimmed, is where `program`'s runner states a result:
/// libtest's `test result:` and nextest's `Summary`, pytest's closing
/// banner or quiet summary, the vitest and jest totals, go's package lines,
/// and the test launcher's verdict and stage lines.
fn engram_is_result_line(program: &str, line: &str) -> bool {
    match program {
        "cargo" => line.starts_with("test result:") || line.starts_with("Summary ["),
        "pytest" | "python" | "python3" | "py" => {
            let states = |words: &[&str]| words.iter().any(|word| line.contains(word));
            // The closing banner, or the quiet summary `-q` prints in its
            // place: `3 passed in 0.10s`, `1 failed, 2 passed in 0.52s`.
            let quiet = (line
                .split_whitespace()
                .next()
                .is_some_and(|count| count.parse::<u64>().is_ok())
                || line.starts_with("no tests ran"))
                && line.contains(" in ");
            (line.starts_with('=')
                && states(&[" passed", " failed", " error", "no tests ran", " collected"]))
                || (quiet && states(&[" passed", " failed", " error", "no tests ran"]))
        }
        "npm" | "pnpm" | "yarn" | "npx" => ["Tests ", "Tests:", "Test Files ", "Test Suites:"]
            .iter()
            .any(|prefix| line.starts_with(prefix)),
        "go" => line.starts_with("ok ") || line.starts_with("FAIL") || line.starts_with("? "),
        "node" => {
            line.starts_with("PASS ")
                || line.starts_with("FAIL ")
                || engram_launcher_stage_line(line).is_some()
        }
        _ => false,
    }
}

/// A test-launcher stage line, `NAME: passed|failed …`, cut after the
/// status: the rest names a log path, which the summary leaves out.
fn engram_launcher_stage_line(line: &str) -> Option<String> {
    let (name, rest) = line.split_once(": ")?;
    let status = rest.split_whitespace().next()?;
    (!name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
        && matches!(status, "passed" | "failed"))
    .then(|| format!("{name}: {status}"))
}

/// The count a result line reports as passed: the number before its first
/// ` passed` (`test result: ok. 3 passed;`, `42 tests run: 42 passed`,
/// `Tests  3 passed (3)`, `=== 4 passed in 0.10s ===`).
fn engram_passed_count(line: &str) -> Option<u64> {
    let (before, _) = line.split_once(" passed")?;
    before.split_whitespace().next_back()?.parse().ok()
}

/// Whether the check's result lines show that at least one test ran and
/// passed. A run that tested nothing can exit 0 (a filter that matched
/// nothing, a collect-only or list mode, an npm script that runs no tests),
/// so success needs this positive evidence, runner by runner; without it the
/// check is unknown, never passed. The test launcher shows it only through a
/// passed test stage of the mode that ran it: a full gate's `rust-tests` or
/// `vitest`, a live run's `engram-live`. A focused run's verdict says nothing
/// about how many tests the wrapped command ran.
fn engram_check_showed_passing_tests(check: &EngramCheckCommand, result_lines: &[String]) -> bool {
    let passed = |prefixes: &[&str]| {
        result_lines.iter().any(|line| {
            prefixes.iter().any(|prefix| line.starts_with(prefix))
                && engram_passed_count(line).is_some_and(|count| count > 0)
        })
    };
    match check.program.as_str() {
        "cargo" => passed(&["test result:", "Summary ["]),
        // Every pytest result line is a summary: the banner or the quiet one.
        "pytest" | "python" | "python3" | "py" => passed(&[""]),
        "npm" | "pnpm" | "yarn" | "npx" => passed(&["Tests ", "Tests:"]),
        "go" => result_lines.iter().any(|line| {
            line.starts_with("ok ")
                && !line.contains("[no tests to run]")
                && !line.contains("[no test files]")
        }),
        "node" => {
            let test_stages: &[&str] =
                match engram_shell_words(engram_first_command(&check.normalized))
                    .as_ref()
                    .and_then(|words| words.get(2))
                    .map(String::as_str)
                {
                    Some("full") => &["rust-tests: passed", "vitest: passed"],
                    Some("live") => &["engram-live: passed"],
                    _ => &[],
                };
            result_lines
                .iter()
                .any(|line| test_stages.contains(&line.as_str()))
        }
        _ => false,
    }
}

/// The verification summary: the command, how it ended, and its runner's
/// result lines in output order, within Engram's bound. A size inventory is
/// all or nothing: when its lines do not all fit, every one is left out and
/// a note says how many, since a partial list would misstate which paths the
/// check covered.
fn engram_check_summary(
    check: &EngramCheckCommand,
    exit: EngramCommandExit,
    result_lines: &[String],
) -> String {
    let command = engram_truncate_utf8(&check.normalized, ENGRAM_CHECK_REF_MAX_BYTES);
    let mut summary = match exit {
        EngramCommandExit::Code(code) => format!("`{command}` exited {code}"),
        EngramCommandExit::ReportedSuccess => {
            format!("`{command}` succeeded (the runtime reports no exit status)")
        }
        EngramCommandExit::Unknown | EngramCommandExit::NotFinished => {
            format!("`{command}` ended without an exit status")
        }
    };
    let whole = summary.len()
        + result_lines
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>();
    if whole <= ENGRAM_CHECK_SUMMARY_MAX_BYTES {
        for line in result_lines {
            summary.push('\n');
            summary.push_str(line);
        }
        return summary;
    }
    let inventory = result_lines
        .iter()
        .filter(|line| engram_is_inventory_line(line))
        .count();
    let note = (inventory > 0)
        .then(|| format!("inventory omitted: {inventory} lines over the summary budget"));
    let reserved = note.as_ref().map_or(0, |note| note.len() + 1);
    for line in result_lines
        .iter()
        .filter(|line| !engram_is_inventory_line(line))
    {
        if summary.len() + 1 + line.len() + reserved > ENGRAM_CHECK_SUMMARY_MAX_BYTES {
            break;
        }
        summary.push('\n');
        summary.push_str(line);
    }
    if let Some(note) = note {
        summary.push('\n');
        summary.push_str(&note);
    }
    summary
}

/// The verification references: the normalised command, unless it exceeds
/// Engram's bound (the check fingerprint names it anyway), and, when known,
/// its exit status.
fn engram_check_refs(check: &EngramCheckCommand, exit: EngramCommandExit) -> Vec<String> {
    let mut refs = Vec::new();
    let command = format!("command:{}", check.normalized);
    if command.len() <= ENGRAM_CHECK_REF_MAX_BYTES {
        refs.push(command);
    }
    if let EngramCommandExit::Code(code) = exit {
        refs.push(format!("exit:{code}"));
    }
    refs
}
