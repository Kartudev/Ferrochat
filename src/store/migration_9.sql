-- Schema version 9: when each receipt of our messages arrived, for
-- "Message info". One row per message, participant ('' in direct chats)
-- and step (2 = delivered, 3 = read, 4 = played); the first time wins.
CREATE TABLE IF NOT EXISTS receipt_times (
    chat_id     TEXT NOT NULL,
    msg_id      TEXT NOT NULL,
    participant TEXT NOT NULL,
    status      INTEGER NOT NULL,
    ts          INTEGER NOT NULL,
    PRIMARY KEY (chat_id, msg_id, participant, status)
);
