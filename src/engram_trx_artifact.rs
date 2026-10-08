// The result artifact of a declared test command and its TRX reading
// (docs/features/declared-test-commands.md, "The artifact" and "Outcomes").
// Owns the artifact's start image, its inspection when the command ends (one
// regular file at the same canonical in-root path, untracked and ignored in
// the Git context the source basis uses, at most 4 MiB, read once as one
// image, different from the start image, modified at or after the start),
// the reading of one complete TRX `TestRun` document with no DTD, its
// `ResultSummary` and `Counters`, and the PASS, FAIL or UNKNOWN a declared
// check ends with, with the facts its record carries. Does not own the
// declaration and the match (`engram_declared_tests.rs`), or how a check's
// outcome is withheld, reported or told (`engram_turn_checks.rs`). New
// module beside `engram_declared_tests.rs`.

/// A larger artifact is UNKNOWN, at the start or at the end.
const ENGRAM_ARTIFACT_MAX_BYTES: usize = 4 * 1024 * 1024;
/// How much older than the check's start an artifact's modification time may
/// read and still count as written at or after it: a file system stamps
/// times from a coarser clock than the host's (FAT keeps two seconds), so a
/// file written just after the start can read as just before it. The hash
/// difference from the start image is the freshness rule; the time is a
/// heuristic on top of it, never proof.
const ENGRAM_ARTIFACT_CLOCK_SLACK: Duration = Duration::from_secs(2);
/// Elements deeper than this are not a TRX run TermAl reads.
const ENGRAM_TRX_MAX_DEPTH: usize = 256;

/// The artifact's image when a declared check starts: absent, its hash, or a
/// failed inspection (unreadable, not a regular file, too large), which is
/// not absence.
fn engram_artifact_start_image(root: &FsPath, artifact: &str) -> EngramArtifactStart {
    let path = root.join(artifact);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => EngramArtifactStart::Absent,
        Err(_) => EngramArtifactStart::Failed,
        Ok(_) => match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => {
                match engram_read_bounded(&path, ENGRAM_ARTIFACT_MAX_BYTES) {
                    Ok(Some(bytes)) => EngramArtifactStart::Present {
                        sha256: sha256_hex(&bytes),
                    },
                    _ => EngramArtifactStart::Failed,
                }
            }
            _ => EngramArtifactStart::Failed,
        },
    }
}

/// The artifact as its check's end read it, for the record.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramArtifactFacts {
    /// Root-relative, as declared.
    path: String,
    size: u64,
    sha256: String,
    modified: String,
}

/// The `Counters` of a TRX run, every attribute a real one carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct EngramTrxCounters {
    values: Vec<(String, u64)>,
}

impl EngramTrxCounters {
    fn get(&self, name: &str) -> u64 {
        self.values
            .iter()
            .find(|(counter, _)| counter == name)
            .map_or(0, |(_, value)| *value)
    }

    /// `total=3,executed=3,…`, the counters real files carry in their order;
    /// a name only the file gives is not repeated.
    fn describe(&self) -> String {
        ENGRAM_TRX_COUNTERS
            .iter()
            .map(|name| format!("{name}={}", self.get(name)))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// The counters every real TRX `Counters` element carries (from captured
/// `dotnet test --logger trx` runs); a file missing one is UNKNOWN.
const ENGRAM_TRX_COUNTERS: [&str; 16] = [
    "total",
    "executed",
    "passed",
    "failed",
    "error",
    "timeout",
    "aborted",
    "inconclusive",
    "passedButRunAborted",
    "notRunnable",
    "notExecuted",
    "disconnected",
    "warning",
    "completed",
    "inProgress",
    "pending",
];
/// Counters that say tests failed or the run was cut short: any of them
/// non-zero is FAIL.
const ENGRAM_TRX_FAILURE_COUNTERS: [&str; 5] =
    ["failed", "error", "timeout", "aborted", "passedButRunAborted"];
/// Counters whose meaning a PASS does not interpret: any of them non-zero,
/// or any counter not in `ENGRAM_TRX_COUNTERS`, keeps a run from PASS.
const ENGRAM_TRX_UNINTERPRETED_COUNTERS: [&str; 6] = [
    "notRunnable",
    "disconnected",
    "warning",
    "completed",
    "inProgress",
    "pending",
];

/// A TRX run's verdict before the exit status and the artifact rules.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramTrxVerdict {
    Pass,
    Fail(String),
    Unknown(String),
}

/// What a TRX document said: its verdict, its run outcome word and its
/// counters when it got that far.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramTrxReading {
    verdict: EngramTrxVerdict,
    outcome: Option<String>,
    counters: Option<EngramTrxCounters>,
}

