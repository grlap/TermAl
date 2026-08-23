/*
Response board storage and HTTP handlers.

The board is deliberately one durable SQLite collection with a global staging
inbox and placed-card canvases partitioned by inner tabs. Cards keep immutable
message snapshots and an explicit staged/placed state; geometry never doubles
as workflow state. The legacy board endpoint is the placed-card view of the
deterministic default tab.
*/

const RESPONSE_BOARD_DEFAULT_TAB_ID: &str = "response-board-default";
const RESPONSE_BOARD_DEFAULT_TAB_NAME: &str = "Board";

const RESPONSE_BOARD_DEFAULT_WIDTH: f64 = 360.0;
const RESPONSE_BOARD_DEFAULT_HEIGHT: f64 = 420.0;
const RESPONSE_BOARD_MIN_WIDTH: f64 = 240.0;
const RESPONSE_BOARD_MAX_WIDTH: f64 = 1_600.0;
const RESPONSE_BOARD_MIN_HEIGHT: f64 = 160.0;
const RESPONSE_BOARD_MAX_HEIGHT: f64 = 1_600.0;
const RESPONSE_BOARD_MAX_COORDINATE: f64 = 1_000_000.0;
const RESPONSE_BOARD_MAX_CARDS: i64 = 256;
const RESPONSE_BOARD_MAX_SNAPSHOT_BYTES: usize = 1_048_576;
const RESPONSE_BOARD_MAX_TAB_NAME_BYTES: usize = 120;
const RESPONSE_BOARD_MAX_CUSTOM_TABS: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum ResponseBoardCardPlacement {
    Staged,
    Placed,
}

impl ResponseBoardCardPlacement {
    fn as_db_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Placed => "placed",
        }
    }

    fn from_db_str(value: &str) -> Result<Self, String> {
        match value {
            "staged" => Ok(Self::Staged),
            "placed" => Ok(Self::Placed),
            other => Err(format!("invalid response-board placement: {other}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum ResponseBoardTabKind {
    Custom,
    ProjectDefault,
}

impl ResponseBoardTabKind {
    fn as_db_str(self) -> &'static str {
        match self {
            Self::Custom => "custom",
            Self::ProjectDefault => "projectDefault",
        }
    }

    fn from_db_str(value: &str) -> Result<Self, String> {
        match value {
            "custom" => Ok(Self::Custom),
            "projectDefault" => Ok(Self::ProjectDefault),
            other => Err(format!("invalid response-board tab kind: {other}")),
        }
    }
}

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
    tab_id: String,
    placement: ResponseBoardCardPlacement,
    has_canvas_position: bool,
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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResponseBoardTab {
    id: String,
    name: String,
    kind: ResponseBoardTabKind,
    project_id: Option<String>,
    sort_order: i64,
    created_at: String,
    placed_card_count: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResponseBoardTabs {
    tabs: Vec<ResponseBoardTab>,
    staged_card_count: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResponseBoardTabView {
    tab: ResponseBoardTab,
    cards: Vec<ResponseBoardCard>,
    staged_cards: Vec<ResponseBoardCard>,
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
struct StageResponseBoardCardRequest {
    session_id: String,
    message_id: String,
    tab_id: Option<String>,
    project_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateResponseBoardCardRequest {
    x: Option<f64>,
    y: Option<f64>,
    w: Option<f64>,
    h: Option<f64>,
    tab_id: Option<String>,
    placement: Option<ResponseBoardCardPlacement>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateResponseBoardTabRequest {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateResponseBoardTabRequest {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReorderResponseBoardTabsRequest {
    tab_ids: Vec<String>,
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

fn validate_response_board_tab_name(value: &str) -> Result<String, ApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request("tab name must not be empty"));
    }
    if trimmed.len() > RESPONSE_BOARD_MAX_TAB_NAME_BYTES {
        return Err(ApiError::bad_request("tab name is too long"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "tab name must not contain control characters",
        ));
    }
    Ok(trimmed.to_owned())
}

fn response_board_invalid_data(message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}

/// Upgrades legacy singleton-board databases in place. Existing cards remain
/// byte-for-byte snapshots and become placed cards in the default partition.
fn ensure_sqlite_response_board_schema(connection: &rusqlite::Connection) -> anyhow::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS response_board_tabs (
           id TEXT PRIMARY KEY,
           name TEXT NOT NULL,
           kind TEXT NOT NULL,
           project_id TEXT UNIQUE,
           sort_order INTEGER NOT NULL,
           created_at TEXT NOT NULL
         );",
    )?;

    let columns = connection
        .prepare("PRAGMA table_info(board_cards)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|column| column == "tab_id") {
        connection.execute_batch(
            "ALTER TABLE board_cards
             ADD COLUMN tab_id TEXT NOT NULL DEFAULT 'response-board-default';",
        )?;
    }
    if !columns.iter().any(|column| column == "placement") {
        connection.execute_batch(
            "ALTER TABLE board_cards
             ADD COLUMN placement TEXT NOT NULL DEFAULT 'placed';",
        )?;
    }
    if !columns.iter().any(|column| column == "has_canvas_position") {
        connection.execute_batch(
            "ALTER TABLE board_cards
             ADD COLUMN has_canvas_position INTEGER NOT NULL DEFAULT 1;",
        )?;
    }

    connection.execute(
        "INSERT OR IGNORE INTO response_board_tabs(
           id, name, kind, project_id, sort_order, created_at
         ) VALUES (?1, ?2, 'custom', NULL, 0, ?3)",
        rusqlite::params![
            RESPONSE_BOARD_DEFAULT_TAB_ID,
            RESPONSE_BOARD_DEFAULT_TAB_NAME,
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ],
    )?;
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS board_cards_tab_placement_created_idx
           ON board_cards(tab_id, placement, created_at, id);",
    )?;
    Ok(())
}

fn decode_response_board_card_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResponseBoardCard> {
    let snapshot_json: String = row.get(8)?;
    let snapshot_record: ResponseBoardSnapshotRecord = serde_json::from_str(&snapshot_json)
        .map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                8,
                rusqlite::types::Type::Text,
                Box::new(err),
            )
        })?;
    Ok(ResponseBoardCard {
        id: row.get(0)?,
        tab_id: row.get(1)?,
        placement: ResponseBoardCardPlacement::from_db_str(&row.get::<_, String>(2)?)
            .map_err(response_board_invalid_data)?,
        has_canvas_position: row.get(3)?,
        x: row.get(4)?,
        y: row.get(5)?,
        w: row.get(6)?,
        h: row.get(7)?,
        snapshot: snapshot_record.message,
        source_session_id: row.get(9)?,
        source_message_id: row.get(10)?,
        source_message_position: snapshot_record.source_message_position,
        source_session_name: snapshot_record.source_session_name,
        source_agent: snapshot_record.source_agent,
        created_at: row.get(11)?,
    })
}

