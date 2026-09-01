// Claude Code CLI turn processing.
//
// Covers the Claude Code stdio protocol parser used by the long-lived
// `spawn_claude_runtime` process, the per-turn state machine
// (`ClaudeTurnState`, `ClaudeToolUse`, `ClaudeToolPermissionRequest`), event
// dispatch from the NDJSON protocol, tool-use bookkeeping, tool-result routing
// (bash vs file + task), approval handling, streamed assistant text
// reconciliation (delta + completed), thinking-line splitting, and the
// description/summary helpers used to render tool requests in the transcript.
//
// Extracted from turns.rs into its own `include!()` fragment so turns.rs
// stays focused on the TurnRecorder abstraction + shared helpers used
// across agents (error summarization, preview text, command language
// inference, prompt-image parsing, etc.).

/// Tracks Claude turn state.
#[derive(Default)]
struct ClaudeTurnState {
    approval_keys_this_turn: HashSet<String>,
    /// Bounds automatic AskUserQuestion self-resolution in a headless turn.
    /// Fresh request ids bypass ordinary dedupe, so without this counter a
    /// re-asking model could loop forever while appending audit cards.
    unattended_questions_self_resolved_this_turn: usize,
    parallel_agent_group_key: Option<String>,
    parallel_agent_order: Vec<String>,
    parallel_agents: HashMap<String, ParallelAgentProgress>,
    permission_denied_this_turn: bool,
    pending_tools: HashMap<String, ClaudeToolUse>,
    /// Whether this attempt emitted transcript content, reached a tool /
    /// approval boundary, or produced a protocol event not explicitly proven
    /// effect-free. Transient API retries are allowed only while this is false,
    /// so replay cannot duplicate a partially executed turn or hidden hook.
    replay_became_unsafe: bool,
    streamed_assistant_text: String,
    saw_text_delta: bool,
}

/// Represents Claude tool use.
struct ClaudeToolUse {
    command: Option<String>,
    description: Option<String>,
    file_path: Option<String>,
    name: String,
    subagent_type: Option<String>,
}

/// Represents the Claude tool permission request payload.
struct ClaudeToolPermissionRequest {
    detail: String,
    permission_mode_for_session: Option<String>,
    request_id: String,
    title: String,
    tool_name: String,
    tool_input: Value,
}

/// Represents Claude Code's blocking AskUserQuestion dialog payload.
struct ClaudeUserInputDialogRequest {
    detail: String,
    questions: Vec<UserInputQuestion>,
    request: ClaudePendingUserInput,
    title: String,
}

/// Denial for `AskUserQuestion` on sessions that are genuinely
/// noninteractive (see [`claude_question_is_unattended`]): nobody is
/// watching, so a parked question card would stall the run forever. The
/// wording steers Claude to resolve the question itself.
///
/// The self-resolution policy is specific to questions — a question only
/// needs a choice among options the model itself proposed — and does not
/// extend to side-effect approvals, which keep their own mode-dependent
/// handling.
const CLAUDE_UNATTENDED_QUESTION_DENIAL: &str = "TermAl denied AskUserQuestion because this \
     session runs unattended (a read-only reviewer or a non-interactive delegation child) and \
     nobody is watching to answer. Choose the best option using your own judgment and continue.";

/// Told to Claude when the user skips an AskUserQuestion card. The matching
/// tool-result error is an expected denial outcome, not a second transcript
/// error after the resolved question card.
const CLAUDE_USER_DECLINED_QUESTION_MESSAGE: &str =
    "The user declined to answer these questions. Decide using your best judgment and continue.";

/// Title of the resolved transcript card recorded when TermAl self-resolves
/// an `AskUserQuestion` on an unattended session.
const CLAUDE_SELF_RESOLVED_QUESTION_TITLE: &str = "Claude asked a question";

/// Detail of that card: the audit trail must say plainly that no person
/// answered and why.
const CLAUDE_SELF_RESOLVED_QUESTION_DETAIL: &str = "TermAl asked Claude to decide without human \
     input: this session runs unattended (a read-only reviewer or a non-interactive delegation \
     child), so no question card was shown.";

/// Compatibility-only audit detail for the legacy dialog channel. Unlike a
/// permission request it has no deny decision, so TermAl can only return a
/// protocol control error carrying the self-decide instruction. Current
/// Claude CLIs no longer emit this channel, so that fallback is not backed by
/// the live permission-transport capture.
const CLAUDE_SELF_RESOLVED_LEGACY_QUESTION_DETAIL: &str = "TermAl asked Claude to decide without \
     human input: this session runs unattended, so no question card was shown. The legacy \
     question-dialog protocol has no deny envelope; TermAl returned a control error carrying \
     the same self-decide instruction.";

/// A headless turn may ask a small number of self-resolved questions, but the
/// fourth attempt fails the turn instead of looping forever.
const MAX_CLAUDE_UNATTENDED_QUESTIONS_PER_TURN: usize = 3;

/// Reserves one unattended self-resolution slot for this turn. This is
/// called after transport-specific request-id dedupe, so replaying the same
/// control request does not consume another slot.
fn reserve_claude_unattended_question_self_resolution(
    state: &mut ClaudeTurnState,
) -> Result<()> {
    if state.unattended_questions_self_resolved_this_turn
        >= MAX_CLAUDE_UNATTENDED_QUESTIONS_PER_TURN
    {
        bail!(
            "Claude asked AskUserQuestion more than {MAX_CLAUDE_UNATTENDED_QUESTIONS_PER_TURN} times in one unattended turn; TermAl stopped the turn to prevent an unattended question loop. Restart with a prompt that supplies the missing decision, or run the work in an attended Ask-mode session"
        );
    }
    state.unattended_questions_self_resolved_this_turn += 1;
    Ok(())
}

/// Attendedness policy for `AskUserQuestion`, applied identically to the
/// `can_use_tool` permission transport and the legacy dialog channel.
///
/// Unattended means: the internal `ReadOnlyAutoApprove` mode (currently
/// assigned only to read-only reviewer delegations), or a delegation child
/// running under AutoApprove or Plan — those modes otherwise make progress
/// without an operator answering ordinary approval prompts, so a question
/// card would unexpectedly park the child under the fan-in. Everything else
/// is attended: a root AutoApprove or Plan session (a person is watching it),
/// an Ask-mode implementer child (it surfaces its approvals and questions to
/// a human on purpose), and ordinary or orchestrator sessions absent an
/// explicit headless signal. Being a delegation child alone is never the
/// signal, and writable children are never forced into Ask — that would
/// override the user's policy for ordinary tools.
fn claude_question_is_unattended(
    approval_mode: ClaudeApprovalMode,
    delegation_child: bool,
) -> bool {
    match approval_mode {
        ClaudeApprovalMode::ReadOnlyAutoApprove => true,
        ClaudeApprovalMode::AutoApprove | ClaudeApprovalMode::Plan => delegation_child,
        ClaudeApprovalMode::Ask => false,
    }
}