/// Reads one complete, well-formed TRX `TestRun` document from `bytes` and
/// judges it from its direct `ResultSummary` and that one's direct
/// `Counters`. A DTD, a second root, an unclosed element or any XML error is
/// UNKNOWN; so is a missing or repeated `ResultSummary` or `Counters`, a
/// missing or non-numeric counter, and a document in another encoding than
/// UTF-8. A count is never defaulted.
///
/// FAIL: a run outcome of Failed, Error, Aborted, Timeout or
/// PassedButRunAborted, or a non-zero failed, error, timeout, aborted or
/// passedButRunAborted counter. PASS: the run outcome Completed, no run-level
/// error (`RunInfo` with outcome Error), the counters consistent as real files
/// are (the outcome counters sum to `executed`, which is at most `total`; a
/// skipped test is counted in neither), no uninterpreted counter non-zero,
/// and at least one test executed and passed. Anything else is UNKNOWN.
fn engram_read_trx(bytes: &[u8]) -> EngramTrxReading {
    let unknown = |reason: &str| EngramTrxReading {
        verdict: EngramTrxVerdict::Unknown(reason.to_owned()),
        outcome: None,
        counters: None,
    };
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let Ok(text) = std::str::from_utf8(bytes) else {
        return unknown("the TRX file is not UTF-8");
    };
    // Every character of the document, in text, attributes, comments and
    // names alike, must be one XML allows (`engram_xml_char`).
    if !text.chars().all(engram_xml_char) || !engram_xml_declaration_well_formed(text) {
        return unknown("the TRX file is not complete, well-formed XML");
    }
    let summary = match engram_trx_summary(text) {
        Ok(summary) => summary,
        Err(reason) => return unknown(reason),
    };
    let outcome = summary.outcome;
    let counters = summary.counters;
    let verdict = engram_trx_verdict(&outcome, &counters, summary.run_error);
    EngramTrxReading {
        verdict,
        outcome: Some(outcome),
        counters: Some(counters),
    }
}

/// What a TRX document's direct `ResultSummary` holds.
struct EngramTrxSummary {
    outcome: String,
    counters: EngramTrxCounters,
    run_error: bool,
}

