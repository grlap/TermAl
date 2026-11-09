// Coordination direct-prompt floor: separate durable records, not executor wake
// records and not a runnable queue. Only the primary session queue can run a
// prompt. The storage adapter validates the complete typed JSON before use.
// Historical coordination definitions must not change when this schema grows.
// The step-7 predecessor comes from the landed review-custody schema owner;
// do not maintain a second speculative definition here.
// Recovered from the retained direct-prompt storage component.

fn remote_prompt_storage_schema_sql() -> String {
    let mut sql = String::new();
    for table in ["remote_prompt_intents", "remote_prompt_receives"] {
        sql.push_str(&format!(
            "CREATE TABLE {table} (
              source_host_id TEXT NOT NULL CHECK (length(source_host_id) = 36),
              source_epoch INTEGER NOT NULL CHECK (source_epoch > 0),
              source_session_id TEXT NOT NULL CHECK (length(source_session_id) BETWEEN 1 AND 160),
              source_creation_id TEXT NOT NULL CHECK (length(source_creation_id) = 36),
              idempotency_key TEXT NOT NULL CHECK (length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 160),
              operation_id TEXT NOT NULL UNIQUE CHECK (length(operation_id) = 36),
              intent_json TEXT NOT NULL CHECK (json_valid(intent_json)),
              intent_digest TEXT NOT NULL CHECK (
                length(intent_digest) = 64 AND intent_digest NOT GLOB '*[^0-9a-f]*'),
              evidence_json TEXT NOT NULL CHECK (json_valid(evidence_json)),
              retained_bytes INTEGER NOT NULL CHECK (
                retained_bytes = length(CAST(source_host_id AS BLOB)) + 8
                  + length(CAST(source_session_id AS BLOB))
                  + length(CAST(source_creation_id AS BLOB))
                  + length(CAST(idempotency_key AS BLOB))
                  + length(CAST(operation_id AS BLOB))
                  + length(CAST(intent_json AS BLOB))
                  + length(CAST(intent_digest AS BLOB))
                  + length(CAST(evidence_json AS BLOB))),
              PRIMARY KEY (source_host_id, source_epoch, source_session_id,
                source_creation_id, idempotency_key)
            );
            CREATE TRIGGER {table}_identity_immutable
            BEFORE UPDATE OF source_host_id, source_epoch, source_session_id,
              source_creation_id, idempotency_key, operation_id, intent_json, intent_digest
            ON {table}
            WHEN OLD.source_host_id IS NOT NEW.source_host_id
              OR OLD.source_epoch IS NOT NEW.source_epoch
              OR OLD.source_session_id IS NOT NEW.source_session_id
              OR OLD.source_creation_id IS NOT NEW.source_creation_id
              OR OLD.idempotency_key IS NOT NEW.idempotency_key
              OR OLD.operation_id IS NOT NEW.operation_id
              OR OLD.intent_json IS NOT NEW.intent_json
              OR OLD.intent_digest IS NOT NEW.intent_digest
            BEGIN SELECT RAISE(ABORT, 'direct prompt identity is immutable'); END;"
        ));
    }
    sql
}