/// Classifies Claude control request. `delegation_child` is the session's
/// delegation-child identity read together with `approval_mode` under one
/// state lock (see `claude_control_request_context`).
fn classify_claude_control_request(
    message: &Value,
    state: &mut ClaudeTurnState,
    approval_mode: ClaudeApprovalMode,
    delegation_child: bool,
    cwd: &str,
    delegation_control_plane_access: bool,
) -> Result<Option<ClaudeControlRequestAction>> {
    let parsed_dialog = match parse_claude_user_input_dialog_request(message) {
        Ok(request) => request,
        Err(err) => {
            let Some(request_id) = claude_user_input_dialog_request_id(message).map(str::to_owned)
            else {
                return Err(err);
            };
            if claude_question_is_unattended(approval_mode, delegation_child) {
                let key = format!("user-dialog-legacy\n{request_id}");
                if !state.approval_keys_this_turn.insert(key) {
                    return Ok(None);
                }
                reserve_claude_unattended_question_self_resolution(state)?;
                return Ok(Some(
                    ClaudeControlRequestAction::RecordSelfResolvedQuestionError {
                        detail: format!(
                            "Claude AskUserQuestion legacy dialog was malformed and TermAl rejected it in this unattended session: {err:#}"
                        ),
                        response: ClaudeSelfResolvedQuestionResponse::DialogError(
                            ClaudeControlErrorResponse {
                                error: CLAUDE_UNATTENDED_QUESTION_DENIAL.to_owned(),
                                request_id,
                            },
                        ),
                    },
                ));
            }
            return Ok(Some(ClaudeControlRequestAction::RespondError(
                ClaudeControlErrorResponse {
                    error: format!("invalid Claude AskUserQuestion dialog: {err:#}"),
                    request_id,
                },
            )));
        }
    };
    if let Some(request) = parsed_dialog {
        // Dedupe keys are namespaced per transport so a legacy dialog and a
        // permission request that happen to share a request id within one
        // turn never suppress each other.
        let key = format!("user-dialog-legacy\n{}", request.request.request_id);
        if !state.approval_keys_this_turn.insert(key) {
            return Ok(None);
        }
        // The no-park invariant covers both question transports: an
        // unattended session must not queue a question card on the legacy
        // dialog channel either. The dialog protocol has no deny decision,
        // so the control error envelope carries the self-decide wording
        // back to the agent.
        if claude_question_is_unattended(approval_mode, delegation_child) {
            reserve_claude_unattended_question_self_resolution(state)?;
            return Ok(Some(
                ClaudeControlRequestAction::RecordSelfResolvedQuestion {
                    title: CLAUDE_SELF_RESOLVED_QUESTION_TITLE.to_owned(),
                    detail: CLAUDE_SELF_RESOLVED_LEGACY_QUESTION_DETAIL.to_owned(),
                    questions: request.questions,
                    response: ClaudeSelfResolvedQuestionResponse::DialogError(
                        ClaudeControlErrorResponse {
                            error: CLAUDE_UNATTENDED_QUESTION_DENIAL.to_owned(),
                            request_id: request.request.request_id,
                        },
                    ),
                },
            ));
        }
        return Ok(Some(ClaudeControlRequestAction::QueueUserInput {
            title: request.title,
            detail: request.detail,
            questions: request.questions,
            request: request.request,
        }));
    }

    let Some(mut request) = parse_claude_tool_permission_request(message) else {
        return Ok(None);
    };

    // Claude Code no longer opens the `request_user_dialog` channel for
    // AskUserQuestion in stream-json sessions: the questions arrive inside
    // this very `can_use_tool` payload and the user's answers must travel
    // back in the permission decision's `updatedInput.answers` (verified
    // against real CLI 2.1.250/2.1.251 captures). Route the questions to the
    // user-input card in every attended approval mode — including Plan,
    // because answering the user's own question is an interaction, not a
    // tool mutation. Unattended sessions (`claude_question_is_unattended`:
    // read-only reviewers and auto-approve delegation children) get an
    // immediate fail-closed denial instead: a parked question card would
    // stall them forever, and the denial message tells Claude to decide
    // without asking. A payload without a well-formed question list falls
    // through to the ordinary permission flow instead of failing the turn,
    // with the rejection reason surfaced on the approval card.
    //
    // The exact-name match below is the only identity the CLI exposes for
    // this tool, and it cannot collide with MCP tools: the CLI always
    // namespaces those as `mcp__<server>__<tool>` in `can_use_tool` (the
    // convention `src/delegation_mcp.rs` matches its own tool by), so an MCP
    // tool that names itself `AskUserQuestion` arrives with the prefix and
    // takes the ordinary permission flow.
    if request.tool_name == "AskUserQuestion" {
        if claude_question_is_unattended(approval_mode, delegation_child) {
            let key = format!("user-dialog-permission\n{}", request.request_id);
            if !state.approval_keys_this_turn.insert(key) {
                return Ok(None);
            }
            reserve_claude_unattended_question_self_resolution(state)?;
            let decision = ClaudePermissionDecision::Deny {
                request_id: request.request_id,
                message: CLAUDE_UNATTENDED_QUESTION_DENIAL.to_owned(),
            };
            // The parsed questions ride along for the audit card; a payload
            // that does not parse still gets the identical deny, just with
            // no card to record.
            return Ok(Some(
                match parse_claude_ask_user_question_payload(&request.tool_input) {
                    Ok(questions) => ClaudeControlRequestAction::RecordSelfResolvedQuestion {
                        title: CLAUDE_SELF_RESOLVED_QUESTION_TITLE.to_owned(),
                        detail: CLAUDE_SELF_RESOLVED_QUESTION_DETAIL.to_owned(),
                        questions,
                        response: ClaudeSelfResolvedQuestionResponse::PermissionDeny(decision),
                    },
                    Err(err) => {
                        eprintln!(
                            "claude> unattended AskUserQuestion permission payload did not parse; recording the diagnostic and denying: {err:#}"
                        );
                        ClaudeControlRequestAction::RecordSelfResolvedQuestionError {
                            detail: format!(
                                "Claude AskUserQuestion payload was malformed and TermAl denied it in this unattended session: {err:#}"
                            ),
                            response: ClaudeSelfResolvedQuestionResponse::PermissionDeny(decision),
                        }
                    }
                },
            ));
        }
        let parsed = parse_claude_ask_user_question_payload(&request.tool_input);
        match parsed {
            Ok(questions) => {
                let key = format!("user-dialog-permission\n{}", request.request_id);
                if !state.approval_keys_this_turn.insert(key) {
                    return Ok(None);
                }
                return Ok(Some(ClaudeControlRequestAction::QueueUserInput {
                    title: "Claude needs your input".to_owned(),
                    detail: claude_user_input_detail(questions.len()),
                    questions: questions.clone(),
                    request: ClaudePendingUserInput {
                        input: request.tool_input,
                        questions,
                        request_id: request.request_id,
                        transport: ClaudeUserInputTransport::Permission,
                    },
                }));
            }
            Err(err) => {
                // Mode-neutral wording: depending on the session's approval
                // mode the ordinary flow shows an approval card, auto-allows
                // (which Claude reports as unanswered questions), or denies.
                eprintln!(
                    "claude> AskUserQuestion permission payload did not parse; falling back to the ordinary permission flow: {err:#}"
                );
                request.detail = format!(
                    "{}\nAskUserQuestion payload rejected ({err:#}); TermAl routed it through the session's normal permission handling instead of a question card.",
                    request.detail
                );
            }
        }
    }

    let command = describe_claude_tool_request(&request);
    let key = format!("{}\n{}\n{}", request.request_id, request.title, command);
    if !state.approval_keys_this_turn.insert(key) {
        return Ok(None);
    }

    if delegation_control_plane_access
        && delegation_control_plane_capability_for_claude_tool_name(&request.tool_name).is_some()
    {
        return Ok(Some(ClaudeControlRequestAction::Respond(
            ClaudePermissionDecision::Allow {
                request_id: request.request_id,
                updated_input: request.tool_input,
            },
        )));
    }

    Ok(Some(match approval_mode {
        ClaudeApprovalMode::Ask => ClaudeControlRequestAction::QueueApproval {
            title: request.title,
            command,
            detail: request.detail,
            approval: ClaudePendingApproval {
                permission_mode_for_session: request.permission_mode_for_session,
                request_id: request.request_id,
                tool_input: request.tool_input,
            },
        },
        ClaudeApprovalMode::AutoApprove => {
            ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow {
                request_id: request.request_id,
                updated_input: request.tool_input,
            })
        }
        ClaudeApprovalMode::ReadOnlyAutoApprove => {
            ClaudeControlRequestAction::Respond(read_only_claude_permission_decision(request, cwd))
        }
        ClaudeApprovalMode::Plan => {
            ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny {
                request_id: request.request_id,
                message: "TermAl denied this tool request because Claude is in plan mode."
                    .to_owned(),
            })
        }
    }))
}

fn claude_user_input_dialog_request_id(message: &Value) -> Option<&str> {
    let request = message.get("request")?;
    (message.get("type").and_then(Value::as_str) == Some("control_request")
        && request.get("subtype").and_then(Value::as_str) == Some("request_user_dialog")
        && request.get("dialog_kind").and_then(Value::as_str)
            == Some("permission_ask_user_question"))
    .then(|| {
        message
            .get("request_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|request_id| !request_id.is_empty())
    })
    .flatten()
}

/// Parses the Claude Code 2.1 user-dialog channel used by AskUserQuestion.
///
/// The host opts into this exact dialog kind during `initialize`. Claude's
/// opaque payload contains the original tool input plus one to four questions;
/// TermAl gives each question a stable transport id while retaining the
/// original question text because Claude indexes `updatedInput.answers` by
/// that text rather than by an id.
fn parse_claude_user_input_dialog_request(
    message: &Value,
) -> Result<Option<ClaudeUserInputDialogRequest>> {
    if message.get("type").and_then(Value::as_str) != Some("control_request") {
        return Ok(None);
    }
    let Some(request) = message.get("request") else {
        return Ok(None);
    };
    if request.get("subtype").and_then(Value::as_str) != Some("request_user_dialog")
        || request.get("dialog_kind").and_then(Value::as_str)
            != Some("permission_ask_user_question")
    {
        return Ok(None);
    }

    let request_id = message
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|request_id| !request_id.trim().is_empty())
        .ok_or_else(|| anyhow!("Claude user dialog is missing request_id"))?
        .to_owned();
    let payload = request
        .get("payload")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("Claude AskUserQuestion dialog is missing its payload"))?;
    let input = payload
        .get("input")
        .filter(|input| input.is_object())
        .cloned()
        .ok_or_else(|| anyhow!("Claude AskUserQuestion dialog is missing its original input"))?;
    let raw_questions = payload
        .get("questions")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Claude AskUserQuestion dialog is missing questions"))?;
    let questions = parse_claude_ask_user_question_list(
        raw_questions,
        ClaudeUserInputTransport::Dialog,
    )?;

    let question_count = questions.len();
    Ok(Some(ClaudeUserInputDialogRequest {
        detail: claude_user_input_detail(question_count),
        request: ClaudePendingUserInput {
            input,
            questions: questions.clone(),
            request_id,
            transport: ClaudeUserInputTransport::Dialog,
        },
        questions,
        title: "Claude needs your input".to_owned(),
    }))
}

fn claude_user_input_detail(question_count: usize) -> String {
    if question_count == 1 {
        "Answer Claude's question to continue.".to_owned()
    } else {
        format!("Answer Claude's {question_count} questions to continue.")
    }
}

/// Parses the `questions` array out of an AskUserQuestion `can_use_tool`
/// tool input; a missing or non-array field is a diagnosed failure like any
/// other malformed list.
fn parse_claude_ask_user_question_payload(tool_input: &Value) -> Result<Vec<UserInputQuestion>> {
    tool_input
        .get("questions")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("AskUserQuestion payload has no `questions` array"))
        .and_then(|raw_questions| {
            parse_claude_ask_user_question_list(
                raw_questions,
                ClaudeUserInputTransport::Permission,
            )
        })
}

