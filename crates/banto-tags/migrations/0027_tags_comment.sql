-- Additive metadata only. Existing tags and old insert clients receive an empty comment.
ALTER TABLE tags ADD COLUMN comment TEXT NOT NULL DEFAULT '' CHECK (length(comment) <= 500);