fn engram_trx_summary(text: &str) -> Result<EngramTrxSummary, &'static str> {
    use quick_xml::events::Event;
    const MALFORMED: &str = "the TRX file is not complete, well-formed XML";
    let mut reader = quick_xml::Reader::from_str(text);
    // A comment holding `--` is not XML; quick-xml skips that check unless
    // asked. End names are checked against their start by default.
    reader.config_mut().check_comments = true;
    let mut open: Vec<String> = Vec::new();
    let mut rooted = false;
    let mut first = true;
    let mut outcome = None;
    let mut counters = None;
    let mut summaries = 0usize;
    let mut run_error = false;
    loop {
        let event = reader.read_event().map_err(|_| MALFORMED)?;
        let declaration_allowed = std::mem::take(&mut first);
        let (element, empty) = match &event {
            Event::Start(element) => (element, false),
            Event::Empty(element) => (element, true),
            Event::End(_) => {
                open.pop().ok_or(MALFORMED)?;
                continue;
            }
            Event::Decl(declaration) => {
                if !declaration_allowed {
                    return Err(MALFORMED);
                }
                match declaration.encoding() {
                    None => {}
                    Some(Ok(encoding)) if encoding.eq_ignore_ascii_case("utf-8") => {}
                    Some(Ok(_)) => {
                        return Err("the TRX file declares an encoding other than UTF-8");
                    }
                    Some(Err(_)) => return Err(MALFORMED),
                }
                continue;
            }
            Event::DocType(_) => return Err("the TRX file has a DTD"),
            Event::Eof => break,
            // Outside the root only XML's own whitespace (S) may stand.
            Event::Text(text) if open.is_empty() => {
                if text
                    .chars()
                    .all(|character| matches!(character, ' ' | '\t' | '\r' | '\n'))
                {
                    continue;
                }
                return Err(MALFORMED);
            }
            Event::Text(text) => {
                if text.contains("]]>") || !engram_xml_text_well_formed(text) {
                    return Err(MALFORMED);
                }
                continue;
            }
            // A processing instruction's target is a Name, and not `xml` in
            // any case, which only the declaration at the start may use.
            Event::PI(instruction) => {
                let target = instruction.target();
                if engram_xml_name(target) && !target.eq_ignore_ascii_case("xml") {
                    continue;
                }
                return Err(MALFORMED);
            }
            // With no DTD, only the five predefined entities and character
            // references are declared.
            Event::GeneralRef(reference) if !open.is_empty() => {
                if engram_xml_reference_name_declared(reference) {
                    continue;
                }
                return Err(MALFORMED);
            }
            Event::Comment(_) => continue,
            Event::CData(_) if !open.is_empty() => continue,
            _ => return Err(MALFORMED),
        };
        let name = element.name().as_ref().to_owned();
        // quick-xml does not check names; an end tag must match its start,
        // so checking every start covers the end tags too.
        if !engram_xml_name(&name) || !engram_xml_attributes_separated(element.attributes_raw()) {
            return Err(MALFORMED);
        }
        // Every attribute of every element is read, so a duplicate or
        // malformed one anywhere fails the document, not only where a value
        // is looked up.
        let attributes = engram_trx_attributes(element)?;
        if open.is_empty() {
            if rooted {
                return Err(MALFORMED);
            }
            rooted = true;
            if name != "TestRun" {
                return Err("the artifact is not a TRX TestRun document");
            }
        }
        // Only the root's direct ResultSummary, its direct Counters and the
        // RunInfo elements of its RunInfos count; the same names elsewhere
        // are not the run's summary.
        let in_summary = open.len() >= 2 && open[1] == "ResultSummary";
        let outcome_of = |attributes: &[(String, String)]| {
            attributes
                .iter()
                .find(|(key, _)| key == "outcome")
                .map(|(_, value)| value.clone())
                .ok_or("a TRX outcome is missing")
        };
        if name == "ResultSummary" && open.len() == 1 {
            summaries += 1;
            if summaries > 1 {
                return Err("the TRX file has more than one ResultSummary");
            }
            outcome = Some(outcome_of(&attributes)?);
        } else if name == "Counters" && open.len() == 2 && in_summary {
            if counters.is_some() {
                return Err("the TRX file has more than one Counters");
            }
            counters = Some(engram_trx_counters(attributes)?);
        } else if name == "RunInfo" && open.len() == 3 && in_summary && open[2] == "RunInfos" {
            run_error |= outcome_of(&attributes)? == "Error";
        }
        if !empty {
            if open.len() >= ENGRAM_TRX_MAX_DEPTH {
                return Err(MALFORMED);
            }
            open.push(name);
        }
    }
    if !rooted || !open.is_empty() {
        return Err(MALFORMED);
    }
    Ok(EngramTrxSummary {
        outcome: outcome.ok_or("the TRX file has no ResultSummary")?,
        counters: counters.ok_or("the TRX file has no Counters")?,
        run_error,
    })
}

/// Every attribute of `element` as written, each checked: a duplicate, one
/// without a quoted value, or a value holding `<` or an undeclared reference
/// fails the document. Values are kept raw: TRX writes its outcome words and
/// counts with no escapes, so an escaped one reads as unknown.
fn engram_trx_attributes(
    element: &quick_xml::events::BytesStart,
) -> Result<Vec<(String, String)>, &'static str> {
    const MALFORMED: &str = "the TRX file is not complete, well-formed XML";
    let mut attributes = Vec::new();
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|_| MALFORMED)?;
        let value = attribute.value.as_ref();
        let key = attribute.key.as_ref();
        if !engram_xml_name(key) || value.contains('<') || !engram_xml_text_well_formed(value) {
            return Err(MALFORMED);
        }
        attributes.push((key.to_owned(), value.to_owned()));
    }
    Ok(attributes)
}