/// Parses an AskUserQuestion question list (1 to 4 unique questions with
/// optional labeled options) into TermAl's transport shape. Shared by the
/// legacy `request_user_dialog` channel and the `can_use_tool` permission
/// payload, which carry the same array.
fn parse_claude_ask_user_question_list(
    raw_questions: &[Value],
    transport: ClaudeUserInputTransport,
) -> Result<Vec<UserInputQuestion>> {
    if !(1..=4).contains(&raw_questions.len()) {
        return Err(anyhow!(
            "Claude AskUserQuestion payload contained {} questions; expected 1 to 4",
            raw_questions.len()
        ));
    }

    let mut question_texts = HashSet::new();
    let mut questions = Vec::with_capacity(raw_questions.len());
    for (index, raw_question) in raw_questions.iter().enumerate() {
        let raw_question = raw_question
            .as_object()
            .ok_or_else(|| anyhow!("Claude question {} is not an object", index + 1))?;
        let raw_question_text = raw_question
            .get("question")
            .and_then(Value::as_str)
            .filter(|question| !question.trim().is_empty())
            .ok_or_else(|| anyhow!("Claude question {} has no text", index + 1))?;
        // The permission transport is live-verified to index
        // `updatedInput.answers` by the exact question string, so preserve it
        // byte-for-byte. The compatibility-only dialog transport retains its
        // historical trimming rather than changing an unverified contract.
        let question = match transport {
            ClaudeUserInputTransport::Dialog => raw_question_text.trim(),
            ClaudeUserInputTransport::Permission => raw_question_text,
        };
        if !question_texts.insert(question.to_owned()) {
            return Err(anyhow!(
                "Claude AskUserQuestion payload contains duplicate question text"
            ));
        }
        let header = raw_question
            .get("header")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|header| !header.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Question {}", index + 1));
        let options = match raw_question.get("options") {
            None => None,
            Some(Value::Array(raw_options)) => {
                if raw_options.len() > 4 {
                    return Err(anyhow!(
                        "Claude question {} contained {} options; expected at most 4",
                        index + 1,
                        raw_options.len()
                    ));
                }
                let mut option_labels = HashSet::new();
                Some(
                    raw_options
                        .iter()
                        .enumerate()
                        .map(|(option_index, raw_option)| {
                        let raw_option = raw_option.as_object().ok_or_else(|| {
                            anyhow!(
                                "Claude question {} option {} is not an object",
                                index + 1,
                                option_index + 1
                            )
                        })?;
                        let raw_label = raw_option
                            .get("label")
                            .and_then(Value::as_str)
                            .filter(|label| !label.trim().is_empty())
                            .ok_or_else(|| {
                                anyhow!(
                                    "Claude question {} option {} has no label",
                                    index + 1,
                                    option_index + 1
                                )
                            })?;
                        // Match question-text normalization per transport: the
                        // permission channel returns exact option labels to
                        // Claude, while the dialog fallback keeps historical
                        // trimming.
                        let label = match transport {
                            ClaudeUserInputTransport::Dialog => raw_label.trim(),
                            ClaudeUserInputTransport::Permission => raw_label,
                        };
                            if !option_labels.insert(label.to_owned()) {
                                return Err(anyhow!(
                                    "Claude question {} contains duplicate option label `{label}`",
                                    index + 1
                                ));
                            }
                            Ok(UserInputQuestionOption {
                            description: raw_option
                                .get("description")
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .unwrap_or_default()
                                .to_owned(),
                            label: label.to_owned(),
                            })
                        })
                        .collect::<Result<Vec<_>>>()?,
                )
            }
            Some(_) => {
                return Err(anyhow!(
                    "Claude question {} `options` is not an array",
                    index + 1
                ));
            }
        };

        let multi_select = match raw_question.get("multiSelect") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                return Err(anyhow!(
                    "Claude question {} `multiSelect` is not a boolean",
                    index + 1
                ));
            }
        };

        questions.push(UserInputQuestion {
            header,
            id: format!("claude-question-{}", index + 1),
            is_other: true,
            is_secret: false,
            multi_select,
            options,
            question: question.to_owned(),
        });
    }

    Ok(questions)
}

fn read_only_claude_permission_decision(
    request: ClaudeToolPermissionRequest,
    cwd: &str,
) -> ClaudePermissionDecision {
    if claude_tool_permission_request_is_read_only(&request, cwd) {
        return ClaudePermissionDecision::Allow {
            request_id: request.request_id,
            updated_input: request.tool_input,
        };
    }

    ClaudePermissionDecision::Deny {
        request_id: request.request_id,
        message:
            "TermAl denied this tool request because this Claude reviewer delegation is read-only."
                .to_owned(),
    }
}

// Read-only Claude reviewer children need unattended review commands, but the
// parser is intentionally conservative: unsupported shell syntax denies by
// default, and only simple stderr-to-dev-null redirection is tolerated.
fn claude_tool_permission_request_is_read_only(
    request: &ClaudeToolPermissionRequest,
    cwd: &str,
) -> bool {
    match request.tool_name.as_str() {
        "Read" | "LS" | "Glob" | "Grep" => true,
        // The Windows PowerShell tool is DENIED for read-only reviewers. It carries
        // its command in the same `command` field, so an earlier revision routed it
        // through the Bash reader below. That reader implements BASH grammar, and
        // every attempt to bolt PowerShell onto it produced a security defect:
        //
        //   * `echo (Set-Content x y)` — PowerShell EVALUATES a parenthesized
        //     sub-expression. Survived only because the tokenizer happens to fail
        //     closed on `(`.
        //   * `git status 2>/dev/null` — the reader strips that literal before its
        //     `>` gate, then approves the ORIGINAL; PowerShell writes the file
        //     `<drive>\dev\null`.
        //   * `cd (Set-Content x y)` — the `cd ` head reached `continue` before the
        //     tokenizer ran: arbitrary-path WRITE.
        //   * `g\it status` — the tokenizer de-escapes `\` per bash, so it reads as
        //     `git`; PowerShell treats `\` as a PATH SEPARATOR and executes
        //     `.\g\it(.cmd/.ps1)` FROM THE REVIEWED TREE: arbitrary code execution
        //     from the reviewed tree: arbitrary code execution.
        //
        // Four defects, each a fresh denylist entry on a parser that models the
        // wrong language, and the escapes got worse each time. `&` (call operator),
        // `--%` (stop-parsing), and profile side effects are still unexamined. So
        // the rule is structural rather than another patch: a bash parser may only
        // gate bash.
        //
        // Cost is nil — reviewers already do their work through the Bash tool (Git
        // Bash on Windows), and this arm never cleared anything beyond `git …`
        // anyway. Restoring PowerShell needs its OWN fail-closed checker, not a
        // re-route through this one.
        "PowerShell" => false,
        "Bash" => request
            .tool_input
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| claude_bash_command_is_read_only(command, cwd)),
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => false,
        _ => false,
    }
}

fn claude_bash_command_is_read_only(command: &str, cwd: &str) -> bool {
    // Detect background separators on the ORIGINAL command, before the
    // `2>/dev/null` strip below. That strip is a naive text replace, so on input
    // like `echo x \2>/dev/null& touch y` it would delete `2>/dev/null` and leave
    // `\&`, making the real background `&` look escaped to the (escape-aware)
    // separator gate. Bash escapes the `2`, not the `&`.
    if claude_bash_command_has_background_separator(command) {
        return false;
    }

    // Reviewers commonly inspect the same reader-owned path for a short list of
    // literal names. Support that one bounded loop shape by statically expanding
    // it and sending every resulting body command back through this checker. This
    // is deliberately not a general Bash interpreter: dynamic loop values,
    // nested control flow, unknown variables, and trailing commands fail closed.
    if command.trim_start().starts_with("for ") {
        return claude_bash_literal_for_loop_is_read_only(command, cwd);
    }

    let normalized = command
        .replace("2> /dev/null", "")
        .replace("2>/dev/null", "");
    if normalized.contains('\n')
        || normalized.contains('\r')
        || normalized.contains(';')
        || normalized.contains('>')
        || normalized.contains('<')
        || normalized.contains('`')
        || normalized.contains("$(")
    {
        return false;
    }

    let pipe_normalized = normalized.replace("&&", "|").replace("||", "|");
    let segments: Vec<&str> = pipe_normalized.split('|').map(str::trim).collect();

    // `cd <dir> && git ...` can retarget git to a *different* repo whose on-disk
    // `.git/config` carries `core.fsmonitor`/`diff.external`/`core.pager` exec sinks
    // that fire during an otherwise read-only subcommand. Allow it ONLY when every
    // `cd` is a no-op into the delegation's own `cwd`: that runs git against the exact
    // same repo and config a plain `git ...` would — which is already allowed — so it
    // adds no attack surface. A `cd` to a subdirectory (which may hold a nested or
    // planted `.git`), a parent, `HOME` (bare `cd`), or any other path fails closed.
    //
    // `runs_git` MUST be decided from tokenized segments, not raw text: the tokenizer
    // (like bash) de-quotes and unescapes, so `'git'`, `"git"`, and `g\it` all execute
    // git. Raw-text matching here would report "no git" for `cd <other> && 'git' status`
    // and skip this guard, while the approval pass below still de-quotes and approves the
    // git command — retargeting git to another repo.
    let runs_git = segments
        .iter()
        .any(|&segment| claude_segment_invokes_git(segment));
    if runs_git {
        for &segment in &segments {
            // Decide "is this a `cd`" from TOKENS, symmetrically with `runs_git`
            // above for the same reason: raw-text matching misses
            // `'cd'` / `"cd"` / `c\d`, which the tokenizer (like bash) de-quotes
            // and executes. Raw matching here skipped the guard for a quoted cd
            // while the approval pass below de-quoted and accepted it.
            let is_cd = claude_bash_segment_tokens(segment)
                .is_some_and(|tokens| tokens.first().is_some_and(|token| token == "cd"));
            if is_cd && !claude_cd_segment_targets_cwd(segment, cwd) {
                return false;
            }
        }
    }

    for segment in segments {
        if segment.is_empty() {
            return false;
        }
        if segment == "true" || segment == ":" {
            continue;
        }

        // Tokenize BEFORE classifying the head. The token scanner is this
        // checker's fail-closed arm for expansion / subshell / glob syntax, so no
        // raw-text prefix may reach `continue` ahead of it. A `pwd`/`cd ` prefix
        // used to short-circuit here, so `cd (Set-Content victim.txt data)` was
        // auto-approved WITHOUT ever tokenizing — and PowerShell EVALUATES that
        // parenthesized write, giving a read-only reviewer an arbitrary-path write
        // primitive. `runs_git` already follows this rule; the `cd`/`pwd` heads
        // must do the same.
        let Some(tokens) = claude_bash_segment_tokens(segment) else {
            return false;
        };
        let tokens = tokens.iter().map(String::as_str).collect::<Vec<_>>();

        // `pwd` and `cd <target>` are inert on their own; a `cd` that could
        // retarget git was already vetted against `cwd` by the guard above. Bare
        // `cd` (HOME) and malformed `cd a b` fall through and fail closed.
        if matches!(tokens.as_slice(), ["pwd"] | ["cd", _]) {
            continue;
        }

        if !claude_bash_tokens_are_read_only(&tokens) {
            return false;
        }
    }

    true
}

