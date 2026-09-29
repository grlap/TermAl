// Where a session's command may write, for overlap marking, when TermAl
// cannot follow the command's shell: the places a lost shell may be in (where
// it was, and each absolute literal target of the directory changes that
// lost it) rather than every worktree on the host. A change TermAl cannot
// place exactly (a relative target, one it cannot read, one on a line that
// may repeat it, run it in another shell, or change the paths it names)
// still counts as anywhere. Owns a line's directory changes
// (`EngramLineChanges`, `engram_line_changes`), where they may lead
// (`EngramLostMove`, `engram_resolve_lost_move`, `engram_line_lost_move`) and
// the places a command may write in (`EngramCommandPlaces`,
// `engram_command_write_places`). Does not own following a shell
// (`engram_shell_move`, `EngramShellDirectory` in `engram_check_paths.rs`),
// where a test is credited (a lost shell credits none), or overlap marking
// itself (`engram_turn_checks.rs`). New module.

/// The directory changes a line names, as written (`engram_line_changes`),
/// before TermAl resolves where they lead (`engram_resolve_lost_move`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct EngramLineChanges {
    /// Each literal target, in the order the line names them.
    targets: Vec<String>,
    /// Whether a change may lead where no literal target says
    /// (`engram_line_changes`).
    hidden: bool,
}

/// Where a line's directory changes may take a shell, resolved
/// (`engram_resolve_lost_move`): the only form a lost shell keeps
/// (`EngramShellDirectory::lose`) and a command's worktrees take.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct EngramLostMove {
    /// The existing directories the changes lead to.
    targets: Vec<String>,
    /// Whether they may lead anywhere, in which case `targets` is empty.
    computed: bool,
}

/// Where a command may run and so write (`engram_command_write_places`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramCommandPlaces {
    /// Each directory, `None` for the session's workdir.
    directories: Vec<Option<String>>,
    /// Whether it may also run anywhere: a lost shell moved where TermAl
    /// cannot name.
    anywhere: bool,
    /// Whether these are the places the shell may be in, rather than one
    /// place TermAl knows it in: TermAl lost the shell, or another command's
    /// move is pending. A relative change of the command may then start from
    /// a place TermAl knows only as resolved, not as the shell spells it, and
    /// may lead anywhere.
    shell_lost: bool,
}

impl EngramCommandPlaces {
    /// The places of a command whose shell TermAl follows, or whose runtime
    /// reports where it runs.
    fn at(directories: Vec<Option<String>>) -> Self {
        Self {
            directories,
            anywhere: false,
            shell_lost: false,
        }
    }
}

/// Words that start a loop in bash, zsh or PowerShell, whose body may run a
/// directory change any number of times.
const ENGRAM_REPEATING_WORDS: [&str; 9] = [
    "for",
    "while",
    "until",
    "select",
    "repeat",
    "do",
    "foreach",
    "foreach-object",
    "%",
];

/// Programs that run a script of their own, in which a directory change
/// TermAl does not read may happen (`engram_line_hides_changes`).
const ENGRAM_SHELL_PROGRAMS: [&str; 9] = [
    "bash",
    "sh",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "pwsh",
    "powershell",
    "cmd",
];

/// Commands that make, move or remove a path, or change which worktree it
/// belongs to: run on the same line as a change of directory, before or
/// after it, they may make the target TermAl resolved before the line ran
/// lead elsewhere, or lie in another worktree (`engram_line_hides_changes`).
/// cmd's and PowerShell's aliases are included (`rd`, `erase`, `ren`, …), and
/// `find` counts with an action that deletes or runs a program.
const ENGRAM_PATH_CHANGING_PROGRAMS: [&str; 22] = [
    "ln",
    "mklink",
    "mv",
    "rm",
    "rmdir",
    "rd",
    "del",
    "erase",
    "move",
    "ren",
    "rename",
    "unlink",
    "subst",
    "junction",
    "new-item",
    "ni",
    "remove-item",
    "ri",
    "move-item",
    "mi",
    "rename-item",
    "rni",
];