/// Whether every `&` in `text` starts a declared reference ending in `;`.
fn engram_xml_text_well_formed(text: &str) -> bool {
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        let Some(end) = rest[at..].find(';') else {
            return false;
        };
        if !engram_xml_reference_name_declared(&rest[at + 1..at + end]) {
            return false;
        }
        rest = &rest[at + end + 1..];
    }
    true
}

/// Whether `name`, between `&` and `;`, is one a document with no DTD may
/// use: a predefined entity or a valid character reference.
fn engram_xml_reference_name_declared(name: &str) -> bool {
    if matches!(name, "lt" | "gt" | "amp" | "apos" | "quot") {
        return true;
    }
    let number = if let Some(hex) = name.strip_prefix("#x") {
        (!hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| u32::from_str_radix(hex, 16).ok())
            .flatten()
    } else if let Some(decimal) = name.strip_prefix('#') {
        (!decimal.is_empty() && decimal.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| decimal.parse::<u32>().ok())
            .flatten()
    } else {
        None
    };
    number.and_then(char::from_u32).is_some_and(engram_xml_char)
}

/// Whether XML 1.0 allows `character` in a document (its `Char` production).
fn engram_xml_char(character: char) -> bool {
    matches!(
        character,
        '\t' | '\n' | '\r'
            | '\u{20}'..='\u{D7FF}'
            | '\u{E000}'..='\u{FFFD}'
            | '\u{10000}'..='\u{10FFFF}'
    )
}

/// Whether `name` is an XML 1.0 `Name`: a `NameStartChar`, then
/// `NameChar`s. A prefixed name (`t:Note`) is one, the colon included.
fn engram_xml_name(name: &str) -> bool {
    let start = |character: char| {
        matches!(
            character,
            ':' | 'A'..='Z'
                | '_'
                | 'a'..='z'
                | '\u{C0}'..='\u{D6}'
                | '\u{D8}'..='\u{F6}'
                | '\u{F8}'..='\u{2FF}'
                | '\u{370}'..='\u{37D}'
                | '\u{37F}'..='\u{1FFF}'
                | '\u{200C}'..='\u{200D}'
                | '\u{2070}'..='\u{218F}'
                | '\u{2C00}'..='\u{2FEF}'
                | '\u{3001}'..='\u{D7FF}'
                | '\u{F900}'..='\u{FDCF}'
                | '\u{FDF0}'..='\u{FFFD}'
                | '\u{10000}'..='\u{EFFFF}'
        )
    };
    let mut characters = name.chars();
    characters.next().is_some_and(start)
        && characters.all(|character| {
            start(character)
                || matches!(
                    character,
                    '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}'
                )
        })
}