fn claude_bash_literal_for_loop_is_read_only(command: &str, cwd: &str) -> bool {
    const MAX_LOOP_VALUES: usize = 64;
    const MAX_BODY_COMMANDS: usize = 16;

    let Some(clauses) = claude_bash_top_level_semicolon_clauses(command) else {
        return false;
    };
    if clauses.len() < 3 || clauses.last().map(String::as_str) != Some("done") {
        return false;
    }

    let Some(header_tokens) = claude_bash_segment_tokens(&clauses[0]) else {
        return false;
    };
    if header_tokens.len() < 4 || header_tokens[0] != "for" || header_tokens[2] != "in" {
        return false;
    }
    let variable = header_tokens[1].as_str();
    if !claude_bash_loop_identifier_is_safe(variable) {
        return false;
    }
    let values = &header_tokens[3..];
    if values.is_empty()
        || values.len() > MAX_LOOP_VALUES
        || values
            .iter()
            .any(|value| !claude_bash_loop_literal_is_safe(value))
    {
        return false;
    }

    let Some(first_body) = claude_bash_strip_keyword(&clauses[1], "do") else {
        return false;
    };
    if first_body.is_empty() {
        return false;
    }
    let mut body = vec![first_body];
    body.extend(
        clauses[2..clauses.len() - 1]
            .iter()
            .map(String::as_str),
    );
    if body.len() > MAX_BODY_COMMANDS
        || body.iter().any(|command| command.trim().is_empty())
    {
        return false;
    }

    values.iter().all(|value| {
        let expanded = body
            .iter()
            .map(|body_command| {
                claude_expand_bash_loop_variable(body_command, variable, value)
            })
            .collect::<Option<Vec<_>>>();
        expanded.is_some_and(|commands| {
            // Validate the body as one shell sequence, not isolated commands.
            // Directory changes persist between `;` clauses and iterations, so
            // separate checks would miss `cd other; git status` retargeting Git.
            claude_bash_command_is_read_only(&commands.join(" && "), cwd)
        })
    })
}

fn claude_bash_top_level_semicolon_clauses(command: &str) -> Option<Vec<String>> {
    let mut clauses = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut characters = command.chars();

    while let Some(character) = characters.next() {
        if let Some(active_quote) = quote {
            current.push(character);
            if active_quote == '"' && character == '\\' {
                current.push(characters.next()?);
            } else if character == active_quote {
                quote = None;
            }
            continue;
        }

        match character {
            '\\' => {
                current.push(character);
                current.push(characters.next()?);
            }
            '\'' | '"' => {
                quote = Some(character);
                current.push(character);
            }
            ';' => {
                let clause = current.trim();
                if clause.is_empty() {
                    return None;
                }
                clauses.push(clause.to_owned());
                current.clear();
            }
            '\n' | '\r' | '`' => return None,
            _ => current.push(character),
        }
    }

    if quote.is_some() {
        return None;
    }
    let final_clause = current.trim();
    if final_clause.is_empty() {
        return None;
    }
    clauses.push(final_clause.to_owned());
    Some(clauses)
}

fn claude_bash_strip_keyword<'a>(clause: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = clause.strip_prefix(keyword)?;
    if rest.is_empty() {
        return Some(rest);
    }
    rest.chars()
        .next()
        .is_some_and(char::is_whitespace)
        .then(|| rest.trim_start())
}

fn claude_bash_loop_identifier_is_safe(identifier: &str) -> bool {
    let mut characters = identifier.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn claude_bash_loop_literal_is_safe(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
}

fn claude_expand_bash_loop_variable(
    command: &str,
    variable: &str,
    value: &str,
) -> Option<String> {
    let mut expanded = String::with_capacity(command.len());
    let mut quote: Option<char> = None;
    let mut characters = command.chars().peekable();

    while let Some(character) = characters.next() {
        if quote == Some('\'') {
            expanded.push(character);
            if character == '\'' {
                quote = None;
            }
            continue;
        }

        match character {
            '\\' => {
                expanded.push(character);
                expanded.push(characters.next()?);
            }
            '\'' => {
                // A single quote inside an active double-quoted string is a
                // literal character in Bash; it must not replace that quote
                // state and swallow the later closing double quote.
                if quote.is_none() {
                    quote = Some(character);
                }
                expanded.push(character);
            }
            '"' => {
                quote = if quote == Some('"') {
                    None
                } else {
                    Some(character)
                };
                expanded.push(character);
            }
            '$' => {
                if characters.peek() == Some(&'{') {
                    characters.next();
                    let mut name = String::new();
                    let mut closed = false;
                    for next in characters.by_ref() {
                        if next == '}' {
                            closed = true;
                            break;
                        }
                        name.push(next);
                    }
                    if !closed || name != variable {
                        return None;
                    }
                    expanded.push_str(value);
                } else {
                    let mut name = String::new();
                    while characters.peek().is_some_and(|next| {
                        *next == '_' || next.is_ascii_alphanumeric()
                    }) {
                        name.push(characters.next().expect("peeked loop variable character"));
                    }
                    if name != variable {
                        return None;
                    }
                    expanded.push_str(value);
                }
            }
            '`' | '\n' | '\r' => return None,
            _ => expanded.push(character),
        }
    }

    quote.is_none().then_some(expanded)
}

/// Whether a `cd <target>` segment is a no-op into the delegation's own `cwd`.
///
/// True only for `cd .`, `cd ./`, or `cd <path>` whose normalized form equals `cwd`.
/// A subdirectory, a parent, a bare `cd` (which goes to `HOME`), or a `cd a b` all
/// return false so the caller fails closed. Only a same-folder `cd` runs git against
/// the exact repo and `.git/config` a plain `git ...` would, so it adds no exec-sink
/// surface over what is already allowed. `cwd` is expected pre-normalized by the
/// caller (see `spawn_claude_runtime`), and the target is normalized here the same way.
fn claude_cd_segment_targets_cwd(segment: &str, cwd: &str) -> bool {
    let Some(tokens) = claude_bash_segment_tokens(segment) else {
        return false;
    };
    // Exactly `cd <target>`; anything else (bare `cd`, extra args) fails closed.
    if tokens.len() != 2 || tokens[0] != "cd" {
        return false;
    }
    let target = tokens[1].as_str();
    if target == "." || target == "./" {
        return true;
    }
    if cwd.is_empty() {
        return false;
    }
    // Compare as directory keys, not raw strings: `normalize_user_facing_path` does
    // NOT unify `/` vs `\` or case, but the agent writes `cd` with forward slashes
    // while the runtime cwd is stored with backslashes on Windows. A raw `==` then
    // rejects a genuine same-folder `cd`. The key folds separators (and case on
    // Windows, which is case-insensitive), so `cd "C:/repo"` matches cwd `C:\repo`.
    claude_local_dir_match_key(&normalize_local_user_facing_path(target))
        == claude_local_dir_match_key(cwd)
}

/// Normalizes a user-facing directory path to a comparison key that is
/// separator-insensitive (`\` folded to `/`, one trailing separator dropped) and,
/// on Windows, case-insensitive. Used only to decide whether a `cd` target is the
/// delegation's own `cwd`; it never touches the filesystem.
fn claude_local_dir_match_key(path: &str) -> String {
    let unified: String = path
        .chars()
        .map(|character| if character == '\\' { '/' } else { character })
        .collect();
    let trimmed = unified.strip_suffix('/').unwrap_or(&unified);
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed.to_owned()
    }
}

/// Whether a segment, parsed the way bash will, invokes `git`. Decided from the
/// tokenizer — which de-quotes and unescapes exactly like the read-only approval pass —
/// so `'git'`, `"git"`, and `g\it` are all recognized. Raw-text matching would let
/// `cd <other-repo> && 'git' status` skip the cd-guard while the approval still runs git
/// against the other repo. A segment that fails to tokenize is not treated as
/// git here; the per-segment loop denies it regardless, so the command still fails closed.
fn claude_segment_invokes_git(segment: &str) -> bool {
    claude_bash_segment_tokens(segment)
        .is_some_and(|tokens| tokens.first().map(String::as_str) == Some("git"))
}

