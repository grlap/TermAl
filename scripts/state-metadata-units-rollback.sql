-- OFFLINE ONLY: stop TermAl first and keep a SQLite-consistent backup.
-- This materializes CURRENT normalized fields/counters for the primary-2 reader.
-- Do not merely change the schema marker or use the stale migration image.
.bail on
PRAGMA foreign_keys=ON;
BEGIN IMMEDIATE;
CREATE TEMP TABLE termal_metadata_rollback_guard(value INTEGER CHECK(value=1));
INSERT INTO termal_metadata_rollback_guard
SELECT CASE WHEN (SELECT value FROM meta WHERE key='schema_version')='2-metadata-units-2'
 AND EXISTS(SELECT 1 FROM state_metadata_units WHERE field_name='nextSessionNumber' AND item_key='')
 AND EXISTS(SELECT 1 FROM state_metadata_units WHERE field_name='nextMessageNumber' AND item_key='')
 AND EXISTS(SELECT 1 FROM state_metadata_units WHERE field_name='revision' AND item_key='')
 THEN 1 ELSE 0 END;
CREATE TEMP TABLE termal_metadata_expected_fields(name TEXT PRIMARY KEY,kind TEXT NOT NULL);
INSERT INTO termal_metadata_expected_fields VALUES
 ('federation','object'),('preferences','object'),('revision','value'),('nextSessionNumber','value'),('nextMessageNumber','value'),
 ('projects','array'),('engramRetiredWorkAuthorityGrants','array'),('engramWorkSourceRoots','array'),
 ('engramNamedRootJournal','array'),('engramWorkNamingHistory','array'),('engramNamedRootReleaseAudit','array'),
 ('engramSourceSightings','array'),('engramSourceRootGeneration','value'),('pendingCoordinationScopeDeletions','array'),
 ('pendingResponseBoardProjectDetachments','object'),('ignoredDiscoveredCodexThreadIds','array'),
 ('orchestratorInstances','array'),('delegationWaits','array'),('testRunCardsEpoch','value'),('testRunCards','object'),
 ('testRunRegistrations','object'),('testRunObservations','object'),('testRunCompacted','object'),('testRunWaits','array'),
 ('workspaceLayouts','object'),('sessions','array'),('codex','object');