/// Whether an XML declaration at the start of `text`, if there is one,
/// follows XML 1.0's `XMLDecl`: `version` first and required (`1.` and
/// digits), then an optional `encoding` (an `EncName`), then an optional
/// `standalone` (`yes` or `no`), each value in matching quotes. quick-xml
/// does not check this grammar. A processing instruction whose target only
/// starts with `xml` (`<?xml-stylesheet …?>`) is not a declaration.
fn engram_xml_declaration_well_formed(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("<?xml") else {
        return true;
    };
    if !rest.starts_with([' ', '\t', '\r', '\n', '?']) {
        return true;
    }
    let Some(end) = rest.find("?>") else {
        return false;
    };
    let mut body = &rest[..end];
    let space = |body: &mut &str| {
        let trimmed = body.trim_start_matches([' ', '\t', '\r', '\n']);
        let skipped = trimmed.len() != body.len();
        *body = trimmed;
        skipped
    };
    // `S name Eq quoted-value`, the value checked by `valid`.
    let attribute = |body: &mut &str, name: &str, valid: &dyn Fn(&str) -> bool| {
        let mut rest = *body;
        if !space(&mut rest) {
            return false;
        }
        let Some(after) = rest.strip_prefix(name) else {
            return false;
        };
        rest = after;
        space(&mut rest);
        let Some(after) = rest.strip_prefix('=') else {
            return false;
        };
        rest = after;
        space(&mut rest);
        let Some(quote) = rest.chars().next().filter(|quote| matches!(quote, '"' | '\'')) else {
            return false;
        };
        let Some(close) = rest[1..].find(quote) else {
            return false;
        };
        if !valid(&rest[1..1 + close]) {
            return false;
        }
        *body = &rest[close + 2..];
        true
    };
    let version = |value: &str| {
        value
            .strip_prefix("1.")
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
    };
    let encoding = |value: &str| {
        let mut characters = value.chars();
        characters.next().is_some_and(|c| c.is_ascii_alphabetic())
            && characters.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    let standalone = |value: &str| matches!(value, "yes" | "no");
    if !attribute(&mut body, "version", &version) {
        return false;
    }
    let mut probe = body;
    if attribute(&mut probe, "encoding", &encoding) {
        body = probe;
    }
    let mut probe = body;
    if attribute(&mut probe, "standalone", &standalone) {
        body = probe;
    }
    space(&mut body);
    body.is_empty()
}

/// Whether the attributes of a start tag (`raw`, as written after its name)
/// are separated by whitespace, as XML requires and quick-xml does not
/// check: after each closing quote, only whitespace or the tag's end.
fn engram_xml_attributes_separated(raw: &str) -> bool {
    let mut quote = None;
    let mut characters = raw.chars().peekable();
    while let Some(character) = characters.next() {
        match quote {
            Some(open) if character == open => {
                quote = None;
                if !matches!(characters.peek(), None | Some(' ' | '\t' | '\r' | '\n')) {
                    return false;
                }
            }
            Some(_) => {}
            None if matches!(character, '"' | '\'') => quote = Some(character),
            None => {}
        }
    }
    true
}

/// The counters of a `Counters` element's checked attributes, each a decimal
/// count.
fn engram_trx_counters(
    attributes: Vec<(String, String)>,
) -> Result<EngramTrxCounters, &'static str> {
    let mut values = Vec::new();
    for (name, value) in attributes {
        let value = Some(value.as_str())
            .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or("a TRX counter is not a count")?;
        values.push((name, value));
    }
    let counters = EngramTrxCounters { values };
    if ENGRAM_TRX_COUNTERS
        .iter()
        .any(|required| !counters.values.iter().any(|(name, _)| name == required))
    {
        return Err("the TRX Counters lack a counter real files carry");
    }
    Ok(counters)
}

fn engram_trx_verdict(
    outcome: &str,
    counters: &EngramTrxCounters,
    run_error: bool,
) -> EngramTrxVerdict {
    let unknown = |reason: String| EngramTrxVerdict::Unknown(reason);
    if matches!(
        outcome,
        "Failed" | "Error" | "Aborted" | "Timeout" | "PassedButRunAborted"
    ) {
        return EngramTrxVerdict::Fail(format!("the TRX run outcome is {outcome}"));
    }
    if let Some(name) = ENGRAM_TRX_FAILURE_COUNTERS
        .iter()
        .find(|name| counters.get(name) > 0)
    {
        return EngramTrxVerdict::Fail(format!("the TRX counter {name} is {}", counters.get(name)));
    }
    if outcome != "Completed" {
        return unknown(format!(
            "the TRX run outcome is {}, not Completed",
            engram_trx_outcome_word(outcome)
        ));
    }
    if run_error {
        return unknown("the TRX run records a run-level error".to_owned());
    }
    let executed = counters.get("executed");
    let sum = counters
        .values
        .iter()
        .filter(|(name, _)| name != "total" && name != "executed")
        .try_fold(0u64, |sum, (_, value)| sum.checked_add(*value));
    if sum != Some(executed) || executed > counters.get("total") {
        return unknown("the TRX counters are inconsistent".to_owned());
    }
    if let Some((name, _)) = counters.values.iter().find(|(name, value)| {
        *value > 0
            && (ENGRAM_TRX_UNINTERPRETED_COUNTERS.contains(&name.as_str())
                || !ENGRAM_TRX_COUNTERS.contains(&name.as_str()))
    }) {
        let name = if ENGRAM_TRX_COUNTERS.contains(&name.as_str()) {
            name.as_str()
        } else {
            "TermAl does not know"
        };
        return unknown(format!("a TRX counter TermAl does not interpret is non-zero ({name})"));
    }
    if executed == 0 {
        return unknown("no tests executed".to_owned());
    }
    if counters.get("passed") == 0 {
        return unknown("the TRX run passed no test".to_owned());
    }
    EngramTrxVerdict::Pass
}

