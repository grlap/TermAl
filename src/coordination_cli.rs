/*
 * TermAl coordination CLI: `termal sessions ...` and `termal mailbox ...`.
 *
 * Owns: argument parsing for the coordination subcommands, the explicit caller
 * identity (`--as-session`) with its root-session guard, exit-code
 * classification, and the human-readable / `--json` rendering of results.
 *
 * Deliberately does not own: any HTTP, session-name resolution, idempotency,
 * safe-replay, FIFO or CAS-acknowledgement semantics. Every operation runs
 * through `TermalDelegationMcpBridge` in `src/delegation_mcp.rs` (the code
 * path behind the `termal_*` MCP tools), so the CLI is a thin client of the
 * loopback HTTP API, inherits the MCP contracts verbatim, and never opens the
 * live SQLite database.
 *
 * Provenance: new module for the MCP-less coordination fallback; nothing was
 * moved here from another file.
 */

const COORDINATION_CLI_USAGE: &str = "usage:
  termal sessions list [--as-session <id>] [--json] [--base-url <url>]
  termal mailbox list [--as-session <id>] [--json] [--base-url <url>]
  termal mailbox send [--as-session <id>] --to <session-id-or-name>
                      (--message <text> | --message-file <path or ->)
                      --idempotency-key <key> [--topic <text>]
                      [--state-stamp <text>] [--class <class>]
                      [--json] [--base-url <url>]
  termal mailbox read [--as-session <id>] --mailbox-id <id>
                      [--after <sequence>] [--limit <count>]
                      [--json] [--base-url <url>]
  termal mailbox read-message [--as-session <id>] --message-id <id>
                      [--json] [--base-url <url>]
  termal mailbox acknowledge [--as-session <id>] --mailbox-id <id>
                      --expected <processedThrough> --through <processedThrough>
                      [--json] [--base-url <url>]

Flags accept `--flag value` and `--flag=value`. `--as-session` is the root
session the command acts as and defaults to TERMAL_SESSION_ID; delegation-child
sessions are rejected exactly as they are for the peer MCP tools. `--base-url`
defaults to TERMAL_BASE_URL, then http://127.0.0.1:<TERMAL_PORT or 8787>.
Exit codes: 0 success; 2 usage or argument error (no request was sent);
1 any failure after a request was attempted (details on stderr).";

/// `termal sessions list` needs no caller identity: the inventory tool only
/// reads `/api/state` and never transmits the serving session id. The bridge
/// still requires a syntactically valid id, so this placeholder stands in when
/// `--as-session` is omitted.
const COORDINATION_CLI_INVENTORY_CALLER_ID: &str = "session-coordination-cli";

/// An argument problem detected before any request was sent. `main` maps it to
/// exit code 2 so scripts can tell "fix the invocation" from "the server or
/// the request failed" (exit code 1).
#[derive(Debug)]
struct CoordinationCliUsageError(String);

impl std::fmt::Display for CoordinationCliUsageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}\n\n{COORDINATION_CLI_USAGE}", self.0)
    }
}

impl std::error::Error for CoordinationCliUsageError {}

fn coordination_cli_usage_error(message: impl Into<String>) -> anyhow::Error {
    CoordinationCliUsageError(message.into()).into()
}

/// Process exit code for a top-level failure: usage errors are 2, everything
/// else (transport, backend rejection, unexpected response shape) is 1.
fn coordination_cli_exit_code(err: &anyhow::Error) -> i32 {
    if err.downcast_ref::<CoordinationCliUsageError>().is_some() {
        2
    } else {
        1
    }
}

