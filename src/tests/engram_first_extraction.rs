// The host component's facade boundary: source outside the Engram fragments
// reaches check machinery through EngramHost. The former byte-for-byte
// checkpoint equivalence oracle belonged to the removed admission protocol;
// it is not an ordinary observation or native verification contract.
use super::*;

/// The fragments that are the check machinery: what a check is, how it is
/// recognised, marked, carried and reported.
const CHECK_MACHINERY_FILES: [&str; 7] = [
    "engram_turn_checks.rs",
    "engram_carried_checks.rs",
    "engram_watcher_order.rs",
    "engram_reported_ends.rs",
    "engram_check_recognition.rs",
    "engram_launcher_stages.rs",
    "engram_one_call.rs",
];

/// The name of the function a line defines, if it defines one.
fn defined_function(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    // A definition, not prose: only qualifiers may precede `fn`.
    let name = loop {
        if let Some(name) = rest.strip_prefix("fn ") {
            break name;
        }
        let (word, after) = rest.split_once(' ')?;
        if !(word.starts_with("pub") || matches!(word, "async" | "const" | "unsafe")) {
            return None;
        }
        rest = after;
    };
    let end = name
        .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .unwrap_or(name.len());
    (end > 0).then(|| &name[..end])
}

/// Each use in `text` of a name in `machinery`, with its line number.
fn machinery_names_in(text: &str, machinery: &BTreeSet<String>) -> Vec<(usize, String)> {
    let mut named = Vec::new();
    for (index, line) in text.lines().enumerate() {
        for word in
            line.split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        {
            if machinery.contains(word) {
                named.push((index + 1, word.to_owned()));
            }
        }
    }
    named
}

#[test]
fn no_file_outside_the_component_names_a_function_of_the_check_machinery() {
    let source = FsPath::new(env!("CARGO_MANIFEST_DIR")).join("src");
    // Every function the machinery defines whose name says whose it is. A
    // method of one of its types with a general name (`refusal_line`) cannot
    // be told from another type's by its name; reaching those types is the
    // matter of the state they sit in, not of this boundary.
    let mut machinery = BTreeSet::new();
    for file in CHECK_MACHINERY_FILES {
        let text = fs::read_to_string(source.join(file))
            .unwrap_or_else(|error| panic!("{file} should be readable: {error}"));
        machinery.extend(
            text.lines()
                .filter_map(defined_function)
                .filter(|name| name.contains("engram"))
                .map(str::to_owned),
        );
    }
    assert!(
        machinery.contains("note_engram_command_started")
            && machinery.contains("engram_note_turn_started")
            && machinery.contains("poll_engram_carried_runs")
            && machinery.len() > 100,
        "the machinery's functions should be found: {} were",
        machinery.len()
    );
    // The scan finds a call and a mention alike, and nothing in a line that
    // names none.
    assert_eq!(
        machinery_names_in(
            "let x = 1;\n    state.note_engram_host_write(&path);\n// see `engram_note_turn_started`\n",
            &machinery
        ),
        [
            (2, "note_engram_host_write".to_owned()),
            (3, "engram_note_turn_started".to_owned())
        ]
    );

    // The component is the Engram fragments and the facade; its tests live
    // under src/tests and are not source files of the host.
    let mut named = Vec::new();
    for entry in fs::read_dir(&source).expect("the source directory should be readable") {
        let path = entry.expect("a source entry").path();
        let Some(file) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !path.is_file() || !file.ends_with(".rs") || file.starts_with("engram_") {
            continue;
        }
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{file} should be readable: {error}"));
        named.extend(
            machinery_names_in(&text, &machinery)
                .into_iter()
                .map(|(line, name)| format!("{file}:{line}: {name}")),
        );
    }
    assert!(
        named.is_empty(),
        "these lines name a function of the check machinery; reach it through \
         `EngramHost` (src/engram_host.rs):\n{}",
        named.join("\n")
    );
}
