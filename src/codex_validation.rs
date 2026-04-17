// Codex app-server request payload validators.
//
// Each `submit_codex_*` route normalizes + validates the caller's payload
// before forwarding it back into the Codex app server. The validation
// rules enforce schema conformance (MCP elicitation form content, user
// input answer shapes, app-request result depth + size), reject values
// that would crash the Codex process or exceed its limits, and give the
// caller a BAD_REQUEST with a specific message rather than a silent
// protocol error.
//
// Covers user input answers, MCP elicitation submissions + form content +
// per-field value validation (string/array/number), app-request result
// with byte-size + depth caps, and the helper extractors for MCP
// elicitation string/array option sets.
//
// Extracted from state.rs into its own `include!()` fragment so state.rs
// stays focused on the core state model rather than protocol-specific
// request-body validators.

/// Validates Codex user input answers.
fn validate_codex_user_input_answers(
    questions: &[UserInputQuestion],
    answers: BTreeMap<String, Vec<String>>,
) -> std::result::Result<
    (
        BTreeMap<String, BTreeMap<String, Vec<String>>>,
        BTreeMap<String, Vec<String>>,
    ),
    ApiError,
> {
    if questions.is_empty() {
        return Err(ApiError::bad_request(
            "Codex did not include any questions for this request",
        ));
    }

    let question_ids: HashSet<&str> = questions
        .iter()
        .map(|question| question.id.as_str())
        .collect();
    for answer_id in answers.keys() {
        if !question_ids.contains(answer_id.as_str()) {
            return Err(ApiError::bad_request(format!(
                "answer `{answer_id}` does not match any requested question"
            )));
        }
    }

    let mut response_answers = BTreeMap::new();
    let mut display_answers = BTreeMap::new();
    for question in questions {
        let Some(raw_answers) = answers.get(&question.id) else {
            return Err(ApiError::bad_request(format!(
                "question `{}` is missing an answer",
                question.header
            )));
        };

        let normalized_answers = raw_answers
            .iter()
            .map(|answer: &String| answer.trim())
            .filter(|answer: &&str| !answer.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if normalized_answers.len() != 1 {
            return Err(ApiError::bad_request(format!(
                "question `{}` requires exactly one answer",
                question.header
            )));
        }

        if let Some(options) = question.options.as_ref() {
            let selected = &normalized_answers[0];
            let matches_option = options.iter().any(|option| option.label == *selected);
            if !matches_option && !question.is_other {
                return Err(ApiError::bad_request(format!(
                    "question `{}` must use one of the provided options",
                    question.header
                )));
            }
        }

        response_answers.insert(
            question.id.clone(),
            BTreeMap::from([("answers".to_owned(), normalized_answers.clone())]),
        );
        display_answers.insert(
            question.id.clone(),
            if question.is_secret {
                vec!["[secret provided]".to_owned()]
            } else {
                normalized_answers
            },
        );
    }

    Ok((response_answers, display_answers))
}

/// Validates Codex MCP elicitation submission.
fn validate_codex_mcp_elicitation_submission(
    request: &McpElicitationRequestPayload,
    action: McpElicitationAction,
    content: Option<Value>,
) -> std::result::Result<Option<Value>, ApiError> {
    let content = content.filter(|value| !value.is_null());
    match (&request.mode, action) {
        (McpElicitationRequestMode::Url { .. }, _) => {
            if content.is_some() {
                return Err(ApiError::bad_request(
                    "URL-based MCP elicitations do not accept structured content",
                ));
            }
            Ok(None)
        }
        (McpElicitationRequestMode::Form { .. }, McpElicitationAction::Accept) => {
            let content = content.ok_or_else(|| {
                ApiError::bad_request(
                    "accepted MCP elicitation responses must include structured content",
                )
            })?;
            Ok(Some(validate_codex_mcp_elicitation_form_content(
                request, content,
            )?))
        }
        (McpElicitationRequestMode::Form { .. }, _) => {
            if content.is_some() {
                return Err(ApiError::bad_request(
                    "declined or canceled MCP elicitations cannot include structured content",
                ));
            }
            Ok(None)
        }
    }
}

/// Validates Codex MCP elicitation form content.
fn validate_codex_mcp_elicitation_form_content(
    request: &McpElicitationRequestPayload,
    content: Value,
) -> std::result::Result<Value, ApiError> {
    let McpElicitationRequestMode::Form {
        requested_schema, ..
    } = &request.mode
    else {
        return Err(ApiError::bad_request(
            "structured content is only supported for form-mode MCP elicitations",
        ));
    };

    if requested_schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(ApiError::bad_request(
            "MCP elicitation schema must be an object schema",
        ));
    }
    let properties = requested_schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ApiError::bad_request("MCP elicitation schema is missing form properties")
        })?;
    let required = requested_schema
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let object = content
        .as_object()
        .ok_or_else(|| ApiError::bad_request("MCP elicitation content must be a JSON object"))?;

    for key in object.keys() {
        if !properties.contains_key(key) {
            return Err(ApiError::bad_request(format!(
                "field `{key}` is not part of this MCP elicitation",
            )));
        }
    }
    for required_key in required.iter().filter_map(Value::as_str) {
        if !object.contains_key(required_key) {
            return Err(ApiError::bad_request(format!(
                "field `{required_key}` is required for this MCP elicitation",
            )));
        }
    }

    let mut normalized = serde_json::Map::new();
    for (key, value) in object {
        let schema = properties.get(key).ok_or_else(|| {
            ApiError::bad_request(format!("field `{key}` is not part of this MCP elicitation",))
        })?;
        normalized.insert(
            key.clone(),
            validate_codex_mcp_elicitation_field_value(key, schema, value)?,
        );
    }

    Ok(Value::Object(normalized))
}