INSERT INTO termal_metadata_rollback_guard
SELECT CASE WHEN NOT EXISTS (
 SELECT 1 FROM termal_metadata_expected_fields AS expected
 WHERE NOT EXISTS (SELECT 1 FROM state_metadata_units WHERE field_name=expected.name AND item_key='' AND kind=expected.kind)
) AND NOT EXISTS (
 SELECT 1 FROM state_metadata_units WHERE field_name NOT IN (SELECT name FROM termal_metadata_expected_fields)
) AND NOT EXISTS (
 SELECT 1 FROM state_metadata_units AS root WHERE item_key='' AND
 (kind NOT IN ('value','object','array') OR NOT json_valid(value_json) OR
 (kind='value' AND json_type(value_json) IN ('object','array')) OR
 (kind IN ('object','array') AND (json_type(value_json)<>'integer' OR CAST(value_json AS INTEGER)<0 OR
 CAST(value_json AS INTEGER)<>(SELECT count(*) FROM state_metadata_units AS entry
 WHERE entry.field_name=root.field_name AND entry.item_key<>''))))
) AND NOT EXISTS (
 SELECT 1 FROM state_metadata_units AS entry WHERE item_key<>'' AND
 (kind<>'entry' OR substr(item_key,1,1)<>'/' OR NOT json_valid(value_json) OR
 NOT EXISTS (SELECT 1 FROM state_metadata_units AS root WHERE root.field_name=entry.field_name
 AND root.item_key='' AND root.kind IN ('object','array')) OR
 ((SELECT kind FROM state_metadata_units AS root WHERE root.field_name=entry.field_name AND root.item_key='')='array'
 AND item_key<>printf('/%020d',(SELECT count(*) FROM state_metadata_units AS prior
 WHERE prior.field_name=entry.field_name AND prior.item_key<>'' AND prior.item_key<entry.item_key))))
) AND NOT EXISTS (
 SELECT 1 FROM state_metadata_units WHERE item_key='' AND
 field_name IN ('nextSessionNumber','nextMessageNumber','revision','engramSourceRootGeneration') AND
 (kind<>'value' OR json_type(value_json)<>'integer' OR value_json GLOB '*[^0-9]*' OR
 length(value_json)=0 OR length(value_json)>20 OR
 (length(value_json)=20 AND value_json>'18446744073709551615'))
) AND NOT EXISTS (
 SELECT 1 FROM state_metadata_units WHERE field_name='testRunCardsEpoch' AND item_key='' AND
 json_type(value_json) NOT IN ('null','text')
) THEN 1 ELSE 0 END;
-- SQLite JSON aggregation cannot preserve NUL object keys. Refuse before
-- touching persistent data; named constraints identify the affected field.
-- TEXT instr detects U+0000 without mistaking UTF-16 encoding bytes for NUL.
CREATE TEMP TABLE termal_metadata_rollback_nul_keys(field_name TEXT NOT NULL
 CONSTRAINT "rollback refuses NUL object keys in federation" CHECK(field_name<>'federation')
 CONSTRAINT "rollback refuses NUL object keys in preferences" CHECK(field_name<>'preferences')
 CONSTRAINT "rollback refuses NUL object keys in pendingResponseBoardProjectDetachments" CHECK(field_name<>'pendingResponseBoardProjectDetachments')
 CONSTRAINT "rollback refuses NUL object keys in testRunCards" CHECK(field_name<>'testRunCards')
 CONSTRAINT "rollback refuses NUL object keys in testRunRegistrations" CHECK(field_name<>'testRunRegistrations')
 CONSTRAINT "rollback refuses NUL object keys in testRunObservations" CHECK(field_name<>'testRunObservations')
 CONSTRAINT "rollback refuses NUL object keys in testRunCompacted" CHECK(field_name<>'testRunCompacted')
 CONSTRAINT "rollback refuses NUL object keys in workspaceLayouts" CHECK(field_name<>'workspaceLayouts')
 CONSTRAINT "rollback refuses NUL object keys in codex" CHECK(field_name<>'codex'));
INSERT INTO termal_metadata_rollback_nul_keys
SELECT entry.field_name FROM state_metadata_units AS entry
JOIN state_metadata_units AS root ON root.field_name=entry.field_name AND root.item_key=''
WHERE root.kind='object' AND entry.item_key<>'' AND instr(entry.item_key,char(0))>0;
WITH fields AS (
 SELECT root.field_name,
   CASE root.kind
     WHEN 'object' THEN (SELECT json_group_object(
       CAST(substr(CAST(item_key AS BLOB),length(CAST('/' AS BLOB))+1) AS TEXT),json(value_json))
       FROM state_metadata_units AS entry WHERE entry.field_name=root.field_name AND entry.item_key<>'')
     WHEN 'array' THEN (SELECT json_group_array(json(value_json)) FROM
       (SELECT value_json FROM state_metadata_units AS entry
        WHERE entry.field_name=root.field_name AND entry.item_key<>'' ORDER BY item_key))
     WHEN 'value' THEN root.value_json
   END AS encoded
 FROM state_metadata_units AS root WHERE root.item_key=''
)
INSERT INTO app_state(key,value_json)
SELECT 'metadataState',json_group_object(field_name,json(encoded)) FROM fields WHERE true
ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json;
DROP TABLE state_metadata_units;
DELETE FROM meta WHERE key='metadata_units_generation';
UPDATE meta SET value='2' WHERE key='schema_version';
DROP TABLE termal_metadata_rollback_guard;
DROP TABLE termal_metadata_expected_fields;
DROP TABLE termal_metadata_rollback_nul_keys;
COMMIT;
