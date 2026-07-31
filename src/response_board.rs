/*
Response board storage and HTTP handlers.

The board is deliberately a single durable SQLite collection. Card creation
accepts source IDs only, resolves the message from normalized transcript rows,
and stores an immutable snapshot without foreign keys so source pruning cannot
remove board content.
*/

const RESPONSE_BOARD_DEFAULT_WIDTH: f64 = 360.0;
const RESPONSE_BOARD_DEFAULT_HEIGHT: f64 = 420.0;
const RESPONSE_BOARD_MIN_WIDTH: f64 = 240.0;
const RESPONSE_BOARD_MAX_WIDTH: f64 = 1_600.0;
const RESPONSE_BOARD_MIN_HEIGHT: f64 = 160.0;
const RESPONSE_BOARD_MAX_HEIGHT: f64 = 1_600.0;
const RESPONSE_BOARD_MAX_COORDINATE: f64 = 1_000_000.0;
const RESPONSE_BOARD_MAX_CARDS: i64 = 256;
const RESPONSE_BOARD_MAX_SNAPSHOT_BYTES: usize = 1_048_576;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResponseBoardSnapshotRecord {
    message: Message,
    source_session_name: String,
    source_agent: Agent,
    source_message_position: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResponseBoardCard {
    id: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    snapshot: Message,
    source_session_id: String,
    source_message_id: String,
    source_message_position: usize,
    source_session_name: String,
    source_agent: Agent,
    created_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResponseBoard {
    cards: Vec<ResponseBoardCard>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateResponseBoardCardRequest {
    session_id: String,
    message_id: String,
    x: f64,
    y: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateResponseBoardCardRequest {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

fn response_board_storage_error(action: &str, err: impl std::fmt::Display) -> ApiError {
    eprintln!("response-board> {action}: {err}");
    ApiError::internal(format!("failed to {action}"))
}

fn validate_response_board_identifier(label: &str, value: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() {
        return Err(ApiError::bad_request(format!("{label} must not be empty")));
    }
    if value.len() > 512 {
        return Err(ApiError::bad_request(format!("{label} is too long")));
    }
    Ok(())
}

fn validate_response_board_coordinate(label: &str, value: f64) -> Result<(), ApiError> {
    if !value.is_finite() || value.abs() > RESPONSE_BOARD_MAX_COORDINATE {
        return Err(ApiError::bad_request(format!(
            "{label} must be finite and within the board coordinate range"
        )));
    }
    Ok(())
}

fn validate_response_board_size(label: &str, value: f64, min: f64, max: f64) -> Result<(), ApiError> {
    if !value.is_finite() || !(min..=max).contains(&value) {
        return Err(ApiError::bad_request(format!(
            "{label} must be between {min} and {max}"
        )));
    }
    Ok(())
}

fn decode_response_board_card_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResponseBoardCard> {
    let snapshot_json: String = row.get(5)?;
    let snapshot_record: ResponseBoardSnapshotRecord = serde_json::from_str(&snapshot_json)
        .map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(err),
            )
        })?;
    Ok(ResponseBoardCard {
        id: row.get(0)?,
        x: row.get(1)?,
        y: row.get(2)?,
        w: row.get(3)?,
        h: row.get(4)?,
        snapshot: snapshot_record.message,
        source_session_id: row.get(6)?,
        source_message_id: row.get(7)?,
        source_message_position: snapshot_record.source_message_position,
        source_session_name: snapshot_record.source_session_name,
        source_agent: snapshot_record.source_agent,
        created_at: row.get(8)?,
    })
}

fn response_board_select_columns() -> &'static str {
    "id, x, y, w, h, snapshot_json, source_session_id, source_message_id, created_at"
}

fn load_response_board(path: &FsPath) -> Result<ResponseBoard, ApiError> {
    let connection = open_sqlite_state_read_connection(path)
        .map_err(|err| response_board_storage_error("open the response board", err))?;
    let sql = format!(
        "SELECT {} FROM board_cards ORDER BY created_at ASC, id ASC",
        response_board_select_columns()
    );
    let mut statement = connection
        .prepare(&sql)
        .map_err(|err| response_board_storage_error("prepare the response board", err))?;
    let cards = statement
        .query_map([], decode_response_board_card_row)
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .map_err(|err| response_board_storage_error("read the response board", err))?;
    Ok(ResponseBoard { cards })
}

fn insert_response_board_card(
    path: &FsPath,
    request: CreateResponseBoardCardRequest,
) -> Result<ResponseBoardCard, ApiError> {
    validate_response_board_identifier("sessionId", &request.session_id)?;
    validate_response_board_identifier("messageId", &request.message_id)?;
    validate_response_board_coordinate("x", request.x)?;
    validate_response_board_coordinate("y", request.y)?;

    let connection = open_sqlite_state_connection(path)
        .map_err(|err| response_board_storage_error("open the response board", err))?;
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    let transaction = connection
        .unchecked_transaction()
        .map_err(|err| response_board_storage_error("start a response board update", err))?;
    let card_count: i64 = transaction
        .query_row("SELECT COUNT(*) FROM board_cards", [], |row| row.get(0))
        .map_err(|err| response_board_storage_error("count response board cards", err))?;
    if card_count >= RESPONSE_BOARD_MAX_CARDS {
        return Err(ApiError::conflict(format!(
            "the response board is limited to {RESPONSE_BOARD_MAX_CARDS} cards"
        )));
    }

    let session_json: String = transaction
        .query_row(
            "SELECT value_json FROM sessions WHERE id = ?1",
            rusqlite::params![request.session_id],
            |row| row.get(0),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => ApiError::not_found("source session not found"),
            other => response_board_storage_error("read the source session", other),
        })?;
    let session_record: PersistedSessionRecord = serde_json::from_str(&session_json)
        .map_err(|err| response_board_storage_error("decode the source session", err))?;
    if session_record.session.id != request.session_id {
        return Err(response_board_storage_error(
            "validate the source session",
            "persisted session identity mismatch",
        ));
    }

    let (position, message_json): (i64, String) = transaction
        .query_row(
            "SELECT position, value_json FROM messages
             WHERE session_id = ?1 AND message_id = ?2",
            rusqlite::params![request.session_id, request.message_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => {
                ApiError::not_found("source message is not available in durable history")
            }
            other => response_board_storage_error("read the source message", other),
        })?;
    let message: Message = serde_json::from_str(&message_json)
        .map_err(|err| response_board_storage_error("decode the source message", err))?;
    if message.id() != request.message_id {
        return Err(response_board_storage_error(
            "validate the source message",
            "persisted message identity mismatch",
        ));
    }
    let source_message_position = usize::try_from(position).map_err(|_| {
        response_board_storage_error("validate the source message", "invalid message position")
    })?;
    let snapshot_record = ResponseBoardSnapshotRecord {
        message: message.clone(),
        source_session_name: session_record.session.name.clone(),
        source_agent: session_record.session.agent,
        source_message_position,
    };
    let snapshot_json = serde_json::to_string(&snapshot_record)
        .map_err(|err| response_board_storage_error("encode the response snapshot", err))?;
    if snapshot_json.len() > RESPONSE_BOARD_MAX_SNAPSHOT_BYTES {
        return Err(ApiError::bad_request(format!(
            "source message exceeds the {} byte response-board snapshot limit",
            RESPONSE_BOARD_MAX_SNAPSHOT_BYTES
        )));
    }

    let card = ResponseBoardCard {
        id: Uuid::new_v4().to_string(),
        x: request.x,
        y: request.y,
        w: RESPONSE_BOARD_DEFAULT_WIDTH,
        h: RESPONSE_BOARD_DEFAULT_HEIGHT,
        snapshot: message,
        source_session_id: request.session_id,
        source_message_id: request.message_id,
        source_message_position,
        source_session_name: session_record.session.name,
        source_agent: session_record.session.agent,
        created_at: chrono::Utc::now()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    };
    transaction
        .execute(
            "INSERT INTO board_cards(
               id, x, y, w, h, snapshot_json, source_session_id, source_message_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                card.id,
                card.x,
                card.y,
                card.w,
                card.h,
                snapshot_json,
                card.source_session_id,
                card.source_message_id,
                card.created_at,
            ],
        )
        .map_err(|err| response_board_storage_error("store the response-board card", err))?;
    transaction
        .commit()
        .map_err(|err| response_board_storage_error("commit the response-board card", err))?;
    Ok(card)
}