fn response_board_select_columns() -> &'static str {
    "id, tab_id, placement, has_canvas_position, x, y, w, h, snapshot_json, source_session_id, source_message_id, created_at"
}

fn decode_response_board_tab_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResponseBoardTab> {
    Ok(ResponseBoardTab {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: ResponseBoardTabKind::from_db_str(&row.get::<_, String>(2)?)
            .map_err(response_board_invalid_data)?,
        project_id: row.get(3)?,
        sort_order: row.get(4)?,
        created_at: row.get(5)?,
        placed_card_count: row.get(6)?,
    })
}

fn response_board_tab_select_sql() -> &'static str {
    "SELECT t.id, t.name, t.kind, t.project_id, t.sort_order, t.created_at,
            COUNT(c.id)
       FROM response_board_tabs t
       LEFT JOIN board_cards c ON c.tab_id = t.id AND c.placement = 'placed'"
}

fn load_response_board(path: &FsPath) -> Result<ResponseBoard, ApiError> {
    let connection = open_sqlite_state_read_connection(path)
        .map_err(|err| response_board_storage_error("open the response board", err))?;
    let sql = format!(
        "SELECT {} FROM board_cards
         WHERE tab_id = ?1 AND placement = 'placed'
         ORDER BY created_at ASC, id ASC",
        response_board_select_columns()
    );
    let mut statement = connection
        .prepare(&sql)
        .map_err(|err| response_board_storage_error("prepare the response board", err))?;
    let cards = statement
        .query_map(
            rusqlite::params![RESPONSE_BOARD_DEFAULT_TAB_ID],
            decode_response_board_card_row,
        )
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .map_err(|err| response_board_storage_error("read the response board", err))?;
    Ok(ResponseBoard { cards })
}

struct PreparedResponseBoardSnapshot {
    message: Message,
    source_message_position: usize,
    source_session_name: String,
    source_agent: Agent,
    snapshot_json: String,
}

fn prepare_response_board_snapshot(
    transaction: &rusqlite::Transaction<'_>,
    session_id: &str,
    message_id: &str,
) -> Result<PreparedResponseBoardSnapshot, ApiError> {
    let session_json: String = transaction
        .query_row(
            "SELECT value_json FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => ApiError::not_found("source session not found"),
            other => response_board_storage_error("read the source session", other),
        })?;
    let session_record: PersistedSessionRecord = serde_json::from_str(&session_json)
        .map_err(|err| response_board_storage_error("decode the source session", err))?;
    if session_record.session.id != session_id {
        return Err(response_board_storage_error(
            "validate the source session",
            "persisted session identity mismatch",
        ));
    }

    let (position, message_json): (i64, String) = transaction
        .query_row(
            "SELECT position, value_json FROM messages
             WHERE session_id = ?1 AND message_id = ?2",
            rusqlite::params![session_id, message_id],
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
    if message.id() != message_id {
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
    Ok(PreparedResponseBoardSnapshot {
        message,
        source_message_position,
        source_session_name: session_record.session.name,
        source_agent: session_record.session.agent,
        snapshot_json,
    })
}

fn response_board_tab_exists(
    connection: &rusqlite::Connection,
    tab_id: &str,
) -> Result<bool, ApiError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM response_board_tabs WHERE id = ?1)",
            rusqlite::params![tab_id],
            |row| row.get(0),
        )
        .map_err(|err| response_board_storage_error("read the response-board tab", err))
}