/// Validates Codex MCP elicitation field value.
fn validate_codex_mcp_elicitation_field_value(
    field_name: &str,
    schema: &Value,
    value: &Value,
) -> std::result::Result<Value, ApiError> {
    match schema.get("type").and_then(Value::as_str) {
        Some("boolean") => {
            if !value.is_boolean() {
                return Err(ApiError::bad_request(format!(
                    "field `{field_name}` must be true or false",
                )));
            }
            Ok(value.clone())
        }
        Some("number") => {
            validate_codex_mcp_elicitation_number_value(field_name, schema, value, false)
        }
        Some("integer") => {
            validate_codex_mcp_elicitation_number_value(field_name, schema, value, true)
        }
        Some("string") => validate_codex_mcp_elicitation_string_value(field_name, schema, value),
        Some("array") => validate_codex_mcp_elicitation_array_value(field_name, schema, value),
        Some(other) => Err(ApiError::bad_request(format!(
            "field `{field_name}` uses unsupported MCP elicitation type `{other}`",
        ))),
        None => Err(ApiError::bad_request(format!(
            "field `{field_name}` is missing an MCP elicitation type",
        ))),
    }
}

/// Validates Codex MCP elicitation number value.
fn validate_codex_mcp_elicitation_number_value(
    field_name: &str,
    schema: &Value,
    value: &Value,
    require_integer: bool,
) -> std::result::Result<Value, ApiError> {
    let Some(number) = value.as_f64() else {
        let expected = if require_integer {
            "an integer"
        } else {
            "a number"
        };
        return Err(ApiError::bad_request(format!(
            "field `{field_name}` must be {expected}",
        )));
    };

    if require_integer && value.as_i64().is_none() && value.as_u64().is_none() {
        return Err(ApiError::bad_request(format!(
            "field `{field_name}` must be an integer",
        )));
    }

    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64) {
        if number < minimum {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` must be at least {minimum}",
            )));
        }
    }
    if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64) {
        if number > maximum {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` must be at most {maximum}",
            )));
        }
    }

    Ok(value.clone())
}

const CODEX_APP_REQUEST_RESULT_MAX_BYTES: usize = 64 * 1024;
const CODEX_APP_REQUEST_RESULT_MAX_DEPTH: usize = 32;

/// Validates Codex app request result.
fn validate_codex_app_request_result(result: Value) -> std::result::Result<Value, ApiError> {
    validate_codex_app_request_result_depth(&result, 0)?;
    let encoded = serde_json::to_vec(&result).map_err(|err| {
        ApiError::bad_request(format!(
            "Codex app request result could not be serialized as JSON: {err}"
        ))
    })?;
    if encoded.len() > CODEX_APP_REQUEST_RESULT_MAX_BYTES {
        return Err(ApiError::bad_request(format!(
            "Codex app request result must be at most {} KB",
            CODEX_APP_REQUEST_RESULT_MAX_BYTES / 1024
        )));
    }
    Ok(result)
}