/// Where the body of a `mailbox send` comes from. Resolved only after parsing
/// succeeds, so a bad file path is still a usage error and sends nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CoordinationCliMessageSource {
    Inline(String),
    File(PathBuf),
    Stdin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CoordinationCliCommand {
    Help,
    SessionsList {
        as_session: Option<String>,
    },
    MailboxList {
        as_session: String,
    },
    MailboxSend {
        as_session: String,
        to: String,
        message: CoordinationCliMessageSource,
        idempotency_key: String,
        topic: Option<String>,
        state_stamp: Option<String>,
        class: Option<String>,
    },
    MailboxRead {
        as_session: String,
        mailbox_id: String,
        after_sequence: Option<u64>,
        limit: Option<u64>,
    },
    MailboxReadMessage {
        as_session: String,
        message_id: String,
    },
    MailboxAcknowledge {
        as_session: String,
        mailbox_id: String,
        expected_processed_through: u64,
        processed_through: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CoordinationCliInvocation {
    command: CoordinationCliCommand,
    json: bool,
    base_url: Option<String>,
}

/// Named flag values collected before the command is known, so every command
/// shares one grammar (`--flag value`, `--flag=value`, duplicates rejected)
/// and reports leftovers it does not understand.
#[derive(Default)]
struct CoordinationCliFlags {
    values: BTreeMap<String, String>,
}

impl CoordinationCliFlags {
    fn insert(&mut self, name: &str, value: String) -> Result<()> {
        if self.values.insert(name.to_owned(), value).is_some() {
            return Err(coordination_cli_usage_error(format!(
                "`{name}` was given more than once"
            )));
        }
        Ok(())
    }

    /// Identifier-like values are trimmed, matching what the bridge does with
    /// the same fields; message text is taken raw elsewhere.
    fn take_optional(&mut self, name: &str) -> Option<String> {
        self.values
            .remove(name)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    }

    fn take_required(&mut self, name: &str) -> Result<String> {
        self.take_optional(name)
            .ok_or_else(|| coordination_cli_usage_error(format!("`{name}` is required")))
    }

    fn take_optional_u64(&mut self, name: &str) -> Result<Option<u64>> {
        self.take_optional(name)
            .map(|value| {
                value.trim().parse::<u64>().map_err(|_| {
                    coordination_cli_usage_error(format!(
                        "`{name}` must be a non-negative integer, got `{value}`"
                    ))
                })
            })
            .transpose()
    }

    fn take_required_u64(&mut self, name: &str) -> Result<u64> {
        self.take_optional_u64(name)?
            .ok_or_else(|| coordination_cli_usage_error(format!("`{name}` is required")))
    }

    fn finish(self, command: &str) -> Result<()> {
        if self.values.is_empty() {
            return Ok(());
        }
        let unexpected = self.values.keys().cloned().collect::<Vec<_>>().join(", ");
        Err(coordination_cli_usage_error(format!(
            "`{command}` does not accept {unexpected}"
        )))
    }
}

/// Tokens the parser treats as options rather than values: long flags and
/// the recognized short help alias.
fn is_coordination_cli_option_token(argument: &str) -> bool {
    argument.starts_with("--") || argument == "-h"
}

/// Splits `--name=value` into its parts; every other argument is returned
/// unchanged with no inline value.
fn split_coordination_cli_flag(argument: &str) -> (String, Option<String>) {
    if argument.starts_with("--") {
        if let Some((name, value)) = argument.split_once('=') {
            return (name.to_owned(), Some(value.to_owned()));
        }
    }
    (argument.to_owned(), None)
}

fn coordination_cli_help() -> CoordinationCliInvocation {
    CoordinationCliInvocation {
        command: CoordinationCliCommand::Help,
        json: false,
        base_url: None,
    }
}

fn default_termal_session_id() -> Option<String> {
    std::env::var(TERMAL_SESSION_ID_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn take_optional_coordination_cli_session_id(
    flags: &mut CoordinationCliFlags,
    default_session_id: Option<&str>,
) -> Result<Option<String>> {
    match flags.values.remove("--as-session") {
        Some(value) => {
            let value = value.trim();
            if value.is_empty() {
                Err(coordination_cli_usage_error("`--as-session` is empty"))
            } else {
                Ok(Some(value.to_owned()))
            }
        }
        None => Ok(default_session_id.map(str::to_owned)),
    }
}

fn take_coordination_cli_session_id(
    flags: &mut CoordinationCliFlags,
    default_session_id: Option<&str>,
) -> Result<String> {
    take_optional_coordination_cli_session_id(flags, default_session_id)?
        .ok_or_else(|| {
            coordination_cli_usage_error(
                "`--as-session` is required when TERMAL_SESSION_ID is unavailable",
            )
        })
}

/// Parses `sessions ...` / `mailbox ...` arguments (the group word included).
/// Every rejection is a `CoordinationCliUsageError`; nothing here performs I/O.
fn parse_coordination_cli_args(
    args: impl IntoIterator<Item = String>,
) -> Result<CoordinationCliInvocation> {
    parse_coordination_cli_args_with_default_session_id(args, default_termal_session_id())
}

fn parse_coordination_cli_args_with_default_session_id(
    args: impl IntoIterator<Item = String>,
    default_session_id: Option<String>,
) -> Result<CoordinationCliInvocation> {
    let mut args = args.into_iter();
    let group = args.next().unwrap_or_default();
    let verb = args.next().unwrap_or_default();
    if matches!(verb.as_str(), "--help" | "-h") {
        return Ok(coordination_cli_help());
    }
    let mut flags = CoordinationCliFlags::default();
    let mut json = false;
    let mut base_url: Option<String> = None;
    while let Some(argument) = args.next() {
        if matches!(argument.as_str(), "--help" | "-h") {
            return Ok(coordination_cli_help());
        }
        let (name, inline_value) = split_coordination_cli_flag(&argument);
        if !name.starts_with("--") {
            return Err(coordination_cli_usage_error(format!(
                "unexpected argument `{argument}`"
            )));
        }
        if name == "--json" {
            if inline_value.is_some() {
                return Err(coordination_cli_usage_error("`--json` takes no value"));
            }
            if json {
                return Err(coordination_cli_usage_error(
                    "`--json` was given more than once",
                ));
            }
            json = true;
            continue;
        }
        // A following option token is never a value: `--as-session --json`
        // must fail here instead of sending `--json` as a session id, and a
        // help request anywhere still wins. Values that genuinely look like
        // options use the `--flag=value` form.
        let value = match inline_value {
            Some(value) => value,
            None => match args.next() {
                Some(help) if matches!(help.as_str(), "--help" | "-h") => {
                    return Ok(coordination_cli_help());
                }
                Some(value) if !is_coordination_cli_option_token(&value) => value,
                Some(option) => {
                    return Err(coordination_cli_usage_error(format!(
                        "`{name}` requires a value, but the next argument is the option \
                         `{option}`; use `{name}=<value>` for values that look like options"
                    )));
                }
                None => {
                    return Err(coordination_cli_usage_error(format!(
                        "`{name}` requires a value"
                    )));
                }
            },
        };
        if name == "--base-url" {
            if base_url.replace(value).is_some() {
                return Err(coordination_cli_usage_error(
                    "`--base-url` was given more than once",
                ));
            }
            continue;
        }
        flags.insert(&name, value)?;
    }

    let command_label = format!("{group} {verb}");
    let command = match (group.as_str(), verb.as_str()) {
        ("sessions", "list") => CoordinationCliCommand::SessionsList {
            as_session: take_optional_coordination_cli_session_id(
                &mut flags,
                default_session_id.as_deref(),
            )?,
        },
        ("mailbox", "list") => CoordinationCliCommand::MailboxList {
            as_session: take_coordination_cli_session_id(
                &mut flags,
                default_session_id.as_deref(),
            )?,
        },
        ("mailbox", "send") => {
            let as_session = take_coordination_cli_session_id(
                &mut flags,
                default_session_id.as_deref(),
            )?;
            let to = flags.take_required("--to")?;
            let inline = flags.values.remove("--message");
            let file = flags.take_optional("--message-file");
            let message = match (inline, file) {
                (Some(_), Some(_)) => {
                    return Err(coordination_cli_usage_error(
                        "pass either `--message` or `--message-file`, not both",
                    ));
                }
                (None, None) => {
                    return Err(coordination_cli_usage_error(
                        "`--message <text>` or `--message-file <path or ->` is required",
                    ));
                }
                (Some(text), None) => {
                    if text.trim().is_empty() {
                        return Err(coordination_cli_usage_error("`--message` is empty"));
                    }
                    CoordinationCliMessageSource::Inline(text)
                }
                (None, Some(path)) if path.trim() == "-" => CoordinationCliMessageSource::Stdin,
                (None, Some(path)) => CoordinationCliMessageSource::File(PathBuf::from(path)),
            };
            let class = flags.take_optional("--class");
            if let Some(class) = &class {
                // The mailbox schema admits exactly one class today; reject
                // anything else here instead of after a round trip.
                if class != "routine" {
                    return Err(coordination_cli_usage_error(format!(
                        "`--class` must be `routine`, got `{class}`"
                    )));
                }
            }
            CoordinationCliCommand::MailboxSend {
                as_session,
                to,
                message,
                idempotency_key: flags.take_required("--idempotency-key")?,
                topic: flags.take_optional("--topic"),
                state_stamp: flags.take_optional("--state-stamp"),
                class,
            }
        }
        ("mailbox", "read") => CoordinationCliCommand::MailboxRead {
            as_session: take_coordination_cli_session_id(
                &mut flags,
                default_session_id.as_deref(),
            )?,
            mailbox_id: flags.take_required("--mailbox-id")?,
            after_sequence: flags.take_optional_u64("--after")?,
            limit: flags.take_optional_u64("--limit")?,
        },
        ("mailbox", "read-message") => CoordinationCliCommand::MailboxReadMessage {
            as_session: take_coordination_cli_session_id(
                &mut flags,
                default_session_id.as_deref(),
            )?,
            message_id: flags.take_required("--message-id")?,
        },
        ("mailbox", "acknowledge") => CoordinationCliCommand::MailboxAcknowledge {
            as_session: take_coordination_cli_session_id(
                &mut flags,
                default_session_id.as_deref(),
            )?,
            mailbox_id: flags.take_required("--mailbox-id")?,
            expected_processed_through: flags.take_required_u64("--expected")?,
            processed_through: flags.take_required_u64("--through")?,
        },
        ("sessions", "") | ("mailbox", "") => {
            return Err(coordination_cli_usage_error(format!(
                "`termal {group}` needs a subcommand"
            )));
        }
        _ => {
            return Err(coordination_cli_usage_error(format!(
                "unknown command `termal {command_label}`"
            )));
        }
    };
    flags.finish(&command_label)?;
    Ok(CoordinationCliInvocation {
        command,
        json,
        base_url,
    })
}

/// Reads at most the backend body cap plus one byte, so an oversized file or
/// stream is refused here without being buffered whole.
fn read_coordination_cli_message_bounded(reader: impl std::io::Read, label: &str) -> Result<String> {
    let mut bytes = Vec::new();
    let mut limited = std::io::Read::take(reader, MAX_MAILBOX_BODY_BYTES as u64 + 1);
    std::io::Read::read_to_end(&mut limited, &mut bytes)
        .map_err(|err| coordination_cli_usage_error(format!("failed to read {label}: {err}")))?;
    if bytes.len() > MAX_MAILBOX_BODY_BYTES {
        return Err(coordination_cli_usage_error(format!(
            "{label} exceeds the mailbox body limit of {MAX_MAILBOX_BODY_BYTES} bytes"
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| coordination_cli_usage_error(format!("{label} is not valid UTF-8")))
}

/// Reads the message body for `mailbox send`. Files and stdin are read through
/// the bounded reader; a leading UTF-8 byte-order mark (common in files written
/// by Windows editors) is dropped; an empty body or one above the backend cap
/// is a usage error because the backend would reject it anyway, and nothing is
/// sent.
fn resolve_coordination_cli_message(source: &CoordinationCliMessageSource) -> Result<String> {
    let text = match source {
        CoordinationCliMessageSource::Inline(text) => text.clone(),
        CoordinationCliMessageSource::File(path) => {
            let label = format!("--message-file `{}`", path.display());
            let file = fs::File::open(path).map_err(|err| {
                coordination_cli_usage_error(format!("failed to read {label}: {err}"))
            })?;
            read_coordination_cli_message_bounded(file, &label)?
        }
        CoordinationCliMessageSource::Stdin => {
            read_coordination_cli_message_bounded(io::stdin().lock(), "the message from stdin")?
        }
    };
    let text = text
        .strip_prefix('\u{feff}')
        .map(str::to_owned)
        .unwrap_or(text);
    if text.trim().is_empty() {
        return Err(coordination_cli_usage_error("the message is empty"));
    }
    if text.len() > MAX_MAILBOX_BODY_BYTES {
        return Err(coordination_cli_usage_error(format!(
            "the message is {} bytes; the mailbox body limit is {MAX_MAILBOX_BODY_BYTES} bytes",
            text.len()
        )));
    }
    Ok(text)
}

fn coordination_cli_bridge(
    as_session: &str,
    base_url: &str,
) -> Result<TermalDelegationMcpBridge> {
    // The bridge validates the id as a path segment; a rejection here is an
    // argument problem, not a request failure.
    TermalDelegationMcpBridge::new(as_session.to_owned(), base_url.to_owned())
        .map_err(|err| coordination_cli_usage_error(format!("{err:#}")))
}

/// Mirrors the peer-tool gate of the MCP bridge with exact reasons: a
/// delegation child is refused, an unknown caller is refused, and an
/// unreachable backend is reported as such instead of being mistaken for a
/// child.
fn ensure_coordination_cli_root_caller(
    bridge: &TermalDelegationMcpBridge,
    as_session: &str,
    base_url: &str,
) -> Result<()> {
    match bridge.caller_classification() {
        Ok(classification) if classification.is_delegation_child => bail!(
            "`{as_session}` is a delegation-child session; coordination commands are \
             restricted to root sessions, exactly like the peer MCP tools"
        ),
        Ok(_) => Ok(()),
        Err(CallerClassificationFailure::BackendUnavailable(detail)) => bail!(
            "TermAl server at `{base_url}` is unreachable or returned an unusable state \
             snapshot: {detail}"
        ),
        Err(CallerClassificationFailure::SessionNotVisible) => bail!(
            "`{as_session}` is not a session known to the TermAl server at `{base_url}`; \
             run `termal sessions list` to see the root session ids"
        ),
    }
}

/// The only way a mailbox command obtains a bridge: construction and the
/// root-caller guard are one step, so no code path can reach a mailbox
/// request with an unchecked identity.
fn coordination_cli_authorized_bridge(
    as_session: &str,
    base_url: &str,
) -> Result<TermalDelegationMcpBridge> {
    let bridge = coordination_cli_bridge(as_session, base_url)?;
    ensure_coordination_cli_root_caller(&bridge, as_session, base_url)?;
    Ok(bridge)
}

/// Rejects a successful response whose shape is not what the corresponding
/// MCP tool promises, so a malformed 2xx cannot exit 0 as an empty listing or
/// a row of dashes. Mailbox payloads are checked with the wire types
/// themselves; the session inventory is checked field by field because the
/// bridge builds it from a state snapshot.
fn validate_coordination_cli_output(
    command: &CoordinationCliCommand,
    output: &Value,
) -> Result<()> {
    let unusable =
        |detail: String| anyhow!("the server returned an unusable response: {detail}");
    match command {
        CoordinationCliCommand::Help => Ok(()),
        CoordinationCliCommand::SessionsList { .. } => {
            let sessions = output
                .get("sessions")
                .and_then(Value::as_array)
                .ok_or_else(|| unusable("`sessions` is missing or not an array".to_owned()))?;
            for (index, session) in sessions.iter().enumerate() {
                let has_id = session
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.trim().is_empty());
                if !has_id {
                    return Err(unusable(format!("sessions[{index}] has no sessionId")));
                }
                // The bridge emits every attribute key, null when the state
                // snapshot lacks it; a missing key means the contract broke.
                for key in ["name", "agent", "status", "workdir", "preview"] {
                    if !matches!(
                        session.get(key),
                        Some(Value::Null) | Some(Value::String(_))
                    ) {
                        return Err(unusable(format!(
                            "sessions[{index}].{key} is missing or not a string"
                        )));
                    }
                }
            }
            Ok(())
        }
        CoordinationCliCommand::MailboxList { .. } => {
            let mailboxes = output
                .get("mailboxes")
                .cloned()
                .ok_or_else(|| unusable("`mailboxes` is missing".to_owned()))?;
            serde_json::from_value::<Vec<MailboxSummary>>(mailboxes)
                .map(|_| ())
                .map_err(|err| unusable(format!("mailboxes: {err}")))
        }
        CoordinationCliCommand::MailboxSend { .. } => {
            serde_json::from_value::<MailboxAppendReceipt>(output.clone())
                .map_err(|err| unusable(format!("send receipt: {err}")))?;
            let has_target = output
                .get("sessionId")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.trim().is_empty());
            if !has_target {
                return Err(unusable("send receipt has no sessionId".to_owned()));
            }
            Ok(())
        }
        CoordinationCliCommand::MailboxRead { .. } => {
            if !output
                .get("mailboxId")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.trim().is_empty())
            {
                return Err(unusable("`mailboxId` is missing".to_owned()));
            }
            let messages = output
                .get("messages")
                .cloned()
                .ok_or_else(|| unusable("`messages` is missing".to_owned()))?;
            serde_json::from_value::<Vec<MailboxMessage>>(messages)
                .map(|_| ())
                .map_err(|err| unusable(format!("messages: {err}")))
        }
        CoordinationCliCommand::MailboxReadMessage { .. } => {
            serde_json::from_value::<MailboxMessage>(output.clone())
                .map(|_| ())
                .map_err(|err| unusable(format!("message: {err}")))
        }
        CoordinationCliCommand::MailboxAcknowledge { .. } => {
            serde_json::from_value::<MailboxSummary>(output.clone())
                .map(|_| ())
                .map_err(|err| unusable(format!("mailbox summary: {err}")))
        }
    }
}

/// Backend diagnostics can quote peer-controlled text (a hostile message may
/// be echoed inside a server error), so every failure that is not one of our
/// own usage errors is flattened to a single sanitized line before `main`
/// prints it on stderr. Usage errors keep their multi-line usage text.
fn sanitize_coordination_cli_failure(err: anyhow::Error) -> anyhow::Error {
    if err.downcast_ref::<CoordinationCliUsageError>().is_some() {
        return err;
    }
    anyhow!(
        "{}",
        sanitize_coordination_cli_text(&format!("{err:#}"), false)
    )
}

/// A consumer that stopped reading (`termal ... | head`) is not a failure.
fn coordination_cli_error_is_broken_pipe(err: &anyhow::Error) -> bool {
    err.downcast_ref::<io::Error>()
        .is_some_and(|io_error| io_error.kind() == io::ErrorKind::BrokenPipe)
        || err
            .downcast_ref::<serde_json::Error>()
            .and_then(serde_json::Error::io_error_kind)
            .is_some_and(|kind| kind == io::ErrorKind::BrokenPipe)
}

/// Executes one command against `base_url` and returns the same JSON the
/// corresponding MCP tool would return.
fn execute_coordination_cli(command: &CoordinationCliCommand, base_url: &str) -> Result<Value> {
    match command {
        CoordinationCliCommand::Help => Ok(json!({ "usage": COORDINATION_CLI_USAGE })),
        CoordinationCliCommand::SessionsList { as_session } => {
            let caller = as_session
                .as_deref()
                .unwrap_or(COORDINATION_CLI_INVENTORY_CALLER_ID);
            let bridge = coordination_cli_bridge(caller, base_url)?;
            if let Some(as_session) = as_session {
                ensure_coordination_cli_root_caller(&bridge, as_session, base_url)?;
            }
            bridge.tool_list_sessions(json!({}))
        }
        CoordinationCliCommand::MailboxList { as_session } => {
            let bridge = coordination_cli_authorized_bridge(as_session, base_url)?;
            bridge.tool_list_mailboxes(json!({}))
        }
        CoordinationCliCommand::MailboxSend {
            as_session,
            to,
            message,
            idempotency_key,
            topic,
            state_stamp,
            class,
        } => {
            // Resolve the body before touching the network so a bad file path
            // sends nothing and stays a usage error.
            let message = resolve_coordination_cli_message(message)?;
            let bridge = coordination_cli_authorized_bridge(as_session, base_url)?;
            let mut arguments = serde_json::Map::new();
            arguments.insert("sessionId".to_owned(), Value::String(to.clone()));
            arguments.insert("message".to_owned(), Value::String(message));
            arguments.insert(
                "idempotencyKey".to_owned(),
                Value::String(idempotency_key.clone()),
            );
            for (name, value) in [
                ("topic", topic),
                ("stateStamp", state_stamp),
                ("class", class),
            ] {
                if let Some(value) = value {
                    arguments.insert(name.to_owned(), Value::String(value.clone()));
                }
            }
            bridge.tool_send_to_session(Value::Object(arguments))
        }
        CoordinationCliCommand::MailboxRead {
            as_session,
            mailbox_id,
            after_sequence,
            limit,
        } => {
            let bridge = coordination_cli_authorized_bridge(as_session, base_url)?;
            let mut arguments = serde_json::Map::new();
            arguments.insert("mailboxId".to_owned(), Value::String(mailbox_id.clone()));
            if let Some(after_sequence) = after_sequence {
                arguments.insert("afterSequence".to_owned(), json!(after_sequence));
            }
            if let Some(limit) = limit {
                arguments.insert("limit".to_owned(), json!(limit));
            }
            bridge.tool_read_mailbox(Value::Object(arguments))
        }
        CoordinationCliCommand::MailboxReadMessage {
            as_session,
            message_id,
        } => {
            let bridge = coordination_cli_authorized_bridge(as_session, base_url)?;
            bridge.tool_read_mailbox_message(json!({ "messageId": message_id }))
        }
        CoordinationCliCommand::MailboxAcknowledge {
            as_session,
            mailbox_id,
            expected_processed_through,
            processed_through,
        } => {
            let bridge = coordination_cli_authorized_bridge(as_session, base_url)?;
            bridge.tool_acknowledge_mailbox(json!({
                "mailboxId": mailbox_id,
                "expectedProcessedThrough": expected_processed_through,
                "processedThrough": processed_through,
            }))
        }
    }
}

/// Strips terminal control characters from peer- or server-controlled text
/// before it reaches a terminal. Mailbox bodies, topics, previews and names
/// are untrusted input, and the backend accepts control bytes in them, so a
/// hostile peer could otherwise smuggle ESC/CSI/OSC sequences (title changes,
/// hyperlinks, cursor games) into the operator's terminal. Every control
/// character — C0, DEL and C1 alike, so no escape introducer survives —
/// becomes U+FFFD; only a message body keeps its newlines and tabs, because
/// it is printed as a block of its own. Metadata rendered on a single line
/// (topics, stamps, previews, names, ids, paths) loses them too, so a peer
/// cannot forge extra lines or columns in the human output. `--json` output
/// is unaffected: serde escapes it.
fn sanitize_coordination_cli_text(text: &str, keep_line_structure: bool) -> String {
    text.chars()
        .map(|ch| {
            if (keep_line_structure && (ch == '\n' || ch == '\t')) || !ch.is_control() {
                ch
            } else {
                '\u{FFFD}'
            }
        })
        .collect()
}

/// Single-line metadata rendering of a JSON field: control characters,
/// newlines and tabs included, become U+FFFD.
fn coordination_cli_field(value: &Value, key: &str) -> String {
    match value.get(key) {
        None | Some(Value::Null) => "-".to_owned(),
        Some(Value::String(text)) => sanitize_coordination_cli_text(text, false),
        Some(other) => sanitize_coordination_cli_text(&other.to_string(), false),
    }
}

/// Block rendering of a message body: line structure is kept, every other
/// control character becomes U+FFFD.
fn coordination_cli_body(message: &Value) -> String {
    match message.get("body") {
        Some(Value::String(text)) => sanitize_coordination_cli_text(text, true),
        _ => "-".to_owned(),
    }
}

fn render_coordination_cli_message(message: &Value, out: &mut impl Write) -> Result<()> {
    writeln!(
        out,
        "#{} {} from {} ({}) to {} ({}) [{}]",
        coordination_cli_field(message, "sequence"),
        coordination_cli_field(message, "createdAt"),
        coordination_cli_field(message, "senderName"),
        coordination_cli_field(message, "senderSessionId"),
        coordination_cli_field(message, "targetName"),
        coordination_cli_field(message, "targetSessionId"),
        coordination_cli_field(message, "class"),
    )?;
    writeln!(out, "  id: {}", coordination_cli_field(message, "id"))?;
    if message.get("topic").and_then(Value::as_str).is_some() {
        writeln!(out, "  topic: {}", coordination_cli_field(message, "topic"))?;
    }
    if message.get("stateStamp").and_then(Value::as_str).is_some() {
        writeln!(
            out,
            "  stateStamp: {}",
            coordination_cli_field(message, "stateStamp")
        )?;
    }
    writeln!(
        out,
        "  notificationState: {}",
        coordination_cli_field(message, "notificationState")
    )?;
    writeln!(out)?;
    writeln!(out, "{}", coordination_cli_body(message))?;
    writeln!(out)?;
    Ok(())
}

/// Concise human rendering used when `--json` is absent. The JSON form is the
/// stable contract; this text may change.
fn render_coordination_cli_output(
    command: &CoordinationCliCommand,
    output: &Value,
    out: &mut impl Write,
) -> Result<()> {
    match command {
        CoordinationCliCommand::Help => writeln!(out, "{COORDINATION_CLI_USAGE}")?,
        CoordinationCliCommand::SessionsList { .. } => {
            let sessions = output
                .get("sessions")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if sessions.is_empty() {
                writeln!(out, "no root sessions")?;
            }
            for session in sessions {
                writeln!(
                    out,
                    "{}\t{}\t{}\t{}\t{}",
                    coordination_cli_field(session, "sessionId"),
                    coordination_cli_field(session, "agent"),
                    coordination_cli_field(session, "status"),
                    coordination_cli_field(session, "name"),
                    coordination_cli_field(session, "workdir"),
                )?;
            }
        }
        CoordinationCliCommand::MailboxList { as_session } => {
            let mailboxes = output
                .get("mailboxes")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if mailboxes.is_empty() {
                writeln!(out, "no mailboxes for {as_session}")?;
            }
            for mailbox in mailboxes {
                let participants = mailbox
                    .get("participants")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let own_cursor = participants
                    .iter()
                    .find(|participant| {
                        participant.get("sessionId").and_then(Value::as_str) == Some(as_session)
                    })
                    .map(|participant| coordination_cli_field(participant, "processedThrough"))
                    .unwrap_or_else(|| "-".to_owned());
                writeln!(
                    out,
                    "{}\tlatest #{}\tunread {}\tprocessedThrough {}",
                    coordination_cli_field(mailbox, "id"),
                    coordination_cli_field(mailbox, "latestSequence"),
                    coordination_cli_field(mailbox, "unreadCount"),
                    own_cursor,
                )?;
                for participant in participants {
                    writeln!(
                        out,
                        "  {} ({}) processedThrough {}",
                        coordination_cli_field(participant, "displayName"),
                        coordination_cli_field(participant, "sessionId"),
                        coordination_cli_field(participant, "processedThrough"),
                    )?;
                }
                if mailbox
                    .get("latestMessagePreview")
                    .and_then(Value::as_str)
                    .is_some()
                {
                    writeln!(
                        out,
                        "  latest: {}",
                        coordination_cli_field(mailbox, "latestMessagePreview")
                    )?;
                }
            }
        }
        CoordinationCliCommand::MailboxSend { .. } => writeln!(
            out,
            "delivered to {} via {}: message {} #{} (duplicate {}, notification {}, unreadDepth {})",
            coordination_cli_field(output, "sessionId"),
            coordination_cli_field(output, "mailboxId"),
            coordination_cli_field(output, "messageId"),
            coordination_cli_field(output, "sequence"),
            coordination_cli_field(output, "duplicate"),
            coordination_cli_field(output, "notificationDisposition"),
            coordination_cli_field(output, "unreadDepth"),
        )?,
        CoordinationCliCommand::MailboxRead {
            mailbox_id,
            after_sequence,
            ..
        } => {
            let messages = output
                .get("messages")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if messages.is_empty() {
                writeln!(
                    out,
                    "no messages in {mailbox_id} after #{}",
                    after_sequence.unwrap_or(0)
                )?;
            }
            for message in messages {
                render_coordination_cli_message(message, out)?;
            }
        }
        CoordinationCliCommand::MailboxReadMessage { .. } => {
            render_coordination_cli_message(output, out)?;
        }
        CoordinationCliCommand::MailboxAcknowledge {
            processed_through, ..
        } => writeln!(
            out,
            "acknowledged {} through #{} (latest #{}, unread {})",
            coordination_cli_field(output, "id"),
            processed_through,
            coordination_cli_field(output, "latestSequence"),
            coordination_cli_field(output, "unreadCount"),
        )?,
    }
    Ok(())
}

fn write_coordination_cli_output(
    invocation: &CoordinationCliInvocation,
    output: &Value,
) -> Result<()> {
    let mut stdout = io::stdout().lock();
    if invocation.json && !matches!(invocation.command, CoordinationCliCommand::Help) {
        serde_json::to_writer_pretty(&mut stdout, output)?;
        writeln!(stdout)?;
    } else {
        render_coordination_cli_output(&invocation.command, output, &mut stdout)?;
    }
    stdout.flush()?;
    Ok(())
}

/// Entry point for `termal sessions ...` / `termal mailbox ...`: executes the
/// command against the loopback API, validates the response shape, and
/// prints the result to stdout. Errors propagate to `main`, which prints them
/// on stderr and picks the exit code through `coordination_cli_exit_code`.
fn run_coordination_cli(invocation: CoordinationCliInvocation) -> Result<()> {
    let base_url = normalize_termal_http_base_url(
        invocation
            .base_url
            .clone()
            .unwrap_or_else(default_termal_http_base_url),
    );
    let output = execute_coordination_cli(&invocation.command, &base_url)
        .map_err(sanitize_coordination_cli_failure)?;
    validate_coordination_cli_output(&invocation.command, &output)
        .map_err(sanitize_coordination_cli_failure)?;
    match write_coordination_cli_output(&invocation, &output) {
        Err(err) if coordination_cli_error_is_broken_pipe(&err) => Ok(()),
        other => other.map_err(sanitize_coordination_cli_failure),
    }
}

#[cfg(test)]
#[path = "coordination_cli_tests.rs"]
mod coordination_cli_tests;