/// A declared check's result when its command ended: its verdict, why when
/// it is not PASS, and what its record carries.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramDeclaredResult {
    verdict: EngramTrxVerdict,
    artifact: Option<EngramArtifactFacts>,
    outcome: Option<String>,
    counters: Option<EngramTrxCounters>,
}

/// Judges a declared check whose command ended `exit`. Precedence: a
/// declaration changed since the start is UNKNOWN; otherwise a non-zero exit
/// is FAIL, artifact or not. In the one-call form a non-zero exit may come
/// from its `pushd` rather than the test; the built-in route reads that as
/// UNKNOWN unless the runner's result line shows the test ran
/// (`engram_check_exit_is_the_tests`), but the declared route accepts the
/// asymmetry: a false FAIL never grants credit. Otherwise the artifact rules
/// and then the TRX reading decide, and an exit
/// TermAl was not told is UNKNOWN. Reads the file system and runs Git, so it
/// runs off the state lock.
fn engram_declared_result(
    binding: &EngramDeclaredBinding,
    exit: EngramCommandExit,
) -> EngramDeclaredResult {
    let result = |verdict, artifact| EngramDeclaredResult {
        verdict,
        artifact,
        outcome: None,
        counters: None,
    };
    if !engram_declaration_unchanged(binding) {
        return result(
            EngramTrxVerdict::Unknown("the declaration changed while the command ran".to_owned()),
            None,
        );
    }
    let inspected = engram_inspect_artifact(binding);
    match exit {
        EngramCommandExit::Code(0) | EngramCommandExit::ReportedSuccess => {}
        EngramCommandExit::Code(code) => {
            let (artifact, reading) = match inspected {
                Ok((facts, bytes)) => (Some(facts), Some(engram_read_trx(&bytes))),
                Err(_) => (None, None),
            };
            return EngramDeclaredResult {
                verdict: EngramTrxVerdict::Fail(format!("the command exited {code}")),
                artifact,
                outcome: reading.as_ref().and_then(|reading| reading.outcome.clone()),
                counters: reading.and_then(|reading| reading.counters),
            };
        }
        EngramCommandExit::Unknown | EngramCommandExit::NotFinished => {
            return result(
                EngramTrxVerdict::Unknown("the command ended without an exit status".to_owned()),
                None,
            );
        }
    }
    let (facts, bytes) = match inspected {
        Ok(inspected) => inspected,
        Err(reason) => return result(EngramTrxVerdict::Unknown(reason.to_owned()), None),
    };
    let reading = engram_read_trx(&bytes);
    EngramDeclaredResult {
        verdict: reading.verdict,
        artifact: Some(facts),
        outcome: reading.outcome,
        counters: reading.counters,
    }
}

/// The artifact the command left, read once, with its facts, or the
/// observed reason it cannot count.
fn engram_inspect_artifact(
    binding: &EngramDeclaredBinding,
) -> Result<(EngramArtifactFacts, Vec<u8>), &'static str> {
    use std::io::Read;
    if binding.artifact_start == EngramArtifactStart::Failed {
        return Err("the artifact's start inspection failed");
    }
    let path = binding.root.join(&binding.entry.artifact);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err("the artifact is missing at the end");
        }
        Err(_) => return Err("the artifact cannot be read"),
        Ok(_) => {}
    }
    let canonical = fs::canonicalize(&path).map_err(|_| "the artifact is missing at the end")?;
    let root = fs::canonicalize(&binding.root).map_err(|_| "the artifact cannot be read")?;
    if !engram_path_within(
        &engram_exact_path_key(&canonical),
        &engram_exact_path_key(&root),
    ) {
        return Err("the artifact is outside the worktree");
    }
    if engram_exact_path_key(&canonical) != binding.artifact_canonical {
        return Err("the artifact is not at the canonical path its start bound");
    }
    // Windows refuses to open a directory as a file, so its type is told
    // before the open; the open file is checked again below.
    if !fs::metadata(&canonical).is_ok_and(|metadata| metadata.is_file()) {
        return Err("the artifact is not a regular file");
    }
    let mut file = fs::File::open(&canonical).map_err(|_| "the artifact cannot be read")?;
    let before = file.metadata().map_err(|_| "the artifact cannot be read")?;
    if !before.is_file() {
        return Err("the artifact is not a regular file");
    }
    if before.len() > ENGRAM_ARTIFACT_MAX_BYTES as u64 {
        return Err("the artifact is too large");
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(ENGRAM_ARTIFACT_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "the artifact cannot be read")?;
    if bytes.len() > ENGRAM_ARTIFACT_MAX_BYTES {
        return Err("the artifact is too large");
    }
    let after = file.metadata().map_err(|_| "the artifact cannot be read")?;
    let modified = after.modified().map_err(|_| "the artifact cannot be read")?;
    if after.len() != before.len()
        || bytes.len() as u64 != after.len()
        || before.modified().ok() != Some(modified)
    {
        return Err("the artifact changed while it was read");
    }
    let sha256 = sha256_hex(&bytes);
    if binding.artifact_start
        == (EngramArtifactStart::Present {
            sha256: sha256.clone(),
        })
    {
        return Err("the artifact is unchanged since the start");
    }
    if modified + ENGRAM_ARTIFACT_CLOCK_SLACK < binding.started {
        return Err("the artifact was modified before the command started");
    }
    engram_artifact_ignored(&root, &canonical)?;
    Ok((
        EngramArtifactFacts {
            path: binding.entry.artifact.clone(),
            size: bytes.len() as u64,
            sha256,
            modified: chrono::DateTime::<chrono::Utc>::from(modified)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        },
        bytes,
    ))
}

