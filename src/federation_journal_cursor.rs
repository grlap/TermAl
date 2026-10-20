// Bounded, stateless locators for operator journal pages. A cursor never grants
// authority or retains a database snapshot between requests.
const FEDERATION_JOURNAL_PAGE_LIMIT: usize = 100;
const FEDERATION_JOURNAL_CURSOR_LIMIT: usize = 2048;

type FederationJournalResult<T> = std::result::Result<T, FederationJournalError>;

enum FederationJournalError {
    Invalid,
    Restart,
    NonCentral(FederationRole),
    Unavailable,
    Storage,
}

impl axum::response::IntoResponse for FederationJournalError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::Invalid => (
                StatusCode::BAD_REQUEST,
                json!({
                "code":"journal_invalid_query", "error":"invalid journal query or cursor"}),
            ),
            Self::Restart => (
                StatusCode::CONFLICT,
                json!({
                "code":"journal_restart_required", "restart":true,
                "error":"journal page basis changed; restart pagination"}),
            ),
            Self::NonCentral(role) => (
                StatusCode::CONFLICT,
                json!({
                "code":"journal_role_not_central", "role":role,
                "error":"wake journal is unavailable for this local role"}),
            ),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({
                "code":"journal_identity_unavailable",
                "error":"durable local journal identity is unavailable"}),
            ),
            Self::Storage => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({
                "code":"journal_read_failed", "error":"journal storage read failed"}),
            ),
        };
        (status, Json(body)).into_response()
    }
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FederationJournalStream {
    Operations,
    Wakes,
}

impl FederationJournalStream {
    fn table(self) -> &'static str {
        match self {
            Self::Operations => "federation_executor_operations",
            Self::Wakes => "federation_mailbox_wake_obligations",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FederationJournalQuery {
    stream: Option<FederationJournalStream>,
    limit: Option<usize>,
    cursor: Option<String>,
}

impl FederationJournalQuery {
    fn validate(&self) -> FederationJournalResult<(FederationJournalStream, usize)> {
        let limit = self.limit.unwrap_or(FEDERATION_JOURNAL_PAGE_LIMIT);
        if !(1..=FEDERATION_JOURNAL_PAGE_LIMIT).contains(&limit)
            || self.cursor.as_ref().is_some_and(|cursor| {
                cursor.is_empty() || cursor.len() > FEDERATION_JOURNAL_CURSOR_LIMIT
            })
        {
            return Err(FederationJournalError::Invalid);
        }
        Ok((
            self.stream.unwrap_or(FederationJournalStream::Operations),
            limit,
        ))
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FederationJournalAnchor {
    rowid: i64,
    identity: String,
}

impl FederationJournalAnchor {
    fn valid(&self) -> bool {
        self.rowid > 0
            && self.identity.len() == 64
            && self
                .identity
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FederationJournalCursor {
    version: u8,
    stream: FederationJournalStream,
    basis: String,
    high: FederationJournalAnchor,
    previous: FederationJournalAnchor,
}

impl FederationJournalCursor {
    fn decode(
        text: &str,
        stream: FederationJournalStream,
        basis: &str,
    ) -> FederationJournalResult<Self> {
        if text.len() > FEDERATION_JOURNAL_CURSOR_LIMIT {
            return Err(FederationJournalError::Invalid);
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| FederationJournalError::Invalid)?;
        let cursor: Self =
            serde_json::from_slice(&bytes).map_err(|_| FederationJournalError::Invalid)?;
        if cursor.version != 1
            || cursor.stream != stream
            || !cursor.high.valid()
            || !cursor.previous.valid()
            || cursor.previous.rowid > cursor.high.rowid
        {
            return Err(FederationJournalError::Invalid);
        }
        if cursor.basis != basis {
            return Err(FederationJournalError::Restart);
        }
        Ok(cursor)
    }

    fn encode(&self) -> FederationJournalResult<String> {
        let bytes = serde_json::to_vec(self).map_err(|_| FederationJournalError::Storage)?;
        let text = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        if text.len() > FEDERATION_JOURNAL_CURSOR_LIMIT {
            return Err(FederationJournalError::Storage);
        }
        Ok(text)
    }
}

fn federation_journal_identity(parts: &[&str]) -> String {
    sha256_hex(&serde_json::to_vec(parts).expect("journal identity strings serialize"))
}

fn federation_journal_anchor(
    connection: &rusqlite::Connection,
    stream: FederationJournalStream,
    rowid: Option<i64>,
) -> FederationJournalResult<Option<FederationJournalAnchor>> {
    // All SQL identifiers come from the closed stream enum, never query text.
    let identity = match stream {
        FederationJournalStream::Operations => "central_host_id,operation_key",
        FederationJournalStream::Wakes => "message_id",
    };
    let predicate = if rowid.is_some() {
        "rowid=?1"
    } else {
        "?1 IS NULL"
    };
    let sql = format!(
        "SELECT rowid,{identity} FROM {} WHERE {predicate} \
        ORDER BY rowid DESC LIMIT 1",
        stream.table()
    );
    connection
        .query_row(&sql, [rowid], |row| {
            let first: String = row.get(1)?;
            let hash = match stream {
                FederationJournalStream::Operations => {
                    let second: String = row.get(2)?;
                    federation_journal_identity(&[&first, &second])
                }
                FederationJournalStream::Wakes => federation_journal_identity(&[&first]),
            };
            Ok(FederationJournalAnchor {
                rowid: row.get(0)?,
                identity: hash,
            })
        })
        .optional()
        .map_err(|_| FederationJournalError::Storage)
}

fn federation_journal_validate_anchor(
    connection: &rusqlite::Connection,
    stream: FederationJournalStream,
    anchor: &FederationJournalAnchor,
) -> FederationJournalResult<()> {
    let current = federation_journal_anchor(connection, stream, Some(anchor.rowid))?;
    if current
        .as_ref()
        .is_none_or(|current| current.identity != anchor.identity)
    {
        return Err(FederationJournalError::Restart);
    }
    Ok(())
}