/// Validates Codex app request result depth.
fn validate_codex_app_request_result_depth(
    value: &Value,
    depth: usize,
) -> std::result::Result<(), ApiError> {
    if depth > CODEX_APP_REQUEST_RESULT_MAX_DEPTH {
        return Err(ApiError::bad_request(format!(
            "Codex app request result must be at most {CODEX_APP_REQUEST_RESULT_MAX_DEPTH} levels deep",
        )));
    }

    match value {
        Value::Array(values) => {
            for entry in values {
                validate_codex_app_request_result_depth(entry, depth + 1)?;
            }
        }
        Value::Object(entries) => {
            for entry in entries.values() {
                validate_codex_app_request_result_depth(entry, depth + 1)?;
            }
        }
        _ => {}
    }

    Ok(())
}

/// Validates Codex MCP elicitation string value.
fn validate_codex_mcp_elicitation_string_value(
    field_name: &str,
    schema: &Value,
    value: &Value,
) -> std::result::Result<Value, ApiError> {
    let Some(text) = value.as_str() else {
        return Err(ApiError::bad_request(format!(
            "field `{field_name}` must be a string",
        )));
    };

    if let Some(min_length) = schema.get("minLength").and_then(Value::as_u64) {
        if text.chars().count() < min_length as usize {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` must be at least {min_length} characters",
            )));
        }
    }
    if let Some(max_length) = schema.get("maxLength").and_then(Value::as_u64) {
        if text.chars().count() > max_length as usize {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` must be at most {max_length} characters",
            )));
        }
    }

    if let Some(options) = codex_mcp_elicitation_string_options(schema) {
        if !options.iter().any(|option| option == text) {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` must use one of the provided options",
            )));
        }
    }

    Ok(Value::String(text.to_owned()))
}

/// Validates Codex MCP elicitation array value.
fn validate_codex_mcp_elicitation_array_value(
    field_name: &str,
    schema: &Value,
    value: &Value,
) -> std::result::Result<Value, ApiError> {
    let values = value
        .as_array()
        .ok_or_else(|| ApiError::bad_request(format!("field `{field_name}` must be a list")))?;

    if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64) {
        if values.len() < min_items as usize {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` must include at least {min_items} selections",
            )));
        }
    }
    if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64) {
        if values.len() > max_items as usize {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` must include at most {max_items} selections",
            )));
        }
    }

    let item_schema = schema.get("items").ok_or_else(|| {
        ApiError::bad_request(format!(
            "field `{field_name}` is missing its MCP elicitation item schema",
        ))
    })?;
    let allowed = codex_mcp_elicitation_array_options(item_schema);
    let mut normalized = Vec::with_capacity(values.len());
    for entry in values {
        let Some(text) = entry.as_str() else {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` only accepts string selections",
            )));
        };
        if let Some(options) = allowed.as_ref() {
            if !options.iter().any(|option| option == text) {
                return Err(ApiError::bad_request(format!(
                    "field `{field_name}` must use one of the provided options",
                )));
            }
        }
        normalized.push(Value::String(text.to_owned()));
    }

    Ok(Value::Array(normalized))
}

/// Handles Codex MCP elicitation string options.
fn codex_mcp_elicitation_string_options(schema: &Value) -> Option<Vec<String>> {
    if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
        let collected = options
            .iter()
            .filter_map(|option| option.get("const").and_then(Value::as_str))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !collected.is_empty() {
            return Some(collected);
        }
    }

    let collected = schema
        .get("enum")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (!collected.is_empty()).then_some(collected)
}

/// Handles Codex MCP elicitation array options.
fn codex_mcp_elicitation_array_options(schema: &Value) -> Option<Vec<String>> {
    if let Some(options) = schema.get("anyOf").and_then(Value::as_array) {
        let collected = options
            .iter()
            .filter_map(|option| option.get("const").and_then(Value::as_str))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !collected.is_empty() {
            return Some(collected);
        }
    }

    let collected = schema
        .get("enum")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (!collected.is_empty()).then_some(collected)
}
