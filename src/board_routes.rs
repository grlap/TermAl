/*
Coordination board HTTP surface (tm-uwx.7.3).

Owns: session-scoped board routes (list/get/set — scope deletion is
deliberately route-less, see the comment where its handler used to live),
their wire request types, caller→scope resolution (session → local project →
scope id), and store-error → ApiError mapping. Deliberately does NOT own: store
semantics, validation, CAS/idempotency (src/coordination_board.rs — the store
is the single validation authority and its Validation messages pass through
verbatim), MCP tool exposure (src/delegation_mcp.rs), or UI rendering.
New file for the tm-uwx.7 board feature; split design mirrors
src/mailboxes.rs' route section.

Wire notes:
- `set` distinguishes "set to JSON null" from "delete": JSON cannot express
  Rust's Some(Null) vs None, so deletion uses an explicit `delete: true` with
  the `value` field ABSENT; a present `value` (including `null`) is a set.
- Values arriving here have passed serde_json's default 128-deep parser
  recursion limit; the store's own depth walk is independently iterative and
  safe regardless (hardened after the review exchange), so the parser limit
  is belt-and-suspenders rather than load-bearing.
- Read responses never contain tombstones; `value` is always present there.
*/

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BoardSetRequest {
    key: String,
    /// Present (any JSON, `null` included) = set. Absent = requires
    /// `delete: true`. The custom deserializer preserves explicit `null`
    /// as `Some(Value::Null)` instead of collapsing it into "absent".
    #[serde(default, deserialize_with = "deserialize_present_json_value")]
    value: Option<Value>,
    #[serde(default)]
    delete: bool,
    /// 0 = create-only (key must never have existed); otherwise the exact
    /// current revision — including a tombstone's revision for the
    /// deliberate-restore path (design v1.1).
    expected_revision: u64,
    idempotency_key: String,
    #[serde(default)]
    state_stamp: Option<String>,
}

fn deserialize_present_json_value<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BoardListQuery {
    #[serde(default)]
    after_key: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    snapshot_generation: Option<u64>,
    #[serde(default)]
    known_generation: Option<u64>,
}

/// Resolves the caller session to its coordination-board scope: the session
/// must be a LOCAL ROOT session (the backend is the authority here — MCP
/// tool filtering is defense-in-depth, not the gate), must belong to a
/// project, and that project must be LOCAL — the board is
/// local-authoritative in v1 (design v1.1: remote projects and proxies are
/// rejected here, before any store call). The root predicate mirrors
/// `mailbox_peer_names` exactly: the two delegation-child sources are
/// INDEPENDENT — rejection fires on parent marker OR durable-index
/// membership, so root standing requires marker null AND index absence
/// (the acceptance-side conjunction pinned by the eligibility work in
/// tm-487's lineage).
fn resolve_board_scope_for_session(
    state: &AppState,
    session_id: &str,
) -> Result<(String, String), ApiError> {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let session_index = inner
        .find_session_index(session_id)
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    let record = &inner.sessions[session_index];
    if record.hidden
        || record.is_remote_proxy()
        || record.session.parent_delegation_id.is_some()
        || inner
            .find_delegation_index_by_child_session_id(&record.session.id)
            .is_some()
    {
        return Err(ApiError::bad_request(
            "session must be a local root session",
        ));
    }
    // Session display names predate the board's bounded audit fields and can
    // contain arbitrary persisted text. A valid root session must not lose
    // board access because its human-facing name is oversized or contains a
    // control character. Store a safe, bounded display snapshot; the stable
    // author_session_id remains the authoritative identity.
    let author_name = coordination_board_author_name(&record.session.name);
    let project_id = record
        .session
        .project_id
        .clone()
        .ok_or_else(|| {
            ApiError::bad_request(
                "session has no project; the coordination board is scoped to a project",
            )
        })?;
    let project = inner.find_project(&project_id).ok_or_else(|| {
        ApiError::bad_request(format!("unknown project `{project_id}`"))
    })?;
    if project.remote_id != default_local_remote_id() {
        return Err(ApiError::bad_request(
            "the coordination board is local-authoritative in v1; remote projects are not \
             supported",
        ));
    }
    Ok((project.id.clone(), author_name))
}

fn coordination_board_author_name(session_name: &str) -> String {
    let mut normalized = String::with_capacity(
        session_name
            .len()
            .min(COORDINATION_BOARD_MAX_AUTHOR_NAME_BYTES),
    );
    for character in session_name.trim().chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if normalized.len() + character.len_utf8() > COORDINATION_BOARD_MAX_AUTHOR_NAME_BYTES {
            break;
        }
        normalized.push(character);
    }
    let normalized = normalized.trim();
    if normalized.is_empty() {
        "Unnamed session".to_owned()
    } else {
        normalized.to_owned()
    }
}

