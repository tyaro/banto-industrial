-- S2 文字列タグ: `write_constant_text` carries the constant for a rule whose
-- write TARGET is a string device (write_value_mode='constant'); the numeric
-- `write_constant_value` stays NULL for those rules, and vice versa. Which
-- column must be set depends on the target's data type, enforced at the
-- application layer (relay_wright_core::write_rules) like every other
-- cross-column rule in this schema.
--
-- NOTE: like every file in this directory, this is schema DOCUMENTATION -
-- the executable source of truth is `db.rs::apply_app_schema` (fresh
-- databases include the column in the CREATE TABLE) plus the pragma-guarded
-- ALTER in the same function (existing pre-S2 databases). A plain nullable
-- column needs no rebuild - this is 0003's `users.role` ADD COLUMN pattern.

ALTER TABLE write_rules ADD COLUMN write_constant_text TEXT;
