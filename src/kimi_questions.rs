// Adapts Kimi's permission-channel questions to the shared user-input card.
// Uses ACP's existing pending-request ownership and shared submission route.
// Kimi's permission fallback is single-select; option IDs are opaque values,
// and rejecting the request means the question was dismissed.

fn kimi_permission_is_question(params: &Value, options: &[Value]) -> bool {
    params.pointer("/toolCall/title").and_then(Value::as_str) == Some("AskUserQuestion")
        || params.get("toolName").and_then(Value::as_str) == Some("AskUserQuestion")
        || options
            .iter()
            .filter(|option| option["kind"] == "allow_once")
            .count()
            > 1
}

fn kimi_permission_question(params: &Value, options: &[Value]) -> Option<UserInputQuestion> {
    let mut ids = HashSet::new();
    let mut choices = Vec::new();
    for option in options {
        let id = option.get("optionId")?.as_str()?.to_owned();
        if id.is_empty() || !ids.insert(id.clone()) {
            return None;
        }
        match option.get("kind")?.as_str()? {
            "allow_once" | "allow_always" => {
                let label = option.get("name")?.as_str()?.to_owned();
                if label.trim().is_empty() {
                    return None;
                }
                choices.push(UserInputQuestionOption {
                    id: Some(id),
                    label,
                    description: String::new(),
                });
            }
            "reject_once" | "reject_always" => {}
            _ => return None,
        }
    }
    if choices.is_empty() {
        return None;
    }
    let text = params
        .pointer("/toolCall/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.pointer("/content/text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    let text = if text.trim().is_empty() {
        params
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("Choose an answer.")
    } else {
        &text
    };
    Some(UserInputQuestion {
        id: "question".to_owned(),
        header: "Question".to_owned(),
        question: truncate_acp_approval_detail(text),
        is_other: false,
        is_secret: false,
        multi_select: false,
        options: Some(choices),
    })
}

/// Validates the submitted opaque value against this still-live question.
/// Labels are presentation only, so equal labels cannot alias two choices.
fn kimi_question_answer(
    pending: &AcpPendingApproval,
    answers: BTreeMap<String, Vec<String>>,
    declined: bool,
) -> Result<(Value, Option<BTreeMap<String, Vec<String>>>), ApiError> {
    let question = pending
        .question
        .as_ref()
        .ok_or_else(|| ApiError::conflict("this request is not a question"))?;
    if declined {
        return Ok((
            match &pending.reject_option_id {
                Some(option_id) => json!({"outcome":"selected", "optionId":option_id}),
                None => json!({"outcome":"cancelled"}),
            },
            None,
        ));
    }
    let values = answers
        .get(&question.id)
        .filter(|_| answers.len() == 1)
        .filter(|values| values.len() == 1)
        .ok_or_else(|| ApiError::bad_request("choose exactly one answer to this question"))?;
    let selected = &values[0];
    if !question.options.as_ref().is_some_and(|options| {
        options
            .iter()
            .any(|option| option.id.as_ref() == Some(selected))
    }) {
        return Err(ApiError::bad_request(
            "the selected answer was not offered by Kimi",
        ));
    }
    Ok((
        json!({"outcome":"selected", "optionId":selected}),
        Some(answers),
    ))
}
