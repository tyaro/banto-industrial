-- Indexed record scope for direct audit rows and related rows such as attachments.
-- Keep the generated expressions in sync with banto-admin-services/src/audit.rs's
-- test setup; REST tests exercise this migrated schema rather than that copy.
-- The parent keys are an optional pair in detail; invalid or incomplete JSON
-- retains the row's canonical resource/entity_id. VIRTUAL columns also derive
-- the scope for rows written through the existing audit spool INSERT.
ALTER TABLE audit_log ADD COLUMN record_resource TEXT GENERATED ALWAYS AS (
  COALESCE(
    CASE WHEN json_valid(detail) THEN
      CASE WHEN json_type(detail, '$.parentResource') = 'text'
        AND json_type(detail, '$.parentId') = 'text'
      THEN CAST(json_extract(detail, '$.parentResource') AS TEXT) END
    END,
    resource
  )
) VIRTUAL;

ALTER TABLE audit_log ADD COLUMN record_id TEXT GENERATED ALWAYS AS (
  COALESCE(
    CASE WHEN json_valid(detail) THEN
      CASE WHEN json_type(detail, '$.parentResource') = 'text'
        AND json_type(detail, '$.parentId') = 'text'
      THEN CAST(json_extract(detail, '$.parentId') AS TEXT) END
    END,
    entity_id
  )
) VIRTUAL;

CREATE INDEX idx_audit_log_record_scope ON audit_log(record_resource, record_id);