/// Words that define a name the line may call to change directory where
/// TermAl does not read (an alias or a function, in bash, zsh or
/// PowerShell), or that run code TermAl does not read in the shell itself:
/// bash's `trap`, whose command runs later in the same shell (a DEBUG or
/// RETURN trap before each later command), cmd's `call` of a batch file,
/// PowerShell's `Invoke-Expression`, `Invoke-Command` and `Import-Module`. A
/// PowerShell script or a batch file the line runs by name
/// (`ENGRAM_IN_SHELL_SCRIPT_EXTENSIONS`) counts too.
const ENGRAM_DEFINING_WORDS: [&str; 14] = [
    "alias",
    "function",
    "trap",
    "set-alias",
    "new-alias",
    "sal",
    "nal",
    "call",
    "invoke-expression",
    "iex",
    "invoke-command",
    "icm",
    "import-module",
    "ipmo",
];

/// Extensions of scripts that run in the calling shell, so a change of
/// location in one moves that shell: PowerShell scripts and modules, and
/// cmd's batch files.
const ENGRAM_IN_SHELL_SCRIPT_EXTENSIONS: [&str; 4] = [".ps1", ".psm1", ".bat", ".cmd"];

/// Git subcommands that leave the work tree as it is. Any other (`checkout`,
/// `switch`, `reset`, `worktree`, `clone`, `-C DIR …`) may make, move or
/// remove paths, a tracked link or a worktree among them.
const ENGRAM_READ_ONLY_GIT: [&str; 20] = [
    "status",
    "log",
    "diff",
    "show",
    "rev-parse",
    "ls-files",
    "ls-tree",
    "grep",
    "blame",
    "describe",
    "shortlog",
    "cat-file",
    "branch",
    "tag",
    "remote",
    "fetch",
    "config",
    "reflog",
    "merge-base",
    "for-each-ref",
];

/// Programs that only read. Only these (a git subcommand in
/// `ENGRAM_READ_ONLY_GIT`, a change of directory itself, a bare shell
/// keyword) may run before a lost line's last change of directory without
/// making it one TermAl cannot place: any other program may re-point a link
/// or a junction its target passes through, or make or remove the target,
/// after TermAl resolved it (`engram_line_hides_changes`).
const ENGRAM_READ_ONLY_PROGRAMS: [&str; 32] = [
    "echo",
    "printf",
    "pwd",
    "ls",
    "dir",
    "cat",
    "type",
    "head",
    "tail",
    "grep",
    "rg",
    "findstr",
    "wc",
    "sort",
    "uniq",
    "true",
    "false",
    ":",
    "test",
    "[",
    "which",
    "where",
    "sleep",
    "date",
    "whoami",
    "get-location",
    "get-childitem",
    "gci",
    "get-content",
    "select-string",
    "write-output",
    "test-path",
];

/// Shell keywords, which may stand alone or before a command.
const ENGRAM_SHELL_KEYWORDS: [&str; 10] = [
    "then", "else", "elif", "if", "!", "time", "fi", "done", "esac", "do",
];

/// Whether the command `words` only reads (`ENGRAM_READ_ONLY_PROGRAMS`, a
/// read-only git subcommand, or a change of directory, which `eval` is not),
/// after any shell keywords before it. A reading program counts only named
/// by its bare word: one given by a path or with an extension (`./ls`,
/// `node_modules/.bin/cat`, `cat.exe`) may be any program.
fn engram_command_only_reads(words: &[String]) -> bool {
    let mut words = words
        .iter()
        .skip_while(|word| ENGRAM_SHELL_KEYWORDS.contains(&word.to_ascii_lowercase().as_str()));
    let Some(first) = words.next() else {
        return true;
    };
    let program = first.to_ascii_lowercase();
    ((engram_changes_directory(first) || engram_directory_shortcut(first))
        && engram_program_name(first) != "eval")
        || ENGRAM_READ_ONLY_PROGRAMS.contains(&program.as_str())
        || (program == "git"
            && words
                .next()
                .is_some_and(|subcommand| ENGRAM_READ_ONLY_GIT.contains(&subcommand.as_str())))
}

/// Words that may stand before a command without changing what it is: a
/// shell keyword or a wrapper (`then . env.sh`, `sudo . env.sh`). An
/// assignment may too (`engram_assignment_word`).
const ENGRAM_LEADING_WORDS: [&str; 16] = [
    "then", "else", "elif", "do", "if", "while", "until", "!", "time", "sudo", "env", "nohup",
    "command", "builtin", "exec", "xargs",
];