fn claude_bash_command_has_background_separator(command: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut characters = command.chars().peekable();

    while let Some(character) = characters.next() {
        if let Some(active_quote) = quote {
            // Double quotes still process `\`; single quotes do not. Either way a
            // `\"` / `\\` must not be mistaken for the closing quote.
            if active_quote == '"' && character == '\\' {
                characters.next();
                continue;
            }
            if character == active_quote {
                quote = None;
            }
            continue;
        }

        match character {
            // A backslash escapes the next character, so `\"` / `\&` are literals
            // and must not open a quote or count as a background separator. The
            // tokenizer already de-escapes; this gate must agree, or an escaped
            // quote hides a trailing `& <command>` from the read-only check.
            '\\' => {
                characters.next();
            }
            '\'' | '"' => quote = Some(character),
            '&' => {
                if characters.peek() == Some(&'&') {
                    characters.next();
                } else {
                    return true;
                }
            }
            _ => {}
        }
    }

    false
}

fn claude_bash_tokens_are_read_only(tokens: &[&str]) -> bool {
    let Some(command) = tokens.first().copied() else {
        return false;
    };

    // Pure readers: they consume stdin/files and write only to stdout. The hashers
    // are here so reviewers can fingerprint a diff (`git diff … | sha256sum`) to prove
    // content identity — a common, entirely read-only review technique.
    let read_only_commands = [
        "cat", "cksum", "echo", "grep", "head", "ls", "md5sum", "nl", "pwd", "sha1sum",
        "sha256sum", "sha512sum", "tail", "wc",
    ];
    if read_only_commands.contains(&command) {
        return true;
    }

    if command == "date" {
        return claude_date_tokens_are_read_only(tokens);
    }

    if command == "rg" {
        return claude_rg_tokens_are_read_only(tokens);
    }

    if command == "find" {
        return claude_find_tokens_are_read_only(tokens);
    }

    if command == "sed" {
        return claude_sed_tokens_are_read_only(tokens);
    }

    if command == "git" {
        return claude_git_tokens_are_read_only(tokens);
    }

    false
}

fn claude_bash_segment_tokens(segment: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;

    // The permission checker MUST tokenize the way bash will, including backslash
    // escaping. Otherwise a reviewer command like `git diff --out\put=/x`
    // tokenizes here as the harmless literal `--out\put=/x` (which no deny-list
    // matches) yet bash strips the backslash and runs the denied `--output=/x`.
    // Every read-only check downstream depends on this equivalence.
    let mut characters = segment.chars();
    while let Some(character) = characters.next() {
        if let Some(active_quote) = quote {
            // Inside double quotes bash still unescapes `"`, `\`, `$`, `` ` ``;
            // every other backslash stays literal. Single quotes escape nothing.
            if active_quote == '"' && character == '\\' {
                match characters.next() {
                    Some(next @ ('"' | '\\' | '$' | '`')) => current.push(next),
                    Some(next) => {
                        current.push('\\');
                        current.push(next);
                    }
                    None => return None,
                }
                continue;
            }
            // Parameter/command expansion stays active inside double quotes; we
            // cannot predict the expanded argv, so fail closed.
            if active_quote == '"' && matches!(character, '$' | '`') {
                return None;
            }
            if character == active_quote {
                quote = None;
            } else {
                current.push(character);
            }
            continue;
        }

        match character {
            // Outside quotes, `\<char>` is the literal char (bash drops the
            // backslash) and `\<newline>` is a line continuation.
            '\\' => match characters.next() {
                Some('\n') => {}
                Some(next) => current.push(next),
                None => return None,
            },
            '\'' | '"' => quote = Some(character),
            // Shell expansion / subshell / glob syntax the checker does not
            // emulate: `$'\x2d'` (ANSI-C), `$VAR`/`${..}`/`$(..)` expansion,
            // backtick command substitution, `{a,b}` brace expansion, `(` /
            // process substitution, and unquoted globs (`*`/`?`/`[`, which can
            // expand to a committed filename that looks like an option, e.g.
            // `git diff *` with a tracked `--output=x` file). All rewrite the
            // argv before execution, so classifying the literal text would be
            // unsound. Fail closed instead.
            '$' | '`' | '{' | '(' | '*' | '?' | '[' => return None,
            character if character.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(character),
        }
    }

    if quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Some(tokens)
}

fn claude_date_tokens_are_read_only(tokens: &[&str]) -> bool {
    tokens
        .iter()
        .skip(1)
        .all(|token| token.starts_with('+') || matches!(*token, "-u" | "--utc"))
}

fn claude_rg_tokens_are_read_only(tokens: &[&str]) -> bool {
    !tokens
        .iter()
        .skip(1)
        .any(|token| *token == "--pre" || token.starts_with("--pre="))
}

fn claude_find_tokens_are_read_only(tokens: &[&str]) -> bool {
    !tokens.iter().any(|token| {
        matches!(
            *token,
            "-delete" | "-exec" | "-execdir" | "-fls" | "-fprint" | "-fprint0" | "-fprintf"
                | "-ok" | "-okdir"
        )
    })
}

fn claude_sed_tokens_are_read_only(tokens: &[&str]) -> bool {
    let mut saw_script_option = false;
    let mut positional_script_checked = false;

    for (index, token) in tokens.iter().enumerate().skip(1) {
        if *token == "-i"
            || *token == "--in-place"
            || token.starts_with("-i")
            || token.starts_with("--in-place=")
            || *token == "-f"
            || *token == "--file"
            || token.starts_with("-f")
            || token.starts_with("--file=")
        {
            return false;
        }

        if let Some(script) = token.strip_prefix("-e") {
            saw_script_option = true;
            let script = if script.is_empty() {
                tokens.get(index + 1).copied().unwrap_or_default()
            } else {
                script
            };
            if claude_sed_script_can_write(script) {
                return false;
            }
            continue;
        }

        if token.starts_with('-') {
            continue;
        }

        if !saw_script_option && !positional_script_checked {
            positional_script_checked = true;
            if claude_sed_script_can_write(token) {
                return false;
            }
        }
    }

    true
}

fn claude_sed_script_can_write(script: &str) -> bool {
    let mut command_start = true;
    let mut escaped = false;
    let mut characters = script.chars().peekable();

    while let Some(character) = characters.next() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character.is_whitespace() && command_start {
            continue;
        }
        if character == ';' || character == '\n' {
            command_start = true;
            continue;
        }

        if !command_start {
            continue;
        }

        match character {
            'w' | 'W' | 'e' => return true,
            '0'..='9' | ',' | '$' => continue,
            '/' => {
                if !claude_skip_sed_address_regex(&mut characters, '/') {
                    return false;
                }
                continue;
            }
            's' | 'y' => {
                let Some(delimiter) = characters.next() else {
                    return false;
                };
                if delimiter == '\\' || delimiter == '\n' {
                    return false;
                }
                let separator_count = if character == 's' { 2 } else { 1 };
                if claude_skip_sed_delimited_sections(&mut characters, delimiter, separator_count)
                    && character == 's'
                    && claude_sed_substitution_flags_can_write(&mut characters)
                {
                    return true;
                }
                command_start = false;
            }
            _ => command_start = false,
        }
    }

    false
}

fn claude_skip_sed_address_regex<I>(
    characters: &mut std::iter::Peekable<I>,
    delimiter: char,
) -> bool
where
    I: Iterator<Item = char>,
{
    let mut escaped = false;
    for character in characters.by_ref() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == delimiter {
            return true;
        }
    }
    false
}

fn claude_skip_sed_delimited_sections<I>(
    characters: &mut std::iter::Peekable<I>,
    delimiter: char,
    separator_count: usize,
) -> bool
where
    I: Iterator<Item = char>,
{
    let mut escaped = false;
    let mut seen_separators = 0;
    for character in characters.by_ref() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == delimiter {
            seen_separators += 1;
            if seen_separators == separator_count {
                return true;
            }
        }
    }
    false
}

fn claude_sed_substitution_flags_can_write<I>(characters: &mut std::iter::Peekable<I>) -> bool
where
    I: Iterator<Item = char>,
{
    while let Some(character) = characters.peek().copied() {
        if character == ';' || character == '\n' {
            return false;
        }
        if character.is_whitespace() {
            characters.next();
            continue;
        }
        if matches!(character, 'w' | 'W' | 'e') {
            return true;
        }
        characters.next();
    }
    false
}