fn patch_response_board_card(
    path: &FsPath,
    card_id: &str,
    request: UpdateResponseBoardCardRequest,
) -> Result<ResponseBoardCard, ApiError> {
    validate_response_board_identifier("card id", card_id)?;
    validate_response_board_coordinate("x", request.x)?;
    validate_response_board_coordinate("y", request.y)?;
    validate_response_board_size(
        "w",
        request.w,
        RESPONSE_BOARD_MIN_WIDTH,
        RESPONSE_BOARD_MAX_WIDTH,
    )?;
    validate_response_board_size(
        "h",
        request.h,
        RESPONSE_BOARD_MIN_HEIGHT,
        RESPONSE_BOARD_MAX_HEIGHT,
    )?;

    let connection = open_sqlite_state_connection(path)
        .map_err(|err| response_board_storage_error("open the response board", err))?;
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    let changed = connection
        .execute(
            "UPDATE board_cards SET x = ?2, y = ?3, w = ?4, h = ?5 WHERE id = ?1",
            rusqlite::params![card_id, request.x, request.y, request.w, request.h],
        )
        .map_err(|err| response_board_storage_error("update the response-board card", err))?;
    if changed == 0 {
        return Err(ApiError::not_found("response-board card not found"));
    }
    let sql = format!(
        "SELECT {} FROM board_cards WHERE id = ?1",
        response_board_select_columns()
    );
    connection
        .query_row(&sql, rusqlite::params![card_id], decode_response_board_card_row)
        .map_err(|err| response_board_storage_error("read the updated response-board card", err))
}

