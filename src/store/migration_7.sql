-- Schema version 7: timed mutes ("8 hours", "1 week"). `mute_end` is
-- 0 (not muted), -1 (always) or the Unix second the mute ends. The old
-- `muted` flag only knew "always"; it is no longer read.
ALTER TABLE chats ADD COLUMN mute_end INTEGER NOT NULL DEFAULT 0;
UPDATE chats SET mute_end = -1 WHERE muted != 0;