fn claude_git_tokens_are_read_only(tokens: &[&str]) -> bool {
    // Skip a conservative allow-list of leading git global options (e.g.
    // `git --no-pager log`) before reading the subcommand. Unknown leading
    // options fail closed, so this never widens the allowed subcommand set.
    //
    // Every option that can make git load configuration from an
    // attacker-influenced source is deliberately excluded, because several
    // config keys (`diff.external`, `core.fsmonitor`, `core.pager`) execute
    // external programs during otherwise read-only subcommands:
    //
    // * `-c <name>=<value>` injects such a key inline.
    // * `-C`/`--git-dir`/`--work-tree`/`--namespace` retarget the repository.
    //   Only the literal `.git` name is un-committable, so a reviewed change
    //   can track a bare-repo fixture and `git --git-dir=fixture.git
    //   --work-tree=. status` would run that config's `core.fsmonitor` --
    //   reviewed content executing code inside this read-only sandbox.
    // * `--paginate`/`-p` force the pager, making `core.pager` an exec sink.
    // * `--exec-path[=...]` repoints git at another binary.
    //
    // Re-enabling `-C` requires TermAl to neutralize those sinks in the
    // reviewer child's git environment; the checker only approves the agent's
    // verbatim command and cannot do it here.
    let mut index = 1;
    while let Some(option) = tokens.get(index).copied() {
        match option {
            // Flag-only global options that cannot retarget the repository,
            // force a pager, or relocate the git binary.
            "--no-pager" | "-P" | "--no-optional-locks" => index += 1,
            // First non-option token is the subcommand.
            _ if !option.starts_with('-') => break,
            // Any other leading option is unknown -> fail closed.
            _ => return false,
        }
    }

    // Rebuild `[git, subcommand, args...]` so the downstream helpers, which use
    // `.skip(2)`, keep their positional assumptions. A value-taking option with
    // no following subcommand yields just `["git"]`, so `get(1)` denies below.
    let normalized: Vec<&str> = std::iter::once("git")
        .chain(tokens.get(index..).unwrap_or_default().iter().copied())
        .collect();

    let Some(subcommand) = normalized.get(1).copied() else {
        return false;
    };

    // `--help` / `-h` dispatch through `git help`, which launches a configured
    // man/browser viewer (a shell-evaluated exec sink) even on read-only
    // subcommands like `git blame --help`. Deny it, and its abbreviations,
    // everywhere.
    if normalized
        .iter()
        .skip(2)
        .any(|token| *token == "-h" || claude_git_long_option_abbreviates(token, "--help"))
    {
        return false;
    }

    match subcommand {
        // `shortlog` (and defensively the other listings) accept `--output
        // <path>`, which writes/truncates an arbitrary file; route every listing
        // subcommand through the same output/exec-sink denial as diff/log/show.
        "diff" | "log" | "show" | "blame" | "describe" | "ls-files" | "rev-parse" | "shortlog"
        | "status" | "patch-id" => claude_git_output_tokens_are_read_only(&normalized),
        // `git hash-object` is deliberately NOT allowed: it applies gitattributes clean
        // filters (`filter.<name>.clean = <cmd>`) unless `--no-filters`, so hashing a
        // tracked file in the repo under review can execute a repo-defined command — an
        // exec sink the diff/log/show arm already blocks. Diff fingerprinting uses
        // `sha256sum` on `git diff` output instead, which reaches no such sink.
        "grep" => claude_git_grep_tokens_are_read_only(&normalized),
        // Only the listing form of `remote`; `add`/`remove`/`set-url`/`prune`
        // and friends are non-flag tokens and fail this check.
        "remote" => normalized
            .iter()
            .skip(2)
            .all(|token| matches!(*token, "-v" | "--verbose")),
        // A read-only `git branch` is only ever a listing. A deny-list cannot be
        // sound here: git parses clustered short options (`-quorigin/main` is
        // `-q -u origin/main`, which sets the upstream) and expands unambiguous
        // long-option abbreviations (`--uns` -> `--unset-upstream`, `--edi` ->
        // `--edit-description`). Allow an exact listing-only set instead, so any
        // cluster, abbreviation, branch name, or unknown option fails closed.
        "branch" => normalized.iter().skip(2).all(|token| {
            matches!(
                *token,
                "-a" | "--all"
                    | "-r" | "--remotes"
                    | "-v" | "-vv" | "--verbose"
                    | "-l" | "--list"
                    | "--show-current"
                    | "--no-color"
            ) || token.starts_with("--sort=")
                || token.starts_with("--format=")
                || token.starts_with("--contains=")
                || token.starts_with("--no-contains=")
                || token.starts_with("--merged=")
                || token.starts_with("--no-merged=")
                || token.starts_with("--points-at=")
        }),
        _ => false,
    }
}

/// Returns whether `token` is `option`, its `option=value` form, or any prefix
/// git would expand to `option`. Git accepts unambiguous long-option
/// abbreviations (`--ext` -> `--ext-diff`, `--textc` -> `--textconv`), so an
/// exact-match deny-list is unsound; every abbreviation must fail closed too.
fn claude_git_long_option_abbreviates(token: &str, option: &str) -> bool {
    if !token.starts_with("--") {
        return false;
    }
    let name = token.split('=').next().unwrap_or(token);
    name.len() > 2 && option.starts_with(name)
}

fn claude_git_output_tokens_are_read_only(tokens: &[&str]) -> bool {
    !tokens.iter().skip(2).any(|token| {
        // `--text` is an exact, read-only diff option (force text on binary).
        // git resolves exact option names before abbreviations, so it must not
        // be denied as a `--textconv` abbreviation.
        if *token == "--text" {
            return false;
        }
        ["--output", "--ext-diff", "--textconv"]
            .iter()
            .any(|option| claude_git_long_option_abbreviates(token, option))
    })
}

fn claude_git_grep_tokens_are_read_only(tokens: &[&str]) -> bool {
    !tokens.iter().skip(2).any(|token| {
        // `-O[<pager>]`/`--open-files-in-pager` opens matches in a pager (an exec
        // sink). git bundles short options, so `-nOcat` parses as
        // `-n --open-files-in-pager=cat`; scan the cluster and deny any `O` that
        // git reads as this option. An earlier value-taking option (`-e`/`-f`,
        // context `-A`/`-B`/`-C`, `-m`) consumes the rest as its value, so an
        // `O` after one of those is a value character, not the sink.
        if token.starts_with('-') && !token.starts_with("--") {
            for flag in token.bytes().skip(1) {
                match flag {
                    b'O' => return true,
                    b'e' | b'f' | b'A' | b'B' | b'C' | b'm' => break,
                    _ => {}
                }
            }
            false
        } else {
            // `--text` (git grep -a: treat binary as text) is an exact read-only
            // option; git resolves it before the `--textconv` abbreviation.
            // `--textconv` runs a config-driven filter program (an exec sink, as
            // for diff/log/show) — denied so grep matches the other read paths.
            *token != "--text"
                && (claude_git_long_option_abbreviates(token, "--open-files-in-pager")
                    || claude_git_long_option_abbreviates(token, "--textconv"))
        }
    })
}

/// Parses Claude tool permission request.
fn parse_claude_tool_permission_request(message: &Value) -> Option<ClaudeToolPermissionRequest> {
    if message.get("type").and_then(Value::as_str) != Some("control_request") {
        return None;
    }

    let request = message.get("request")?;
    if request.get("subtype").and_then(Value::as_str) != Some("can_use_tool") {
        return None;
    }

    let request_id = message
        .get("request_id")
        .and_then(Value::as_str)?
        .to_owned();
    let tool_name = request.get("tool_name").and_then(Value::as_str)?;
    let tool_input = request.get("input").cloned().unwrap_or_else(|| json!({}));
    let permission_mode_for_session = request
        .get("permission_suggestions")
        .and_then(Value::as_array)
        .and_then(|suggestions| {
            suggestions.iter().find_map(|suggestion| {
                (suggestion.get("type").and_then(Value::as_str) == Some("setMode")
                    && suggestion.get("destination").and_then(Value::as_str) == Some("session"))
                .then(|| suggestion.get("mode").and_then(Value::as_str))
                .flatten()
                .map(str::to_owned)
            })
        });

    let detail = describe_claude_permission_detail(
        tool_name,
        &tool_input,
        request.get("decision_reason").and_then(Value::as_str),
    );

    Some(ClaudeToolPermissionRequest {
        detail,
        permission_mode_for_session,
        request_id,
        title: "Claude needs approval".to_owned(),
        tool_name: tool_name.to_owned(),
        tool_input,
    })
}

/// Records Claude assistant text delta.
fn record_claude_assistant_text_delta(
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
    text: &str,
) -> Result<()> {
    let delta = if state.saw_text_delta {
        text
    } else {
        text.trim_start_matches('\n')
    };
    if delta.is_empty() {
        return Ok(());
    }

    recorder.text_delta(delta)?;
    state.replay_became_unsafe = true;
    state.saw_text_delta = true;
    state.streamed_assistant_text.push_str(delta);
    Ok(())
}

/// Records Claude completed assistant text.
fn record_claude_completed_assistant_text(
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
    text: &str,
) -> Result<()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    if !state.saw_text_delta {
        state.replay_became_unsafe = true;
        state.streamed_assistant_text.clear();
        state.streamed_assistant_text.push_str(trimmed);
        return recorder.push_text(trimmed);
    }

    match next_completed_codex_text_update(&mut state.streamed_assistant_text, trimmed) {
        CompletedTextUpdate::NoChange => Ok(()),
        CompletedTextUpdate::Append(unseen_suffix) => recorder.text_delta(&unseen_suffix),
        CompletedTextUpdate::Replace(replacement_text) => {
            recorder.replace_streaming_text(&replacement_text)
        }
    }
}

/// Finishes Claude assistant text stream.
fn finish_claude_assistant_text_stream<R: TurnRecorder + ?Sized>(
    state: &mut ClaudeTurnState,
    recorder: &mut R,
) -> Result<()> {
    recorder.finish_streaming_text()?;
    state.streamed_assistant_text.clear();
    state.saw_text_delta = false;
    Ok(())
}

/// Clears Claude turn-local state.
fn clear_claude_turn_state(state: &mut ClaudeTurnState) {
    state.approval_keys_this_turn.clear();
    state.unattended_questions_self_resolved_this_turn = 0;
    state.parallel_agent_group_key = None;
    state.parallel_agent_order.clear();
    state.parallel_agents.clear();
    state.permission_denied_this_turn = false;
    state.pending_tools.clear();
    state.replay_became_unsafe = false;
    state.streamed_assistant_text.clear();
    state.saw_text_delta = false;
}