fn enforce_response_board_tab_capacity(
    transaction: &rusqlite::Transaction<'_>,
    tab_id: &str,
) -> Result<(), ApiError> {
    let count: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM board_cards
             WHERE tab_id = ?1 AND placement = 'placed'",
            rusqlite::params![tab_id],
            |row| row.get(0),
        )
        .map_err(|err| response_board_storage_error("count response-board cards", err))?;
    if count >= RESPONSE_BOARD_MAX_CARDS {
        return Err(ApiError::conflict(format!(
            "a response-board tab is limited to {RESPONSE_BOARD_MAX_CARDS} cards"
        )));
    }
    Ok(())
}

fn enforce_response_board_staging_capacity(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<(), ApiError> {
    let count: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM board_cards WHERE placement = 'staged'",
            [],
            |row| row.get(0),
        )
        .map_err(|err| response_board_storage_error("count staged responses", err))?;
    if count >= RESPONSE_BOARD_MAX_CARDS {
        return Err(ApiError::conflict(format!(
            "response-board staging is limited to {RESPONSE_BOARD_MAX_CARDS} cards"
        )));
    }
    Ok(())
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
    let duplicate: bool = transaction
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM board_cards
               WHERE source_session_id = ?1 AND source_message_id = ?2
             )",
            rusqlite::params![&request.session_id, &request.message_id],
            |row| row.get(0),
        )
        .map_err(|err| response_board_storage_error("check response-board duplicates", err))?;
    if duplicate {
        return Err(ApiError::conflict(
            "that response is already on the response board",
        ));
    }
    enforce_response_board_tab_capacity(&transaction, RESPONSE_BOARD_DEFAULT_TAB_ID)?;
    let snapshot = prepare_response_board_snapshot(
        &transaction,
        &request.session_id,
        &request.message_id,
    )?;

    let card = ResponseBoardCard {
        id: Uuid::new_v4().to_string(),
        tab_id: RESPONSE_BOARD_DEFAULT_TAB_ID.to_owned(),
        placement: ResponseBoardCardPlacement::Placed,
        has_canvas_position: true,
        x: request.x,
        y: request.y,
        w: RESPONSE_BOARD_DEFAULT_WIDTH,
        h: RESPONSE_BOARD_DEFAULT_HEIGHT,
        snapshot: snapshot.message,
        source_session_id: request.session_id,
        source_message_id: request.message_id,
        source_message_position: snapshot.source_message_position,
        source_session_name: snapshot.source_session_name,
        source_agent: snapshot.source_agent,
        created_at: chrono::Utc::now()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    };
    transaction
        .execute(
            "INSERT INTO board_cards(
               id, tab_id, placement, has_canvas_position, x, y, w, h,
               snapshot_json, source_session_id, source_message_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                card.id,
                card.tab_id,
                card.placement.as_db_str(),
                card.has_canvas_position,
                card.x,
                card.y,
                card.w,
                card.h,
                snapshot.snapshot_json,
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
    if let Some(value) = request.x {
        validate_response_board_coordinate("x", value)?;
    }
    if let Some(value) = request.y {
        validate_response_board_coordinate("y", value)?;
    }
    if let Some(value) = request.w {
        validate_response_board_size(
            "w",
            value,
            RESPONSE_BOARD_MIN_WIDTH,
            RESPONSE_BOARD_MAX_WIDTH,
        )?;
    }
    if let Some(value) = request.h {
        validate_response_board_size(
            "h",
            value,
            RESPONSE_BOARD_MIN_HEIGHT,
            RESPONSE_BOARD_MAX_HEIGHT,
        )?;
    }
    let connection = open_sqlite_state_connection(path)
        .map_err(|err| response_board_storage_error("open the response board", err))?;
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    let transaction = connection
        .unchecked_transaction()
        .map_err(|err| response_board_storage_error("start a response board update", err))?;
    if let Some(tab_id) = request.tab_id.as_deref() {
        validate_response_board_identifier("tabId", tab_id)?;
    }
    let sql = format!(
        "SELECT {} FROM board_cards WHERE id = ?1",
        response_board_select_columns()
    );
    let current = transaction
        .query_row(
            &sql,
            rusqlite::params![card_id],
            decode_response_board_card_row,
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => {
                ApiError::not_found("response-board card not found")
            }
            other => response_board_storage_error("read the response-board card", other),
        })?;
    let next_tab_id = request.tab_id.unwrap_or_else(|| current.tab_id.clone());
    let next_placement = request.placement.unwrap_or(current.placement);
    if next_tab_id != current.tab_id {
        if !response_board_tab_exists(&transaction, &next_tab_id)? {
            return Err(ApiError::not_found("response-board tab not found"));
        }
    }
    let placement_or_tab_changed =
        current.placement != next_placement || next_tab_id != current.tab_id;
    if next_placement == ResponseBoardCardPlacement::Placed && placement_or_tab_changed {
        let duplicate: bool = transaction
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM board_cards
                   WHERE tab_id = ?1 AND placement = 'placed'
                     AND source_session_id = ?2
                     AND source_message_id = ?3 AND id <> ?4
                 )",
                rusqlite::params![
                    next_tab_id,
                    current.source_session_id,
                    current.source_message_id,
                    card_id,
                ],
                |row| row.get(0),
            )
            .map_err(|err| response_board_storage_error("check response-board duplicates", err))?;
        if duplicate {
            return Err(ApiError::conflict(
                "that response is already pinned in the destination tab",
            ));
        }
        enforce_response_board_tab_capacity(&transaction, &next_tab_id)?;
    }
    if next_placement == ResponseBoardCardPlacement::Staged
        && current.placement != ResponseBoardCardPlacement::Staged
    {
        let duplicate_staged: bool = transaction
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM board_cards
                   WHERE placement = 'staged' AND source_session_id = ?1
                     AND source_message_id = ?2 AND id <> ?3
                 )",
                rusqlite::params![
                    current.source_session_id,
                    current.source_message_id,
                    card_id,
                ],
                |row| row.get(0),
            )
            .map_err(|err| {
                response_board_storage_error("check staged response-board duplicates", err)
            })?;
        if duplicate_staged {
            return Err(ApiError::conflict(
                "that response is already waiting in staging",
            ));
        }
        enforce_response_board_staging_capacity(&transaction)?;
    }
    if next_placement == ResponseBoardCardPlacement::Placed
        && !current.has_canvas_position
        && (request.x.is_none() || request.y.is_none())
    {
        return Err(ApiError::bad_request(
            "x and y are required when placing a response without a saved canvas position",
        ));
    }
    let next_has_canvas_position = current.has_canvas_position
        || request.x.is_some()
        || request.y.is_some()
        || next_placement == ResponseBoardCardPlacement::Placed;
    let next_x = request.x.unwrap_or(current.x);
    let next_y = request.y.unwrap_or(current.y);
    let next_w = request.w.unwrap_or(current.w);
    let next_h = request.h.unwrap_or(current.h);
    transaction
        .execute(
            "UPDATE board_cards
             SET tab_id = ?2, placement = ?3, has_canvas_position = ?4,
                 x = ?5, y = ?6, w = ?7, h = ?8
             WHERE id = ?1",
            rusqlite::params![
                card_id,
                next_tab_id,
                next_placement.as_db_str(),
                next_has_canvas_position,
                next_x,
                next_y,
                next_w,
                next_h,
            ],
        )
        .map_err(|err| response_board_storage_error("update the response-board card", err))?;
    let card = transaction
        .query_row(&sql, rusqlite::params![card_id], decode_response_board_card_row)
        .map_err(|err| response_board_storage_error("read the updated response-board card", err))?;
    transaction
        .commit()
        .map_err(|err| response_board_storage_error("commit the response-board card", err))?;
    Ok(card)
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