/// Where the word in command position stands among a command's `words`: the
/// first after any keyword, wrapper, flag or assignment
/// (`ENGRAM_LEADING_WORDS`, `engram_assignment_word`), as in
/// `time -p . env.sh` or `FOO=1 . env.sh`. A flag's separate value is taken
/// as the command (`root` in `sudo -u root ./setup.ps1`). That hides nothing
/// this position is read for: the wrappers whose flags take a value (`sudo`,
/// `env`, `xargs`, `exec`) start another program, so what they run is never
/// code of the current shell, a definition or a shortcut that could move it.
fn engram_command_position(words: &[String]) -> Option<usize> {
    words.iter().position(|word| {
        !ENGRAM_LEADING_WORDS.contains(&word.to_ascii_lowercase().as_str())
            && !engram_assignment_word(word)
            && !word.starts_with('-')
    })
}

/// Whether `word` is a shell assignment (`NAME=value`), which may stand
/// before a command to set its environment (`FOO=1 . env.sh`). A `cd` or
/// `chdir` before the `=` is not one: cmd reads `cd=..` as a change of
/// directory (`engram_directory_shortcut`).
fn engram_assignment_word(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !matches!(name.to_ascii_lowercase().as_str(), "cd" | "chdir")
            && name
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
    })
}

/// Whether `line` runs a command TermAl's reading of its words cannot see:
/// a command substitution (`$(…)`, a backtick) or a process substitution
/// (`<(…)`, `>(…)`), outside single quotes, since double quotes do not stop
/// one from running (`echo "$(ln -sfn …)"`). A brace outside any quotes
/// counts too: a function body or script block that may run any number of
/// times, or a brace expansion that gives a change several words. So does
/// any line whose quoting TermAl may read otherwise than the shell: one with
/// a comment (`#`), whose quotes do not quote, or with a backslash next to a
/// quote (`\'`, `\"`), which may or may not open one.
fn engram_line_runs_hidden_commands(line: &str) -> bool {
    if line.contains('#') || line.contains("\\'") || line.contains("\\\"") {
        return true;
    }
    let mut quote = None;
    let mut previous = None;
    for character in line.chars() {
        match (quote, character) {
            (None, '\'' | '"') => quote = Some(character),
            (Some(open), close) if open == close => quote = None,
            (Some('\''), _) => {}
            (_, '`') => return true,
            (_, '(') if matches!(previous, Some('$' | '<' | '>')) => return true,
            (None, '{') => return true,
            _ => {}
        }
        previous = Some(character);
    }
    false
}