/// Resets Claude turn-local parser and recorder state.
fn reset_claude_turn_state<R: TurnRecorder + ?Sized>(
    state: &mut ClaudeTurnState,
    recorder: &mut R,
) -> Result<()> {
    finish_claude_assistant_text_stream(state, recorder)?;
    clear_claude_turn_state(state);
    recorder.reset_turn_state()
}

/// Returns whether a system envelope is proven not to represent prompt work.
fn claude_system_event_is_effect_free(message: &Value) -> bool {
    match message.get("subtype").and_then(Value::as_str) {
        Some("init") => true,
        // Live Claude Code 2.1.220 stream capture emits this status immediately
        // before the replayed user prompt. It describes request admission and
        // does not itself run the prompt.
        Some("status") => message.get("status").and_then(Value::as_str) == Some("requesting"),
        // SessionStart hooks run once for the persistent child process. Replaying
        // a prompt inside that same process cannot run them again. Prompt hooks
        // such as UserPromptSubmit are intentionally not exempt.
        Some("hook_started" | "hook_progress" | "hook_response") => {
            message.get("hook_event").and_then(Value::as_str) == Some("SessionStart")
        }
        _ => false,
    }
}

/// Returns whether a Claude `user` envelope is the CLI's echo of the submitted
/// prompt rather than a tool-result boundary.
fn claude_user_event_is_prompt_echo(message: &Value) -> bool {
    let Some(content) = message.pointer("/message/content").and_then(Value::as_array) else {
        return false;
    };
    !content.is_empty()
        && content.iter().all(|block| {
            matches!(
                block.get("type").and_then(Value::as_str),
                Some("text" | "image")
            )
        })
}

/// Handles Claude event.
fn handle_claude_event(
    message: &Value,
    session_id: &mut Option<String>,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(event_type) = message.get("type").and_then(Value::as_str) else {
        state.replay_became_unsafe = true;
        return Ok(());
    };

    match event_type {
        "system" => {
            if message.get("subtype").and_then(Value::as_str) == Some("init") {
                if let Some(found_session_id) = message.get("session_id").and_then(Value::as_str) {
                    *session_id = Some(found_session_id.to_owned());
                    recorder.note_external_session(found_session_id)?;
                }
            } else if !claude_system_event_is_effect_free(message) {
                // Prompt hooks and future system events may represent effects
                // TermAl does not understand. Fail closed unless the exact
                // envelope is proven process-local or request-admission-only.
                state.replay_became_unsafe = true;
            }
        }
        "stream_event" => {
            let Some(stream_type) = message.pointer("/event/type").and_then(Value::as_str) else {
                state.replay_became_unsafe = true;
                return Ok(());
            };

            match stream_type {
                "content_block_delta" => {
                    if !state.permission_denied_this_turn {
                        if let Some(text) = message
                            .pointer("/event/delta/text")
                            .or_else(|| message.pointer("/event/delta/text_delta"))
                            .and_then(Value::as_str)
                        {
                            record_claude_assistant_text_delta(state, recorder, text)?;
                        } else {
                            // Thinking, tool-input, and future delta shapes are
                            // not proven safe to replay.
                            state.replay_became_unsafe = true;
                        }
                    }
                }
                "message_stop" => {
                    // Claude can emit the final assistant payload after `message_stop`.
                    // Keep the current text bubble open so any unseen suffix lands in it.
                }
                _ => {
                    state.replay_became_unsafe = true;
                }
            }
        }
        "assistant" => {
            if let Some(contents) = message
                .pointer("/message/content")
                .and_then(Value::as_array)
            {
                for content in contents {
                    let Some(content_type) = content.get("type").and_then(Value::as_str) else {
                        state.replay_became_unsafe = true;
                        continue;
                    };

                    match content_type {
                        "text" => {
                            if let Some(text) = content.get("text").and_then(Value::as_str) {
                                if state.permission_denied_this_turn {
                                    continue;
                                }
                                record_claude_completed_assistant_text(state, recorder, text)?;
                            }
                        }
                        "thinking" => {
                            if let Some(thinking) = content.get("thinking").and_then(Value::as_str)
                            {
                                state.replay_became_unsafe = true;
                                finish_claude_assistant_text_stream(state, recorder)?;
                                let lines = split_thinking_lines(thinking);
                                recorder.push_thinking("Thinking", lines)?;
                            }
                        }
                        "tool_use" => {
                            state.replay_became_unsafe = true;
                            finish_claude_assistant_text_stream(state, recorder)?;
                            register_claude_tool_use(content, state, recorder)?;
                        }
                        _ => {
                            state.replay_became_unsafe = true;
                        }
                    }
                }
            } else {
                state.replay_became_unsafe = true;
            }
        }
        "user" => {
            // --replay-user-messages echoes the submitted text/image prompt on
            // stdout before the assistant response. That echo is effect-free;
            // tool results and unknown user content remain replay barriers.
            if !claude_user_event_is_prompt_echo(message) {
                state.replay_became_unsafe = true;
            }
            handle_claude_tool_result(message, state, recorder)?;
        }
        // Claude Code 2.1.220 emits this telemetry envelope immediately
        // before the terminal result. It reports quota state only and does not
        // represent assistant output, a tool boundary, or a hook effect.
        "rate_limit_event" => {}
        "result" => {
            reset_claude_turn_state(state, recorder)?;

            if message
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                recorder.error(&summarize_error(message))?;
            }
        }
        _ => {
            state.replay_became_unsafe = true;
        }
    }

    Ok(())
}

/// Registers Claude tool use.
fn register_claude_tool_use(
    content: &Value,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(tool_id) = content.get("id").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(name) = content.get("name").and_then(Value::as_str) else {
        return Ok(());
    };

    let input = content.get("input");
    let command = input
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let description = input
        .and_then(|value| value.get("description"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let file_path = input
        .and_then(|value| value.get("file_path").or_else(|| value.get("filePath")))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let subagent_type = input
        .and_then(|value| {
            value
                .get("subagent_type")
                .or_else(|| value.get("subagentType"))
        })
        .and_then(Value::as_str)
        .map(str::to_owned);

    state.pending_tools.insert(
        tool_id.to_owned(),
        ClaudeToolUse {
            command: command.clone(),
            description: description.clone(),
            file_path,
            name: name.to_owned(),
            subagent_type: subagent_type.clone(),
        },
    );

    match name {
        "Bash" => {
            let command_label = command
                .as_deref()
                .or(description.as_deref())
                .unwrap_or("Bash");
            recorder.command_started(tool_id, command_label)?;
        }
        "Task" => {
            if state.parallel_agent_group_key.is_none() {
                state.parallel_agent_group_key = Some(format!("claude-task-group-{tool_id}"));
            }
            if !state.parallel_agents.contains_key(tool_id) {
                state.parallel_agent_order.push(tool_id.to_owned());
            }
            state.parallel_agents.insert(
                tool_id.to_owned(),
                ParallelAgentProgress {
                    detail: Some("Initializing...".to_owned()),
                    id: tool_id.to_owned(),
                    source: ParallelAgentSource::Tool,
                    status: ParallelAgentStatus::Initializing,
                    title: describe_claude_task_tool(
                        description.as_deref(),
                        subagent_type.as_deref(),
                    ),
                },
            );
            sync_claude_parallel_agents(state, recorder)?;
        }
        _ => {}
    }

    Ok(())
}
/// Handles Claude tool result.
fn handle_claude_tool_result(
    message: &Value,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(contents) = message
        .pointer("/message/content")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };

    for content in contents {
        if content.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }

        let Some(tool_use_id) = content.get("tool_use_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(tool_use) = state.pending_tools.remove(tool_use_id) else {
            continue;
        };

        let is_error = content
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let detail = extract_claude_tool_result_text(message, content);

        match tool_use.name.as_str() {
            "Bash" => handle_claude_bash_result(
                tool_use_id,
                &tool_use,
                message.get("tool_use_result"),
                &detail,
                is_error,
                state,
                recorder,
            )?,
            "Task" => handle_claude_task_result(
                tool_use_id,
                &tool_use,
                &detail,
                is_error,
                state,
                recorder,
            )?,
            "Write" | "Edit" => handle_claude_file_result(
                &tool_use,
                message.get("tool_use_result"),
                &detail,
                is_error,
                state,
                recorder,
            )?,
            "AskUserQuestion" => {
                // A user Skip or unattended self-resolution already records
                // a resolved question card. Claude reports the corresponding
                // deny as an error-shaped tool result; suppress only those
                // known denial outcomes so unexpected tool failures remain
                // visible in the transcript.
                if is_error && !is_expected_claude_ask_user_question_denial(&detail) {
                    recorder.error(&detail)?;
                }
            }
            _ => {
                if is_error {
                    recorder.error(&detail)?;
                }
            }
        }
    }

    Ok(())
}
/// Handles Claude task result.
fn handle_claude_task_result(
    tool_use_id: &str,
    tool_use: &ClaudeToolUse,
    detail: &str,
    is_error: bool,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let title = describe_claude_task_tool(
        tool_use.description.as_deref(),
        tool_use.subagent_type.as_deref(),
    );
    let summarized_detail = summarize_claude_task_detail(detail, is_error);
    let status = if is_error {
        ParallelAgentStatus::Error
    } else {
        ParallelAgentStatus::Completed
    };

    if let Some(agent) = state.parallel_agents.get_mut(tool_use_id) {
        agent.detail = Some(summarized_detail.clone());
        if agent.source != ParallelAgentSource::Tool {
            eprintln!(
                "claude task warning> resetting non-tool parallel agent source for `{tool_use_id}`"
            );
            agent.source = ParallelAgentSource::Tool;
        }
        agent.status = status;
        if agent.title.trim().is_empty() {
            agent.title = title.clone();
        }
    } else {
        state.parallel_agent_order.push(tool_use_id.to_owned());
        state.parallel_agents.insert(
            tool_use_id.to_owned(),
            ParallelAgentProgress {
                detail: Some(summarized_detail.clone()),
                id: tool_use_id.to_owned(),
                source: ParallelAgentSource::Tool,
                status,
                title: title.clone(),
            },
        );
    }

    sync_claude_parallel_agents(state, recorder)?;

    let trimmed = detail.trim();
    let result_summary = if trimmed.is_empty() {
        if is_error {
            Some(summarized_detail.as_str())
        } else {
            None
        }
    } else {
        Some(trimmed)
    };
    if let Some(summary) = result_summary {
        recorder.push_subagent_result(&title, summary, None, None)?;
    }

    Ok(())
}