async fn list_coordination_board(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    query: Result<Query<BoardListQuery>, QueryRejection>,
) -> Result<Json<CoordinationBoardListPage>, ApiError> {
    let Query(query) =
        query.map_err(|rejection| api_query_rejection("coordination board list query", rejection))?;
    let page = run_blocking_api(move || {
        let (scope_project_id, _) = resolve_board_scope_for_session(&state, &session_id)?;
        state
            .coordination_board_store
            .list(&CoordinationBoardListRequest {
                scope_project_id,
                after_key: query.after_key,
                limit: query.limit,
                snapshot_generation: query.snapshot_generation,
                known_generation: query.known_generation,
            })
            .map_err(board_api_error)
    })
    .await?;
    Ok(Json(page))
}

async fn get_coordination_board_key(
    AxumPath((session_id, key)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<CoordinationBoardGetResponse>, ApiError> {
    let head = run_blocking_api(move || {
        let (scope_project_id, _) = resolve_board_scope_for_session(&state, &session_id)?;
        state
            .coordination_board_store
            .get(&scope_project_id, &key)
            .map_err(board_api_error)
    })
    .await?;
    Ok(Json(head))
}

async fn set_coordination_board_key(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<BoardSetRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CoordinationBoardSetReceipt>), ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("coordination board set", rejection))?;
    let receipt = run_blocking_api(move || {
        let (scope_project_id, author_name) =
            resolve_board_scope_for_session(&state, &session_id)?;
        let value = match (request.value, request.delete) {
            (Some(_), true) => {
                return Err(ApiError::bad_request(
                    "`value` and `delete: true` are mutually exclusive; omit `value` to delete",
                ));
            }
            (None, false) => {
                return Err(ApiError::bad_request(
                    "provide `value` to set (JSON null is a value) or `delete: true` to delete",
                ));
            }
            (value, _) => value,
        };
        state
            .coordination_board_store
            .set(&CoordinationBoardSetInput {
                scope_project_id,
                key: request.key,
                value,
                expected_revision: request.expected_revision,
                author_session_id: session_id,
                author_name,
                idempotency_key: request.idempotency_key,
                state_stamp: request.state_stamp,
            })
            .map_err(board_api_error)
    })
    .await?;
    let status = if receipt.prior_revision == 0 && !receipt.duplicate {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(receipt)))
}

// Scope deletion deliberately has NO route: it is lifecycle cleanup owned by
// project deletion (session_crud.rs delete_project), not an agent mutation.
// An agent-facing scope wipe would bypass per-key CAS and erase idempotency
// receipts and tombstones wholesale, reopening the ABA class the store was
// hardened against (surface review, mailbox #219).

/// Maps store errors onto the mailbox precedent: Validation→400,
/// Conflict→409, NotFound→404, Retryable→503 (the typed no-commit clause
/// passes through verbatim so bridge-side replay classification keeps
/// working). Conflict/NotFound detail — current head (tombstones carry
/// `deleted: true` with a structurally null value) and current generation —
/// is appended as compact JSON so CAS repair and the deliberate-restore path
/// need no second round trip.
fn board_api_error(err: anyhow::Error) -> ApiError {
    if let Some(board_error) = err.downcast_ref::<CoordinationBoardStoreError>() {
        let mut message = board_error.message.clone();
        let mut detail = serde_json::Map::new();
        if let Some(current) = board_error.current.as_ref() {
            if let Ok(current_json) = serde_json::to_value(current) {
                detail.insert("current".to_owned(), current_json);
            }
        }
        if let Some(generation) = board_error.current_generation {
            detail.insert("currentGeneration".to_owned(), json!(generation));
        }
        if !detail.is_empty() {
            message = format!("{message}; detail: {}", Value::Object(detail));
        }
        return match board_error.kind {
            CoordinationBoardStoreErrorKind::Validation => ApiError::bad_request(message),
            CoordinationBoardStoreErrorKind::Conflict => ApiError::conflict(message),
            CoordinationBoardStoreErrorKind::NotFound => ApiError::not_found(message),
            CoordinationBoardStoreErrorKind::Retryable => {
                ApiError::from_status(StatusCode::SERVICE_UNAVAILABLE, message)
            }
            // Only test-constructed stores are disabled; a production HTTP
            // path reaching one is a wiring bug, not a client error.
            CoordinationBoardStoreErrorKind::Disabled => ApiError::internal(message),
        };
    }
    ApiError::internal(format!("coordination board operation failed: {err:#}"))
}