/// Whether the line `line`, whose commands are `commands`, may take a
/// shell somewhere its literal targets do not say. Every word of every
/// command counts, not only a command's first, since a keyword or a wrapper
/// may stand before it (`then ln …`, `sudo mv …`):
/// - a substitution or a brace may run commands the words do not show
///   (`engram_line_runs_hidden_commands`);
/// - a loop may repeat a change (`ENGRAM_REPEATING_WORDS`);
/// - an alias or a function the line defines may change directory when
///   called, and code the shell runs in itself may too (`call`,
///   `Invoke-Expression`, a `.ps1` or `.bat` script), each named in command
///   position after any keyword, wrapper, flag or assignment
///   (`ENGRAM_DEFINING_WORDS`, `ENGRAM_IN_SHELL_SCRIPT_EXTENSIONS`);
/// - another shell or a sourced script may change directory where TermAl
///   does not read (`ENGRAM_SHELL_PROGRAMS`, `source`, and `.` as a command);
/// - a command known to change paths (`ENGRAM_PATH_CHANGING_PROGRAMS`, `find`
///   with `-delete` or `-exec`, a git subcommand outside
///   `ENGRAM_READ_ONLY_GIT`), anywhere on the line, may make a target lead
///   elsewhere than it did when TermAl resolved it, or lie in another
///   worktree. After the last change this list is best effort, as it is for
///   a shell TermAl follows: another program run there (a build script, a
///   mirroring copy) that removes a nested `.git` is not seen;
/// - and any command that does not only read (`engram_command_only_reads`)
///   before the line's last change of directory may have done so too, since
///   TermAl cannot know every program that re-points a link.
fn engram_line_hides_changes(line: &str, commands: &EngramShellCommands) -> bool {
    if engram_line_runs_hidden_commands(line) {
        return true;
    }
    let unknown_before_last_change =
        [&commands.written, &commands.bash]
            .into_iter()
            .any(|commands| {
                commands
                    .iter()
                    .rposition(|words| {
                        (0..words.len()).any(|index| engram_word_changes_directory(words, index))
                    })
                    .is_some_and(|last| {
                        commands[..last]
                            .iter()
                            .any(|words| !engram_command_only_reads(words))
                    })
            });
    unknown_before_last_change
        || commands.written.iter().chain(&commands.bash).any(|words| {
            // The word in command position (`engram_command_position`): what
            // a definition, `.`, `call` or an in-shell script must be to run,
            // so an argument that merely looks like one (`echo x.ps1`) does
            // not count.
            let command =
                engram_command_position(words).map(|index| words[index].to_ascii_lowercase());
            command.as_deref().is_some_and(|command| {
                command == "."
                    || ENGRAM_DEFINING_WORDS.contains(&command)
                    || ENGRAM_IN_SHELL_SCRIPT_EXTENSIONS
                        .iter()
                        .any(|extension| command.ends_with(extension))
            }) || words.iter().enumerate().any(|(index, word)| {
                let lower = word.to_ascii_lowercase();
                let program = engram_program_name(word);
                ENGRAM_REPEATING_WORDS.contains(&lower.as_str())
                    || program == "source"
                    || ENGRAM_SHELL_PROGRAMS.contains(&program.as_str())
                    || ENGRAM_PATH_CHANGING_PROGRAMS.contains(&program.as_str())
                    || (program == "find"
                        && words[index + 1..].iter().any(|action| {
                            matches!(
                                action.as_str(),
                                "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"
                            )
                        }))
                    || (program == "git"
                        && !words.get(index + 1).is_some_and(|subcommand| {
                            ENGRAM_READ_ONLY_GIT.contains(&subcommand.as_str())
                        }))
            })
        })
}

/// The directory changes on `line`, which `engram_shell_move` cannot follow
/// as one move (a change after another command, in a pipeline or group, or
/// more than one): every command word that changes directory, as written
/// and as bash reads it, with the word after it as its target. Only an
/// absolute literal target names a place TermAl can be sure of. Any other
/// change is hidden, and may lead anywhere: one whose target TermAl cannot
/// read (`cd $X`, `cd -`, a bare `cd`, `popd`, `eval`, a flag, PowerShell's
/// `cd..`, a drive switch), a relative one (a subshell or a branch that did
/// not run may leave the shell elsewhere to take it from, and a link may make
/// its `..` land elsewhere), one given more
/// than its target (cmd's `cd /d X`, zsh's `cd old new`), one bash reads
/// otherwise than it is written, and every change on a line that may hide
/// where they lead (`engram_line_hides_changes`).
fn engram_line_changes(line: &str) -> EngramLineChanges {
    let Some(commands) = engram_shell_commands(line) else {
        return EngramLineChanges {
            targets: Vec::new(),
            hidden: true,
        };
    };
    let read = |commands: &[Vec<String>]| {
        let mut changes = EngramLineChanges::default();
        for words in commands {
            for (index, word) in words.iter().enumerate() {
                if !engram_word_changes_directory(words, index) {
                    continue;
                }
                match words.get(index + 1) {
                    Some(target)
                        if index + 2 == words.len()
                            && engram_change_names_target(word)
                            && engram_literal_directory(target)
                            && FsPath::new(engram_msys_drive_path(target).as_ref())
                                .is_absolute() =>
                    {
                        changes.targets.push(target.clone());
                    }
                    _ => changes.hidden = true,
                }
            }
        }
        changes
    };
    let mut changes = read(&commands.written);
    let read_otherwise = changes != read(&commands.bash);
    changes.hidden |= read_otherwise || engram_line_hides_changes(line, &commands);
    changes
}