/// Whether the file at `canonical`, inside the worktree at `root`, is
/// untracked and ignored there, in the Git context of the source basis
/// (`content_revision.rs`): a path the basis lists (tracked, or untracked and
/// not ignored) is refused, and Git must list it as ignored.
fn engram_artifact_ignored(root: &FsPath, canonical: &FsPath) -> Result<(), &'static str> {
    const UNREAD: &str = "the artifact's ignore state cannot be read";
    let git = ReviewFreezeGit::new(root).map_err(|_| UNREAD)?;
    let relative = canonical
        .strip_prefix(&git.root)
        .map_err(|_| "the artifact is outside the worktree")?;
    let relative = relative.to_str().ok_or(UNREAD)?.replace('\\', "/");
    let pathspec = format!(":(literal){relative}");
    let listed = |args: &[&str]| -> Result<bool, &'static str> {
        let mut command = vec!["ls-files", "-z"];
        command.extend_from_slice(args);
        command.extend_from_slice(&["--", &pathspec]);
        let output = git.run(&command, false).map_err(|_| UNREAD)?;
        Ok(output
            .split(|byte| *byte == 0)
            .any(|path| path == relative.as_bytes()))
    };
    if listed(&["--cached", "--others", "--exclude-standard"])? {
        return Err("the artifact is not ignored, or it is tracked");
    }
    if !listed(&["--others", "--ignored", "--exclude-standard"])? {
        return Err("the artifact is not ignored, or it is tracked");
    }
    Ok(())
}

/// A run outcome word as TermAl repeats it: the file's word when it is a
/// short plain word, so text from the file never reaches a line or record.
fn engram_trx_outcome_word(outcome: &str) -> &str {
    if !outcome.is_empty()
        && outcome.len() <= 32
        && outcome.bytes().all(|byte| byte.is_ascii_alphabetic())
    {
        outcome
    } else {
        "unrecognised"
    }
}

/// The outcome a declared check's `result` reports, or `None` for a run
/// that only launched (a background command, never reported).
fn engram_declared_outcome(
    exit: EngramCommandExit,
    result: &EngramDeclaredResult,
) -> Option<EngramExecutionOutcome> {
    if exit == EngramCommandExit::NotFinished {
        return None;
    }
    Some(match result.verdict {
        EngramTrxVerdict::Pass => EngramExecutionOutcome::Succeeded,
        EngramTrxVerdict::Fail(_) => EngramExecutionOutcome::Failed,
        EngramTrxVerdict::Unknown(_) => EngramExecutionOutcome::Unknown,
    })
}