fn remove_response_board_card(path: &FsPath, card_id: &str) -> Result<(), ApiError> {
    validate_response_board_identifier("card id", card_id)?;
    let connection = open_sqlite_state_connection(path)
        .map_err(|err| response_board_storage_error("open the response board", err))?;
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    let changed = connection
        .execute(
            "DELETE FROM board_cards WHERE id = ?1",
            rusqlite::params![card_id],
        )
        .map_err(|err| response_board_storage_error("remove the response-board card", err))?;
    if changed == 0 {
        return Err(ApiError::not_found("response-board card not found"));
    }
    Ok(())
}

async fn get_response_board(
    State(state): State<AppState>,
) -> Result<Json<ResponseBoard>, ApiError> {
    let path = state.persistence_path.as_ref().clone();
    let board = run_blocking_api(move || load_response_board(&path)).await?;
    Ok(Json(board))
}

async fn create_response_board_card(
    State(state): State<AppState>,
    request: Result<Json<CreateResponseBoardCardRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ResponseBoardCard>), ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("response-board card", rejection))?;
    let path = state.persistence_path.as_ref().clone();
    let card = run_blocking_api(move || insert_response_board_card(&path, request)).await?;
    Ok((StatusCode::CREATED, Json(card)))
}

async fn update_response_board_card(
    AxumPath(card_id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<UpdateResponseBoardCardRequest>, JsonRejection>,
) -> Result<Json<ResponseBoardCard>, ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("response-board card update", rejection))?;
    let path = state.persistence_path.as_ref().clone();
    let card =
        run_blocking_api(move || patch_response_board_card(&path, &card_id, request)).await?;
    Ok(Json(card))
}

async fn delete_response_board_card(
    AxumPath(card_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    let path = state.persistence_path.as_ref().clone();
    run_blocking_api(move || remove_response_board_card(&path, &card_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}