fn list_response_board_tabs_from_storage(
    path: &FsPath,
) -> Result<ResponseBoardTabs, ApiError> {
    let connection = open_sqlite_state_read_connection(path)
        .map_err(|err| response_board_storage_error("open response-board tabs", err))?;
    let sql = format!(
        "{} GROUP BY t.id ORDER BY t.sort_order ASC, t.created_at ASC, t.id ASC",
        response_board_tab_select_sql()
    );
    let mut statement = connection
        .prepare(&sql)
        .map_err(|err| response_board_storage_error("prepare response-board tabs", err))?;
    let tabs = statement
        .query_map([], decode_response_board_tab_row)
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .map_err(|err| response_board_storage_error("read response-board tabs", err))?;
    let staged_card_count = connection
        .query_row(
            "SELECT COUNT(*) FROM board_cards WHERE placement = 'staged'",
            [],
            |row| row.get(0),
        )
        .map_err(|err| response_board_storage_error("count staged responses", err))?;
    Ok(ResponseBoardTabs {
        tabs,
        staged_card_count,
    })
}

fn query_response_board_tab(
    connection: &rusqlite::Connection,
    tab_id: &str,
) -> Result<ResponseBoardTab, ApiError> {
    let sql = format!(
        "{} WHERE t.id = ?1 GROUP BY t.id",
        response_board_tab_select_sql()
    );
    connection
        .query_row(
            &sql,
            rusqlite::params![tab_id],
            decode_response_board_tab_row,
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => {
                ApiError::not_found("response-board tab not found")
            }
            other => response_board_storage_error("read the response-board tab", other),
        })
}