/// `changes`, a line's directory changes, resolved
/// (`engram_resolve_shell_move`): an absolute target once, a relative one (a
/// lone `cd` a command's own write places follow, `engram_command_worktrees`)
/// from each of `bases`, the places the command may start in (`None`, or no
/// base at all: one TermAl cannot name). Hidden changes, and a target that
/// does not resolve, make the move one that may lead anywhere: a relative
/// target of an unknown place, a network path, a `..` the shell may take
/// otherwise than the file system, or a path that does not exist yet, which
/// the line may make before it changes there. So does a move past
/// `ENGRAM_LOST_PLACES_LIMIT` places, as a lost shell kept among that many
/// may be. A move that may lead anywhere names no places, and nothing more
/// is resolved for it. Resolves on the file system, so never under the state
/// lock.
fn engram_resolve_lost_move(
    bases: &[Option<String>],
    changes: &EngramLineChanges,
) -> EngramLostMove {
    let anywhere = EngramLostMove {
        targets: Vec::new(),
        computed: true,
    };
    if changes.hidden {
        return anywhere;
    }
    let mut resolved = EngramLostMove::default();
    for target in &changes.targets {
        let absolute = FsPath::new(engram_msys_drive_path(target).as_ref()).is_absolute();
        let froms: Vec<Option<&str>> = if absolute || bases.is_empty() {
            vec![None]
        } else {
            bases.iter().map(Option::as_deref).collect()
        };
        for from in froms {
            if resolved.targets.len() >= ENGRAM_LOST_PLACES_LIMIT {
                return anywhere;
            }
            match engram_resolve_shell_move(from, target) {
                Some(place) if !resolved.targets.contains(&place) => resolved.targets.push(place),
                Some(_) => {}
                None => return anywhere,
            }
        }
    }
    resolved
}

/// What the line `ran`, read as `shell_move` (`engram_shell_move`), does to
/// a shell it loses, its changes resolved (`engram_resolve_lost_move`): for
/// a line TermAl cannot follow, and for a lone `cd` when TermAl does not know
/// the one place the shell is in (`followed` false: it lost the shell, or
/// another command's move is pending), which only an absolute target places.
/// `None` for any other. Resolves on the file system, so never under the
/// state lock.
fn engram_line_lost_move(
    shell_move: &EngramShellMove,
    ran: Option<&str>,
    followed: bool,
) -> Option<EngramLostMove> {
    let changes = match (shell_move, ran) {
        (EngramShellMove::Lost, Some(ran)) => engram_line_changes(ran),
        (EngramShellMove::To(to), _) if !followed => EngramLineChanges {
            targets: vec![to.clone()],
            hidden: false,
        },
        _ => return None,
    };
    Some(engram_resolve_lost_move(&[None], &changes))
}

/// The places a command of `record` with runtime key `key` may run in, and
/// so write in. While TermAl follows the shell, those it may run in
/// (`engram_command_directories`). Otherwise (TermAl lost the shell, or
/// another command's move is pending), with `shell_lost`, the
/// workdir, where the shell was, where a move pending now leads, and every
/// place the lost shell may be (`EngramShellDirectory::lost_among`); anywhere
/// as well when a change led where TermAl cannot name, and whenever the lost
/// shell names no place at all, which no loss leaves but which fails safe.
fn engram_command_write_places(
    record: &SessionRecord,
    key: &str,
    cwd: Option<&str>,
) -> EngramCommandPlaces {
    if let Some(directories) = engram_command_directories(record, key, cwd) {
        return EngramCommandPlaces::at(directories);
    }
    // `engram_command_directories` answers `None` only for a shell record of
    // the current runtime, so this is not reached today; it is kept so that
    // a change there fails safe (anywhere) rather than panics or narrows.
    let Some(shell) = record.engram.shell_directory.as_ref() else {
        return EngramCommandPlaces {
            directories: vec![None],
            anywhere: true,
            shell_lost: true,
        };
    };
    let mut directories = vec![None];
    directories.extend(shell.directory.iter().cloned().map(Some));
    directories.extend(shell.lost_among.iter().cloned().map(Some));
    let mut anywhere =
        shell.lost_unbounded || (shell.directory.is_none() && shell.lost_among.is_empty());
    if let Some((moving, target)) = &shell.pending
        && moving != key
    {
        match target {
            Some(target) => directories.push(Some(target.clone())),
            None => anywhere = true,
        }
    }
    EngramCommandPlaces {
        directories,
        anywhere,
        shell_lost: true,
    }
}
