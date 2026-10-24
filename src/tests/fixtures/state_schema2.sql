
    CREATE TABLE meta (
      key TEXT PRIMARY KEY,
      value TEXT NOT NULL
    );

    CREATE TABLE sessions (
      id TEXT PRIMARY KEY,
      value_json TEXT NOT NULL
    );

    CREATE TABLE messages (
      session_id TEXT NOT NULL,
      position INTEGER NOT NULL CHECK(position >= 0),
      message_id TEXT NOT NULL,
      value_json TEXT NOT NULL,
      overview_kind INTEGER NOT NULL DEFAULT 0,
      is_user INTEGER NOT NULL DEFAULT 0,
      PRIMARY KEY(session_id, position),
      UNIQUE(session_id, message_id),
      FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
    );

    CREATE TABLE session_overviews (
      session_id TEXT PRIMARY KEY,
      value_blob BLOB NOT NULL,
      FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
    ) WITHOUT ROWID;


    CREATE TABLE app_state (
      key TEXT PRIMARY KEY,
      value_json TEXT NOT NULL
    );

    CREATE TABLE session_prompt_histories (
      session_id TEXT PRIMARY KEY,
      value_json TEXT NOT NULL,
      FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
    ) WITHOUT ROWID;

    CREATE TABLE delegations (
      id TEXT PRIMARY KEY,
      value_json TEXT NOT NULL
    );

    CREATE TABLE response_board_tabs (
      id TEXT PRIMARY KEY,
      name TEXT NOT NULL,
      kind TEXT NOT NULL,
      project_id TEXT UNIQUE,
      sort_order INTEGER NOT NULL,
      created_at TEXT NOT NULL
    );

    CREATE TABLE board_cards (
      id TEXT PRIMARY KEY,
      x REAL NOT NULL,
      y REAL NOT NULL,
      w REAL NOT NULL,
      h REAL NOT NULL,
      snapshot_json TEXT NOT NULL,
      source_session_id TEXT NOT NULL,
      source_message_id TEXT NOT NULL,
      created_at TEXT NOT NULL,
      tab_id TEXT NOT NULL DEFAULT 'response-board-default',
      placement TEXT NOT NULL DEFAULT 'placed',
      has_canvas_position INTEGER NOT NULL DEFAULT 1
    );