fn load_response_board_tab(
    path: &FsPath,
    tab_id: &str,
) -> Result<ResponseBoardTabView, ApiError> {
    validate_response_board_identifier("tab id", tab_id)?;
    let connection = open_sqlite_state_read_connection(path)
        .map_err(|err| response_board_storage_error("open the response-board tab", err))?;
    let tab = query_response_board_tab(&connection, tab_id)?;
    let placed_sql = format!(
        "SELECT {} FROM board_cards WHERE tab_id = ?1 AND placement = 'placed'
         ORDER BY created_at ASC, id ASC",
        response_board_select_columns()
    );
    let mut statement = connection
        .prepare(&placed_sql)
        .map_err(|err| response_board_storage_error("prepare response-board cards", err))?;
    let cards = statement
        .query_map(
            rusqlite::params![tab_id],
            decode_response_board_card_row,
        )
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .map_err(|err| response_board_storage_error("read response-board cards", err))?;
    let staged_sql = format!(
        "SELECT {} FROM board_cards WHERE placement = 'staged'
         ORDER BY created_at ASC, id ASC",
        response_board_select_columns()
    );
    let mut staged_statement = connection
        .prepare(&staged_sql)
        .map_err(|err| response_board_storage_error("prepare staged responses", err))?;
    let staged_cards = staged_statement
        .query_map([], decode_response_board_card_row)
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .map_err(|err| response_board_storage_error("read staged responses", err))?;
    Ok(ResponseBoardTabView {
        tab,
        cards,
        staged_cards,
    })
}

fn insert_response_board_tab(
    path: &FsPath,
    request: CreateResponseBoardTabRequest,
) -> Result<ResponseBoardTab, ApiError> {
    let name = validate_response_board_tab_name(&request.name)?;
    let connection = open_sqlite_state_connection(path)
        .map_err(|err| response_board_storage_error("open response-board tabs", err))?;
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    let custom_tab_count: usize = connection
        .query_row(
            "SELECT COUNT(*) FROM response_board_tabs WHERE kind = ?1 AND id <> ?2",
            rusqlite::params![
                ResponseBoardTabKind::Custom.as_db_str(),
                RESPONSE_BOARD_DEFAULT_TAB_ID,
            ],
            |row| row.get(0),
        )
        .map_err(|err| response_board_storage_error("count response-board tabs", err))?;
    if custom_tab_count >= RESPONSE_BOARD_MAX_CUSTOM_TABS {
        return Err(ApiError::conflict(format!(
            "response board supports at most {RESPONSE_BOARD_MAX_CUSTOM_TABS} custom tabs"
        )));
    }
    let next_sort_order: i64 = connection
        .query_row(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM response_board_tabs",
            [],
            |row| row.get(0),
        )
        .map_err(|err| response_board_storage_error("order the response-board tab", err))?;
    let id = Uuid::new_v4().to_string();
    connection
        .execute(
            "INSERT INTO response_board_tabs(
               id, name, kind, project_id, sort_order, created_at
             ) VALUES (?1, ?2, ?3, NULL, ?4, ?5)",
            rusqlite::params![
                id,
                name,
                ResponseBoardTabKind::Custom.as_db_str(),
                next_sort_order,
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            ],
        )
        .map_err(|err| response_board_storage_error("create the response-board tab", err))?;
    query_response_board_tab(&connection, &id)
}

fn rename_response_board_tab(
    path: &FsPath,
    tab_id: &str,
    request: UpdateResponseBoardTabRequest,
) -> Result<ResponseBoardTab, ApiError> {
    validate_response_board_identifier("tab id", tab_id)?;
    let connection = open_sqlite_state_connection(path)
        .map_err(|err| response_board_storage_error("open the response-board tab", err))?;
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    let tab = query_response_board_tab(&connection, tab_id)?;
    if tab.kind == ResponseBoardTabKind::ProjectDefault {
        return Err(ApiError::conflict(
            "project response-board tabs cannot be renamed",
        ));
    }
    let name = validate_response_board_tab_name(&request.name)?;
    let changed = connection
        .execute(
            "UPDATE response_board_tabs SET name = ?2 WHERE id = ?1",
            rusqlite::params![tab_id, name],
        )
        .map_err(|err| response_board_storage_error("rename the response-board tab", err))?;
    if changed == 0 {
        return Err(ApiError::not_found("response-board tab not found"));
    }
    query_response_board_tab(&connection, tab_id)
}