/// Syncs Claude parallel agents.
fn sync_claude_parallel_agents(
    state: &ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(key) = state.parallel_agent_group_key.as_deref() else {
        return Ok(());
    };

    let agents = state
        .parallel_agent_order
        .iter()
        .filter_map(|agent_id| state.parallel_agents.get(agent_id).cloned())
        .collect::<Vec<_>>();
    if agents.is_empty() {
        return Ok(());
    }

    recorder.upsert_parallel_agents(key, &agents)
}

/// Describes Claude task tool.
fn describe_claude_task_tool(description: Option<&str>, subagent_type: Option<&str>) -> String {
    let trimmed_description = description.unwrap_or("").trim();
    if !trimmed_description.is_empty() {
        return trimmed_description.to_owned();
    }

    let trimmed_subagent_type = subagent_type.unwrap_or("").trim();
    if !trimmed_subagent_type.is_empty() {
        return format!("{} agent", trimmed_subagent_type.replace('-', " "));
    }

    "Task agent".to_owned()
}

/// Summarizes Claude task detail.
fn summarize_claude_task_detail(detail: &str, is_error: bool) -> String {
    let trimmed = detail.trim();
    if trimmed.is_empty() {
        return if is_error {
            "Task failed.".to_owned()
        } else {
            "Completed.".to_owned()
        };
    }

    make_preview(trimmed)
}
/// Handles Claude bash result.
fn handle_claude_bash_result(
    tool_use_id: &str,
    tool_use: &ClaudeToolUse,
    tool_use_result: Option<&Value>,
    detail: &str,
    is_error: bool,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if is_error && is_permission_denial(detail) {
        state.permission_denied_this_turn = true;
        record_claude_approval(
            state,
            recorder,
            "Claude needs approval",
            tool_use.command.as_deref().unwrap_or("Bash"),
            detail,
        )?;
        return Ok(());
    }

    let stdout = tool_use_result
        .and_then(|value| value.get("stdout"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let stderr = tool_use_result
        .and_then(|value| value.get("stderr"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let interrupted = tool_use_result
        .and_then(|value| value.get("interrupted"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut output = String::new();
    if !stdout.is_empty() {
        output.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(stderr);
    }
    if output.trim().is_empty() && !detail.is_empty() {
        output.push_str(detail);
    }

    let status = if is_error || interrupted {
        CommandStatus::Error
    } else {
        CommandStatus::Success
    };
    let command = tool_use.command.as_deref().unwrap_or("Bash");
    recorder.command_completed(tool_use_id, command, output.trim_end(), status)
}

/// Handles Claude file result.
fn handle_claude_file_result(
    tool_use: &ClaudeToolUse,
    tool_use_result: Option<&Value>,
    detail: &str,
    is_error: bool,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if is_error {
        if is_permission_denial(detail) {
            state.permission_denied_this_turn = true;
            record_claude_approval(
                state,
                recorder,
                "Claude needs approval",
                &describe_claude_tool_action(tool_use),
                detail,
            )?;
        } else {
            recorder.error(detail)?;
        }
        return Ok(());
    }

    let Some(tool_use_result) = tool_use_result else {
        return Ok(());
    };

    let tool_kind = tool_use_result
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let Some(file_path) = tool_use_result
        .get("filePath")
        .and_then(Value::as_str)
        .or(tool_use.file_path.as_deref())
    else {
        return Ok(());
    };

    match tool_kind {
        "create" => {
            let content = tool_use_result
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("");
            let diff = content
                .lines()
                .map(|line| format!("+{line}"))
                .collect::<Vec<_>>()
                .join("\n");
            recorder.push_diff(
                file_path,
                &format!("Created {}", short_file_name(file_path)),
                &diff,
                ChangeType::Create,
            )?;
        }
        "update" => {
            let diff = tool_use_result
                .get("structuredPatch")
                .and_then(Value::as_array)
                .map(|patches| flatten_structured_patch(patches.as_slice()))
                .filter(|diff| !diff.trim().is_empty())
                .unwrap_or_else(|| {
                    fallback_file_diff(
                        tool_use_result
                            .get("originalFile")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        tool_use_result
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                    )
                });
            recorder.push_diff(
                file_path,
                &format!("Updated {}", short_file_name(file_path)),
                &diff,
                ChangeType::Edit,
            )?;
        }
        _ => {}
    }

    Ok(())
}

/// Extracts Claude tool result text.
fn extract_claude_tool_result_text(message: &Value, content: &Value) -> String {
    if let Some(text) = content.get("content").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(parts) = content.get("content").and_then(Value::as_array) {
        let combined = parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("content").and_then(Value::as_str))
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !combined.trim().is_empty() {
            return combined;
        }
    }
    if let Some(text) = message.get("tool_use_result").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(text) = message
        .get("tool_use_result")
        .and_then(|value| value.get("stderr"))
        .and_then(Value::as_str)
    {
        return text.to_owned();
    }

    "Claude tool call failed.".to_owned()
}
/// Returns whether permission denial.
fn is_permission_denial(detail: &str) -> bool {
    detail.contains("requested permissions")
}

/// Returns whether an AskUserQuestion error is the expected result of a
/// permission deny that TermAl deliberately sent. Match only TermAl's own
/// decision messages: a generic permission-looking failure can still be an
/// unexpected AskUserQuestion error that belongs in the transcript.
fn is_expected_claude_ask_user_question_denial(detail: &str) -> bool {
    detail.contains(CLAUDE_UNATTENDED_QUESTION_DENIAL)
        || detail.contains(CLAUDE_USER_DECLINED_QUESTION_MESSAGE)
}

/// Records Claude approval.
fn record_claude_approval(
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
    title: &str,
    command: &str,
    detail: &str,
) -> Result<()> {
    let key = format!("{title}\n{command}\n{detail}");
    if state.approval_keys_this_turn.insert(key) {
        recorder.push_approval(title, command, detail)?;
    }

    Ok(())
}

/// Describes Claude tool request.
fn describe_claude_tool_request(request: &ClaudeToolPermissionRequest) -> String {
    describe_claude_tool_action_from_parts(&request.tool_name, &request.tool_input)
}

/// Describes Claude tool action.
fn describe_claude_tool_action(tool_use: &ClaudeToolUse) -> String {
    match (
        tool_use.name.as_str(),
        tool_use.file_path.as_deref(),
        tool_use.command.as_deref(),
    ) {
        ("Write" | "Edit", Some(file_path), _) => format!("{} {}", tool_use.name, file_path),
        (_, _, Some(command)) => command.to_owned(),
        _ => tool_use.name.clone(),
    }
}

/// Describes Claude tool action from parts.
fn describe_claude_tool_action_from_parts(tool_name: &str, tool_input: &Value) -> String {
    match tool_name {
        "Write" | "Edit" => tool_input
            .get("file_path")
            .or_else(|| tool_input.get("filePath"))
            .and_then(Value::as_str)
            .map(|file_path| format!("{tool_name} {file_path}"))
            .unwrap_or_else(|| tool_name.to_owned()),
        "Bash" => tool_input
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| tool_name.to_owned()),
        _ => tool_name.to_owned(),
    }
}

/// Describes Claude permission detail.
fn describe_claude_permission_detail(
    tool_name: &str,
    tool_input: &Value,
    decision_reason: Option<&str>,
) -> String {
    let specific = match tool_name {
        "Write" => tool_input
            .get("file_path")
            .or_else(|| tool_input.get("filePath"))
            .and_then(Value::as_str)
            .map(|file_path| format!("Claude requested permission to write to {file_path}.")),
        "Edit" => tool_input
            .get("file_path")
            .or_else(|| tool_input.get("filePath"))
            .and_then(Value::as_str)
            .map(|file_path| format!("Claude requested permission to edit {file_path}.")),
        "Bash" => tool_input
            .get("command")
            .and_then(Value::as_str)
            .map(|command| format!("Claude requested permission to run `{command}`.")),
        _ => None,
    };

    match (
        specific,
        decision_reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty()),
    ) {
        (Some(specific), Some(reason)) => format!("{specific} Reason: {reason}."),
        (Some(specific), None) => specific,
        (None, Some(reason)) => format!("Claude requested approval. Reason: {reason}."),
        (None, None) => "Claude requested approval.".to_owned(),
    }
}

fn split_thinking_lines(thinking: &str) -> Vec<String> {
    let lines = thinking
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();

    if lines.is_empty() && !thinking.trim().is_empty() {
        vec![thinking.trim().to_owned()]
    } else {
        lines
    }
}
