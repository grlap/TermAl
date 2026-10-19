// Owns the version-3 coordination schema extension for delivery intents.
// Does not migrate or reinterpret historical mailbox messages and cursors.
// Canonical SQL is immutable once released; later changes need a migration.
const FEDERATION_MAILBOX_DELIVERY_SCHEMA_SQL: &str = "
    CREATE TABLE federation_mailbox_intents (
      sender_session_id TEXT NOT NULL,
      idempotency_key TEXT NOT NULL,
      sender_host_id TEXT NOT NULL,
      owner_session_id TEXT NOT NULL,
      owner_creation_id TEXT NOT NULL,
      target_session_id TEXT NOT NULL,
      canonical_digest TEXT NOT NULL,
      envelope_json TEXT NOT NULL,
      state TEXT NOT NULL CHECK (state IN ('localIntentDurable','appendOutcomeUnknown','homeCommitted','refused','abandoned')),
      payload TEXT,
      payload_bytes INTEGER NOT NULL CHECK (payload_bytes >= 0),
      message_id TEXT,
      sequence INTEGER,
      receipt_json TEXT,
      PRIMARY KEY (sender_session_id, idempotency_key)
    );
    CREATE INDEX federation_mailbox_intents_owner
      ON federation_mailbox_intents(owner_session_id, owner_creation_id);
    CREATE TABLE federation_mailbox_dedup (
      sender_host_id TEXT NOT NULL,
      sender_session_id TEXT NOT NULL,
      idempotency_key TEXT NOT NULL,
      target_session_id TEXT NOT NULL,
      canonical_digest TEXT NOT NULL,
      state TEXT NOT NULL CHECK (state IN ('homeCommitted','refused','abandoned')),
      message_id TEXT,
      sequence INTEGER,
      PRIMARY KEY (sender_host_id, sender_session_id, idempotency_key)
    );
    CREATE TABLE federation_mailbox_wake_obligations (
      message_id TEXT PRIMARY KEY,
      target_session_id TEXT NOT NULL,
      target_binding_json TEXT NOT NULL,
      state TEXT NOT NULL CHECK (state IN ('pending','settled')),
      FOREIGN KEY (message_id) REFERENCES mailbox_messages(id)
    );
    CREATE TABLE federation_mailbox_retired_namespaces (
      sender_session_id TEXT PRIMARY KEY,
      retired_at TEXT NOT NULL
    );
";