fn remove_response_board_tab(path: &FsPath, tab_id: &str) -> Result<(), ApiError> {
    validate_response_board_identifier("tab id", tab_id)?;
    if tab_id == RESPONSE_BOARD_DEFAULT_TAB_ID {
        return Err(ApiError::conflict("the default response-board tab cannot be deleted"));
    }
    let connection = open_sqlite_state_connection(path)
        .map_err(|err| response_board_storage_error("open the response-board tab", err))?;
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    let tab = query_response_board_tab(&connection, tab_id)?;
    if tab.kind == ResponseBoardTabKind::ProjectDefault {
        return Err(ApiError::conflict("project response-board tabs cannot be deleted"));
    }
    if tab.placed_card_count > 0 {
        return Err(ApiError::conflict("move or delete this tab's cards first"));
    }
    let transaction = connection
        .unchecked_transaction()
        .map_err(|err| response_board_storage_error("start response-board tab removal", err))?;
    transaction
        .execute(
            "UPDATE board_cards SET tab_id = ?2
             WHERE tab_id = ?1 AND placement = 'staged'",
            rusqlite::params![tab_id, RESPONSE_BOARD_DEFAULT_TAB_ID],
        )
        .map_err(|err| response_board_storage_error("release staged card tab hints", err))?;
    transaction
        .execute(
            "DELETE FROM response_board_tabs WHERE id = ?1",
            rusqlite::params![tab_id],
        )
        .map_err(|err| response_board_storage_error("delete the response-board tab", err))?;
    transaction
        .commit()
        .map_err(|err| response_board_storage_error("commit response-board tab removal", err))?;
    Ok(())
}