/// A declared check's verification summary within Engram's bound: its
/// verdict first, then its line, where it ran, how it ended and what its TRX
/// said. Only the command line is shortened to fit, so a long line never
/// cuts the verdict or the counters away.
fn engram_declared_summary(
    check: &EngramCheckCommand,
    binding: Option<&EngramDeclaredBinding>,
    exit: EngramCommandExit,
    result: &EngramDeclaredResult,
) -> String {
    let ended = match exit {
        EngramCommandExit::Code(code) => format!("exited {code}"),
        EngramCommandExit::ReportedSuccess => "succeeded (no exit status reported)".to_owned(),
        EngramCommandExit::Unknown | EngramCommandExit::NotFinished => {
            "ended without an exit status".to_owned()
        }
    };
    let verdict = match &result.verdict {
        // A pass names every test that did not pass, so nothing is hidden: a
        // skipped test is counted in `total` only, not in `executed`.
        EngramTrxVerdict::Pass => {
            let counters = result.counters.clone().unwrap_or_default();
            let mut pass = format!("PASS: {} passed", counters.get("passed"));
            let skipped = counters
                .get("total")
                .saturating_sub(counters.get("executed"));
            for (count, label) in [
                (counters.get("inconclusive"), "inconclusive"),
                (counters.get("notExecuted"), "not executed"),
                (skipped, "skipped"),
            ] {
                if count > 0 {
                    pass.push_str(&format!(", {count} {label}"));
                }
            }
            pass
        }
        EngramTrxVerdict::Fail(reason) => format!("FAIL: {reason}"),
        EngramTrxVerdict::Unknown(reason) => format!("UNKNOWN: {reason}"),
    };
    let mut tail = format!(
        " (cwd {}) {ended}",
        binding.map_or(".", |binding| binding.entry.cwd.as_str())
    );
    if let Some(outcome) = &result.outcome {
        tail.push_str(&format!("; TRX outcome {}", engram_trx_outcome_word(outcome)));
    }
    if let Some(counters) = &result.counters {
        tail.push_str(&format!("; counters {}", counters.describe()));
    }
    let head = format!("{verdict}. Declared test `");
    const CUT: &str = "...";
    let room = ENGRAM_CHECK_SUMMARY_MAX_BYTES.saturating_sub(head.len() + "`".len() + tail.len());
    let command = if check.normalized.len() <= room {
        check.normalized.clone()
    } else {
        format!(
            "{}{CUT}",
            engram_truncate_utf8(&check.normalized, room.saturating_sub(CUT.len()))
        )
    };
    let mut summary = format!("{head}{command}`{tail}");
    // Only a cwd longer than the bound itself can still overflow it.
    let keep = engram_truncate_utf8(&summary, ENGRAM_CHECK_SUMMARY_MAX_BYTES).len();
    summary.truncate(keep);
    summary
}

/// A declared check's verification references: its command and exit, as a
/// built-in's, then `kind:declared`, the entry's cwd, the declaration's hash
/// and size, the artifact's facts and the TRX outcome and counters. A
/// reference past Engram's bound is left out, as a built-in's command is.
fn engram_declared_refs(
    check: &EngramCheckCommand,
    binding: Option<&EngramDeclaredBinding>,
    exit: EngramCommandExit,
    result: &EngramDeclaredResult,
) -> Vec<String> {
    let mut refs = vec![format!("command:{}", check.normalized)];
    if let EngramCommandExit::Code(code) = exit {
        refs.push(format!("exit:{code}"));
    }
    refs.push("kind:declared".to_owned());
    if let Some(binding) = binding {
        refs.push(format!("declared-cwd:{}", binding.entry.cwd));
        refs.push(format!("declaration-sha256:{}", binding.declaration_sha256));
        refs.push(format!("declaration-bytes:{}", binding.declaration_bytes));
    }
    if let Some(artifact) = &result.artifact {
        refs.push(format!("artifact:{}", artifact.path));
        refs.push(format!("artifact-sha256:{}", artifact.sha256));
        refs.push(format!("artifact-bytes:{}", artifact.size));
        refs.push(format!("artifact-modified:{}", artifact.modified));
    }
    if let Some(outcome) = &result.outcome {
        refs.push(format!("trx-outcome:{}", engram_trx_outcome_word(outcome)));
    }
    if let Some(counters) = &result.counters {
        refs.push(format!("trx-counters:{}", counters.describe()));
    }
    refs.retain(|reference| reference.len() <= ENGRAM_CHECK_REF_MAX_BYTES);
    refs
}
