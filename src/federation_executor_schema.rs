// Version 4 adds inert executor ownership and evidence. No mailbox replica,
// cursor or network dispatch belongs here. Historical schemas stay frozen.
const FEDERATION_EXECUTOR_SCHEMA_SQL: &str = "
    CREATE TABLE federation_executor_operations (
      central_host_id TEXT NOT NULL CHECK (length(central_host_id) BETWEEN 1 AND 256),
      operation_key TEXT NOT NULL CHECK (length(operation_key) BETWEEN 1 AND 1024),
      central_epoch INTEGER NOT NULL CHECK (central_epoch > 0),
      target_host_id TEXT NOT NULL CHECK (length(target_host_id) BETWEEN 1 AND 256),
      enrollment_generation INTEGER NOT NULL CHECK (enrollment_generation > 0),
      session_id TEXT NOT NULL CHECK (length(session_id) BETWEEN 1 AND 256),
      creation_id TEXT NOT NULL CHECK (length(creation_id) BETWEEN 1 AND 256),
      placement_generation INTEGER NOT NULL CHECK (placement_generation > 0),
      command_kind TEXT NOT NULL CHECK (command_kind IN ('wake','start','stop')),
      payload_digest TEXT NOT NULL CHECK (
        length(payload_digest) = 64 AND payload_digest NOT GLOB '*[^0-9a-f]*'),
      payload TEXT NOT NULL DEFAULT '',
      payload_bytes INTEGER NOT NULL DEFAULT 0 CHECK (
        payload_bytes >= 0 AND payload_bytes <= 262144
        AND payload_bytes = length(CAST(payload AS BLOB))),
      stage TEXT NOT NULL CHECK (stage IN ('received','queued','runtime_accepted','processed')),
      effect_state TEXT NOT NULL CHECK (effect_state IN ('not_started','unknown','confirmed')),
      queue_entry_id TEXT CHECK (queue_entry_id IS NULL OR length(queue_entry_id) BETWEEN 1 AND 4096),
      delivery_attempt_id TEXT CHECK (delivery_attempt_id IS NULL OR length(delivery_attempt_id) BETWEEN 1 AND 4096),
      runtime_ack TEXT CHECK (runtime_ack IS NULL OR length(runtime_ack) BETWEEN 1 AND 4096),
      processed_evidence_kind TEXT CHECK (processed_evidence_kind IN ('mailbox_ack','command_outcome')),
      processed_evidence TEXT CHECK (processed_evidence IS NULL OR length(processed_evidence) BETWEEN 1 AND 4096),
      PRIMARY KEY (central_host_id, operation_key),
      CHECK ((queue_entry_id IS NULL) = (delivery_attempt_id IS NULL)),
      CHECK (stage <> 'queued' OR queue_entry_id IS NOT NULL),
      CHECK (stage <> 'runtime_accepted' OR runtime_ack IS NOT NULL),
      CHECK (runtime_ack IS NULL OR queue_entry_id IS NOT NULL),
      CHECK ((processed_evidence_kind IS NULL) = (processed_evidence IS NULL)),
      CHECK (stage <> 'processed' OR processed_evidence IS NOT NULL),
      CHECK (processed_evidence_kind IS NULL
        OR (command_kind = 'wake' AND processed_evidence_kind = 'mailbox_ack')
        OR (command_kind IN ('start','stop') AND processed_evidence_kind = 'command_outcome'))
    );
    CREATE TRIGGER federation_executor_identity_immutable
    BEFORE UPDATE OF central_host_id, operation_key, central_epoch, target_host_id,
      enrollment_generation, session_id, creation_id, placement_generation,
      command_kind, payload_digest, payload, payload_bytes
    ON federation_executor_operations
    WHEN OLD.central_host_id IS NOT NEW.central_host_id
      OR OLD.operation_key IS NOT NEW.operation_key
      OR OLD.central_epoch IS NOT NEW.central_epoch
      OR OLD.target_host_id IS NOT NEW.target_host_id
      OR OLD.enrollment_generation IS NOT NEW.enrollment_generation
      OR OLD.session_id IS NOT NEW.session_id
      OR OLD.creation_id IS NOT NEW.creation_id
      OR OLD.placement_generation IS NOT NEW.placement_generation
      OR OLD.command_kind IS NOT NEW.command_kind
      OR OLD.payload_digest IS NOT NEW.payload_digest
      OR OLD.payload IS NOT NEW.payload
      OR OLD.payload_bytes IS NOT NEW.payload_bytes
    BEGIN SELECT RAISE(ABORT, 'executor operation identity is immutable'); END;
";