fn reorder_response_board_tabs_in_storage(
    path: &FsPath,
    request: ReorderResponseBoardTabsRequest,
) -> Result<ResponseBoardTabs, ApiError> {
    let connection = open_sqlite_state_connection(path)
        .map_err(|err| response_board_storage_error("open response-board tabs", err))?;
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    let transaction = connection
        .unchecked_transaction()
        .map_err(|err| response_board_storage_error("start response-board tab reorder", err))?;
    let existing_ids = transaction
        .prepare("SELECT id FROM response_board_tabs ORDER BY sort_order, created_at, id")
        .and_then(|mut statement| {
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|err| response_board_storage_error("read response-board tab order", err))?;
    let requested_ids = request.tab_ids;
    let unique_ids = requested_ids.iter().collect::<std::collections::HashSet<_>>();
    if requested_ids.len() != existing_ids.len()
        || unique_ids.len() != requested_ids.len()
        || !existing_ids.iter().all(|id| unique_ids.contains(id))
    {
        return Err(ApiError::bad_request(
            "tabIds must contain every response-board tab exactly once",
        ));
    }
    for (sort_order, tab_id) in requested_ids.iter().enumerate() {
        transaction
            .execute(
                "UPDATE response_board_tabs SET sort_order = ?2 WHERE id = ?1",
                rusqlite::params![tab_id, sort_order as i64],
            )
            .map_err(|err| response_board_storage_error("reorder response-board tabs", err))?;
    }
    transaction
        .commit()
        .map_err(|err| response_board_storage_error("commit response-board tab order", err))?;
    list_response_board_tabs_from_storage(path)
}

fn ensure_project_response_board_tab(
    transaction: &rusqlite::Transaction<'_>,
    project_id: &str,
    project_name: &str,
) -> Result<String, ApiError> {
    let existing = transaction
        .query_row(
            "SELECT id FROM response_board_tabs WHERE project_id = ?1",
            rusqlite::params![project_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|err| response_board_storage_error("find the project board tab", err))?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let sort_order: i64 = transaction
        .query_row(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM response_board_tabs",
            [],
            |row| row.get(0),
        )
        .map_err(|err| response_board_storage_error("order the project board tab", err))?;
    let id = Uuid::new_v4().to_string();
    // Project tabs mirror names already accepted by project persistence. The
    // shorter custom-tab limit applies only to names entered for board tabs.
    transaction
        .execute(
            "INSERT INTO response_board_tabs(
               id, name, kind, project_id, sort_order, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                id,
                project_name,
                ResponseBoardTabKind::ProjectDefault.as_db_str(),
                project_id,
                sort_order,
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            ],
        )
        .map_err(|err| response_board_storage_error("create the project board tab", err))?;
    Ok(id)
}

fn stage_response_board_card_in_storage(
    path: &FsPath,
    request: StageResponseBoardCardRequest,
    project: Option<(String, String)>,
) -> Result<(StatusCode, ResponseBoardCard), ApiError> {
    validate_response_board_identifier("sessionId", &request.session_id)?;
    validate_response_board_identifier("messageId", &request.message_id)?;
    if request.tab_id.is_some() && request.project_id.is_some() {
        return Err(ApiError::bad_request(
            "tabId and projectId cannot be supplied together",
        ));
    }
    let connection = open_sqlite_state_connection(path)
        .map_err(|err| response_board_storage_error("open the response board", err))?;
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    let transaction = connection
        .unchecked_transaction()
        .map_err(|err| response_board_storage_error("start a response board update", err))?;
    let sql = format!(
        "SELECT {} FROM board_cards
         WHERE source_session_id = ?1 AND source_message_id = ?2
         ORDER BY CASE placement WHEN 'staged' THEN 0 ELSE 1 END,
                  created_at ASC, id ASC
         LIMIT 1",
        response_board_select_columns()
    );
    let existing = transaction
        .query_row(
            &sql,
            rusqlite::params![&request.session_id, &request.message_id],
            decode_response_board_card_row,
        )
        .optional()
        .map_err(|err| response_board_storage_error("find the staged response", err))?;
    if let Some(card) = existing.as_ref()
        && card.placement == ResponseBoardCardPlacement::Staged
    {
        transaction
            .commit()
            .map_err(|err| response_board_storage_error("finish the staged response", err))?;
        return Ok((StatusCode::OK, card.clone()));
    }

    // A repeated Pin is a workflow action, not only an idempotent lookup:
    // already placed content returns to the shared staging inbox. Resolve the
    // destination only after the lookup so an already staged card cannot
    // create an unused project-default tab as a side effect.
    let tab_id = if let Some((project_id, project_name)) = project {
        ensure_project_response_board_tab(&transaction, &project_id, &project_name)?
    } else {
        request
            .tab_id
            .unwrap_or_else(|| RESPONSE_BOARD_DEFAULT_TAB_ID.to_owned())
    };
    validate_response_board_identifier("tabId", &tab_id)?;
    if !response_board_tab_exists(&transaction, &tab_id)? {
        return Err(ApiError::not_found("response-board tab not found"));
    }
    enforce_response_board_staging_capacity(&transaction)?;
    if let Some(mut card) = existing {
        transaction
            .execute(
                "UPDATE board_cards
                 SET tab_id = ?2, placement = 'staged'
                 WHERE id = ?1",
                rusqlite::params![&card.id, &tab_id],
            )
            .map_err(|err| response_board_storage_error("restage the response", err))?;
        card.tab_id = tab_id;
        card.placement = ResponseBoardCardPlacement::Staged;
        transaction
            .commit()
            .map_err(|err| response_board_storage_error("finish the staged response", err))?;
        return Ok((StatusCode::OK, card));
    }

    let snapshot = prepare_response_board_snapshot(
        &transaction,
        &request.session_id,
        &request.message_id,
    )?;
    let card = ResponseBoardCard {
        id: Uuid::new_v4().to_string(),
        tab_id,
        placement: ResponseBoardCardPlacement::Staged,
        has_canvas_position: false,
        x: 0.0,
        y: 0.0,
        w: RESPONSE_BOARD_DEFAULT_WIDTH,
        h: RESPONSE_BOARD_DEFAULT_HEIGHT,
        snapshot: snapshot.message,
        source_session_id: request.session_id,
        source_message_id: request.message_id,
        source_message_position: snapshot.source_message_position,
        source_session_name: snapshot.source_session_name,
        source_agent: snapshot.source_agent,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    };
    transaction
        .execute(
            "INSERT INTO board_cards(
               id, tab_id, placement, has_canvas_position, x, y, w, h,
               snapshot_json, source_session_id, source_message_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                card.id,
                card.tab_id,
                card.placement.as_db_str(),
                card.has_canvas_position,
                card.x,
                card.y,
                card.w,
                card.h,
                snapshot.snapshot_json,
                card.source_session_id,
                card.source_message_id,
                card.created_at,
            ],
        )
        .map_err(|err| response_board_storage_error("stage the response-board card", err))?;
    transaction
        .commit()
        .map_err(|err| response_board_storage_error("commit the staged response", err))?;
    Ok((StatusCode::CREATED, card))
}

fn convert_deleted_project_response_board_tab(
    path: &FsPath,
    project_id: &str,
    last_project_name: &str,
) -> Result<(), ApiError> {
    let connection = open_sqlite_state_connection(path)
        .map_err(|err| response_board_storage_error("open the response board", err))?;
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    let schema_exists: bool = connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sqlite_master
               WHERE type = 'table' AND name = 'response_board_tabs'
             )",
            [],
            |row| row.get(0),
        )
        .map_err(|err| response_board_storage_error("inspect the response board schema", err))?;
    if !schema_exists {
        return Ok(());
    }
    let transaction = connection
        .unchecked_transaction()
        .map_err(|err| response_board_storage_error("start a response board update", err))?;
    // Preserve the tab, its cards, order, and last project name. Once the
    // owning project is gone it becomes an ordinary user-managed tab rather
    // than an undeletable project-default orphan.
    transaction
        .execute(
            "UPDATE response_board_tabs
             SET name = ?2, kind = ?3, project_id = NULL
             WHERE kind = ?4 AND project_id = ?1",
            rusqlite::params![
                project_id,
                last_project_name,
                ResponseBoardTabKind::Custom.as_db_str(),
                ResponseBoardTabKind::ProjectDefault.as_db_str(),
            ],
        )
        .map_err(|err| response_board_storage_error("detach the project board tab", err))?;
    transaction
        .commit()
        .map_err(|err| response_board_storage_error("finish the response board update", err))?;
    Ok(())
}

