-- Schema version 8: forwarded messages. `forwarded` is the
-- `contextInfo.forwardingScore` of a forwarded message, NULL when the
-- message was not forwarded.
ALTER TABLE messages ADD COLUMN forwarded INTEGER;