fn response_board_project_names(
    state: &AppState,
) -> Result<HashMap<String, String>, ApiError> {
    let inner = state
        .inner
        .lock()
        .map_err(|_| ApiError::internal("state mutex poisoned"))?;
    Ok(inner
        .projects
        .iter()
        .map(|project| (project.id.clone(), project.name.clone()))
        .collect())
}

fn apply_live_project_tab_names(
    tabs: &mut [ResponseBoardTab],
    project_names: &HashMap<String, String>,
) {
    for tab in tabs {
        if tab.kind != ResponseBoardTabKind::ProjectDefault {
            continue;
        }
        let Some(project_id) = tab.project_id.as_deref() else {
            continue;
        };
        if let Some(project_name) = project_names.get(project_id) {
            tab.name.clone_from(project_name);
        }
    }
}

async fn list_response_board_tabs(
    State(state): State<AppState>,
) -> Result<Json<ResponseBoardTabs>, ApiError> {
    let project_names = response_board_project_names(&state)?;
    let path = state.persistence_path.as_ref().clone();
    let mut tabs = run_blocking_api(move || list_response_board_tabs_from_storage(&path)).await?;
    apply_live_project_tab_names(&mut tabs.tabs, &project_names);
    Ok(Json(tabs))
}

async fn get_response_board_tab(
    AxumPath(tab_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<ResponseBoardTabView>, ApiError> {
    let project_names = response_board_project_names(&state)?;
    let path = state.persistence_path.as_ref().clone();
    let mut view = run_blocking_api(move || load_response_board_tab(&path, &tab_id)).await?;
    apply_live_project_tab_names(std::slice::from_mut(&mut view.tab), &project_names);
    Ok(Json(view))
}

async fn create_response_board_tab(
    State(state): State<AppState>,
    request: Result<Json<CreateResponseBoardTabRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ResponseBoardTab>), ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("response-board tab", rejection))?;
    let path = state.persistence_path.as_ref().clone();
    let tab = run_blocking_api(move || insert_response_board_tab(&path, request)).await?;
    Ok((StatusCode::CREATED, Json(tab)))
}

async fn update_response_board_tab(
    AxumPath(tab_id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<UpdateResponseBoardTabRequest>, JsonRejection>,
) -> Result<Json<ResponseBoardTab>, ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("response-board tab update", rejection))?;
    let path = state.persistence_path.as_ref().clone();
    let tab = run_blocking_api(move || rename_response_board_tab(&path, &tab_id, request)).await?;
    Ok(Json(tab))
}

async fn delete_response_board_tab(
    AxumPath(tab_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    let path = state.persistence_path.as_ref().clone();
    run_blocking_api(move || remove_response_board_tab(&path, &tab_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn reorder_response_board_tabs(
    State(state): State<AppState>,
    request: Result<Json<ReorderResponseBoardTabsRequest>, JsonRejection>,
) -> Result<Json<ResponseBoardTabs>, ApiError> {
    let Json(request) = request
        .map_err(|rejection| api_json_rejection("response-board tab order", rejection))?;
    let project_names = response_board_project_names(&state)?;
    let path = state.persistence_path.as_ref().clone();
    let mut tabs =
        run_blocking_api(move || reorder_response_board_tabs_in_storage(&path, request)).await?;
    apply_live_project_tab_names(&mut tabs.tabs, &project_names);
    Ok(Json(tabs))
}

async fn stage_response_board_card(
    State(state): State<AppState>,
    request: Result<Json<StageResponseBoardCardRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ResponseBoardCard>), ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("staged response-board card", rejection))?;
    let project = if let Some(project_id) = request.project_id.as_deref() {
        validate_response_board_identifier("projectId", project_id)?;
        let inner = state
            .inner
            .lock()
            .map_err(|_| ApiError::internal("state mutex poisoned"))?;
        let session_index = inner
            .find_session_index(&request.session_id)
            .ok_or_else(|| ApiError::not_found("source session not found"))?;
        if inner.sessions[session_index].session.project_id.as_deref() != Some(project_id) {
            return Err(ApiError::bad_request(
                "projectId does not match the source session",
            ));
        }
        let project = inner
            .find_project(project_id)
            .ok_or_else(|| ApiError::not_found("project not found"))?;
        Some((project.id.clone(), project.name.clone()))
    } else {
        None
    };
    let path = state.persistence_path.as_ref().clone();
    let (status, card) = run_blocking_api(move || {
        stage_response_board_card_in_storage(&path, request, project)
    })
    .await?;
    Ok((status, Json(card)))
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
