//! Local cache: SQLite via rusqlite.
//!
//! The cache holds chats, messages (bodies + media references), contact
//! names and a few non-secret key/value pairs. Credentials and Signal keys
//! never go here (they are in the keyring and the SQLCipher session store).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, Result};
use crate::net::types::{
    ChatFlag, ChatMeta, Content, ListDef, ListKind, MediaRef, Message, NameSource, ReceiptStatus,
};

const MIGRATIONS: &[&str] = &[
    include_str!("schema.sql"),
    include_str!("migration_2.sql"),
    include_str!("migration_3.sql"),
    include_str!("migration_4.sql"),
    include_str!("migration_5.sql"),
    include_str!("migration_6.sql"),
    include_str!("migration_7.sql"),
    include_str!("migration_8.sql"),
    include_str!("migration_9.sql"),
];

mod social;
pub use social::{MAX_STARRED, MAX_STATUS_ROWS, StoredStatus};

/// Longest emoji string accepted as a reaction (bytes). Skin-tone and ZWJ
/// sequences fit comfortably; anything longer is garbage.
pub const MAX_REACTION_BYTES: usize = 64;
/// Most search hits returned.
pub const MAX_SEARCH_HITS: usize = 100;

/// One message found by a search.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// Chat.
    pub chat: String,
    /// Message id.
    pub id: String,
    /// Unix seconds.
    pub ts: i64,
    /// Sent by us.
    pub from_me: bool,
    /// Sender JID.
    pub sender: String,
    /// Matching text around the hit.
    pub snippet: String,
}

/// A page of messages around an anchor, for "jump to message".
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// Messages, oldest first.
    pub messages: Vec<Message>,
    /// Older messages exist.
    pub has_older: bool,
    /// Newer messages exist.
    pub has_newer: bool,
}

/// Thread-safe handle; clone freely. Heavy calls should run in
/// `spawn_blocking` (the app does).
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Store")
    }
}

/// Chat row plus its newest message, for the chat list.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRow {
    /// Metadata.
    pub meta: ChatMeta,
    /// Newest message, if any.
    pub last: Option<Message>,
}

impl Store {
    /// Opens (creating if needed) the cache at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            crate::config::secure_dir(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let store = Self::init(conn)?;
        crate::config::secure_db_files(path);
        Ok(store)
    }

    /// In-memory cache for tests and `--mock`.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate() {
            let target = i as i64 + 1;
            if version < target {
                conn.execute_batch(sql)?;
                conn.pragma_update(None, "user_version", target)?;
            }
        }
        if version > 0 && version < 3 {
            backfill_search(&conn)?;
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| Error::Protocol("store mutex poisoned".into()))
    }

    /// Schema version (PRAGMA user_version).
    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .lock()?
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    /// Inserts or merges chat metadata. Names are only replaced by non-empty
    /// names, `last_ts` only grows.
    pub fn upsert_chats(&self, chats: &[ChatMeta]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT INTO chats (id, name, unread, last_ts, pinned, archived, mute_end)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                    name = COALESCE(excluded.name, chats.name),
                    unread = excluded.unread,
                    last_ts = MAX(chats.last_ts, excluded.last_ts),
                    pinned = excluded.pinned,
                    archived = excluded.archived,
                    mute_end = excluded.mute_end",
            )?;
            for c in chats {
                st.execute(params![
                    c.id, c.name, c.unread, c.last_ts, c.pinned, c.archived, c.mute_end
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Counts for the info drawer: `(photos and videos, links, documents)`.
    pub fn media_counts(&self, chat: &str) -> Result<(usize, usize, usize)> {
        let conn = self.lock()?;
        let mut st = conn.prepare_cached(
            "SELECT
                COALESCE(SUM(kind IN ('image', 'video')), 0),
                COALESCE(SUM(kind = 'link'), 0),
                COALESCE(SUM(kind = 'document'), 0)
             FROM messages WHERE chat_id = ?1",
        )?;
        let (a, b, c): (i64, i64, i64) =
            st.query_row(params![chat], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        let n = |v: i64| usize::try_from(v).unwrap_or(0);
        Ok((n(a), n(b), n(c)))
    }

    /// The newest messages of `chat` whose kind is one of `kinds`, newest
    /// first (the info drawer's gallery, links and documents).
    pub fn messages_of_kind(
        &self,
        chat: &str,
        kinds: &[&str],
        limit: usize,
    ) -> Result<Vec<Message>> {
        let conn = self.lock()?;
        let mut st = conn.prepare_cached(
            "SELECT chat_id, id, sender, sender_name, from_me, ts, content, thumb, status, expires_at,
                    reply, edited, mentions, forwarded
             FROM messages
             WHERE chat_id = ?1 AND kind IN (SELECT value FROM json_each(?2))
             ORDER BY ts DESC, id DESC LIMIT ?3",
        )?;
        let kinds = serde_json::to_string(kinds).map_err(|e| Error::Protocol(e.to_string()))?;
        let rows = st
            .query_map(params![chat, kinds, limit.min(500) as i64], row_to_message)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Stores a poster frame as the thumbnail of a message that has none.
    pub fn set_thumb(&self, chat: &str, id: &str, thumb: &[u8]) -> Result<()> {
        self.lock()?.execute(
            "UPDATE messages SET thumb = ?3 WHERE chat_id = ?1 AND id = ?2 AND thumb IS NULL",
            params![chat, id, thumb],
        )?;
        Ok(())
    }

    /// Sets the unread counter of a chat.
    pub fn set_unread(&self, chat: &str, unread: u32) -> Result<()> {
        self.lock()?.execute(
            "UPDATE chats SET unread = ?2 WHERE id = ?1",
            params![chat, unread],
        )?;
        Ok(())
    }

    /// Inserts messages (creating chats as needed). Existing messages keep
    /// their content unless the new one is a deletion; statuses go through
    /// the receipt state machine. Returns the ids that were new.
    pub fn insert_messages(&self, msgs: &[Message]) -> Result<Vec<String>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut new_ids = Vec::new();
        {
            let mut existing = tx.prepare_cached(
                "SELECT status, sender, from_me FROM messages WHERE chat_id = ?1 AND id = ?2",
            )?;
            let mut insert = tx.prepare_cached(
                "INSERT INTO messages
                    (chat_id, id, sender, sender_name, from_me, ts, kind, content, thumb, status,
                     expires_at, reply, edited, search, mentions, forwarded)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            )?;
            let mut react = tx.prepare_cached(
                "INSERT INTO reactions (chat_id, msg_id, reactor, emoji, ts)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(chat_id, msg_id, reactor) DO UPDATE SET
                    emoji = excluded.emoji, ts = excluded.ts
                 WHERE excluded.ts >= reactions.ts",
            )?;
            let mut update_status = tx
                .prepare_cached("UPDATE messages SET status = ?3 WHERE chat_id = ?1 AND id = ?2")?;
            let mut update_content = tx.prepare_cached(
                "UPDATE messages SET kind = ?3, content = ?4, thumb = NULL, search = NULL
                 WHERE chat_id = ?1 AND id = ?2",
            )?;
            let mut chat = tx.prepare_cached(
                "INSERT INTO chats (id, last_ts) VALUES (?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET last_ts = MAX(chats.last_ts, excluded.last_ts)",
            )?;
            for m in msgs {
                let prev: Option<(i64, String, bool)> = existing
                    .query_row(params![m.chat, m.id], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })
                    .optional()?;
                let (json, thumb) = encode_content(&m.content)?;
                match prev {
                    None if matches!(m.content, Content::Deleted) => {
                        // A revoke for a message we never saw: nothing to delete.
                        continue;
                    }
                    None => {
                        insert.execute(params![
                            m.chat,
                            m.id,
                            m.sender,
                            m.sender_name,
                            m.from_me,
                            m.timestamp,
                            m.content.kind_tag(),
                            json,
                            thumb,
                            m.status.to_i64(),
                            m.expires_at,
                            m.reply.as_ref().and_then(|q| serde_json::to_string(q).ok()),
                            m.edited,
                            search_text(&m.content),
                            encode_mentions(&m.mentions),
                            m.forwarded,
                        ])?;
                        new_ids.push(m.id.clone());
                    }
                    Some((s, sender, from_me)) => {
                        let merged = ReceiptStatus::from_i64(s).advance(m.status);
                        update_status.execute(params![m.chat, m.id, merged.to_i64()])?;
                        if matches!(m.content, Content::Deleted)
                            && revoke_allowed(from_me, &sender, m)
                        {
                            update_content.execute(params![
                                m.chat,
                                m.id,
                                m.content.kind_tag(),
                                json
                            ])?;
                        }
                    }
                }
                chat.execute(params![m.chat, m.timestamp])?;
                for r in m
                    .reactions
                    .iter()
                    .filter(|r| valid_reaction(&r.reactor, &r.emoji))
                {
                    react.execute(params![m.chat, m.id, r.reactor, r.emoji, r.ts])?;
                }
            }
        }
        tx.commit()?;
        Ok(new_ids)
    }

    /// Replaces a message's content (e.g. after a media upload filled in
    /// the CDN reference) and/or status.
    /// Points a cached message at re-uploaded media (new `directPath`).
    /// `false` when the message is not cached or has no media.
    pub fn set_media_ref(&self, chat: &str, id: &str, media: &MediaRef) -> Result<bool> {
        let Some(m) = self.message(chat, id)? else {
            return Ok(false);
        };
        let mut content = m.content;
        let Some(slot) = content.media_mut() else {
            return Ok(false);
        };
        *slot = media.clone();
        self.update_message(chat, id, &content)?;
        Ok(true)
    }

    pub fn update_message(&self, chat: &str, id: &str, content: &Content) -> Result<()> {
        let (json, thumb) = encode_content(content)?;
        self.lock()?.execute(
            "UPDATE messages SET kind = ?3, content = ?4, thumb = COALESCE(?5, thumb)
             WHERE chat_id = ?1 AND id = ?2",
            params![chat, id, content.kind_tag(), json, thumb],
        )?;
        Ok(())
    }

    /// Applies a receipt. Returns `(id, new_status)` for rows that changed.
    pub fn apply_receipt(
        &self,
        chat: &str,
        ids: &[String],
        status: ReceiptStatus,
    ) -> Result<Vec<(String, ReceiptStatus)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut changed = Vec::new();
        {
            let mut get = tx.prepare_cached(
                "SELECT status FROM messages WHERE chat_id = ?1 AND id = ?2 AND from_me = 1",
            )?;
            let mut set = tx
                .prepare_cached("UPDATE messages SET status = ?3 WHERE chat_id = ?1 AND id = ?2")?;
            for id in ids {
                let Some(cur) = get
                    .query_row(params![chat, id], |r| r.get::<_, i64>(0))
                    .optional()?
                else {
                    continue;
                };
                let cur = ReceiptStatus::from_i64(cur);
                let next = cur.advance(status);
                if next != cur {
                    set.execute(params![chat, id, next.to_i64()])?;
                    changed.push((id.clone(), next));
                }
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Messages of a chat, oldest first. `before` pages backwards from a
    /// `(timestamp, id)` cursor.
    pub fn load_messages(
        &self,
        chat: &str,
        before: Option<(i64, &str)>,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let conn = self.lock()?;
        let (ts, id) = before.unwrap_or((i64::MAX, ""));
        let mut st = conn.prepare_cached(
            "SELECT chat_id, id, sender, sender_name, from_me, ts, content, thumb, status, expires_at,
                    reply, edited, mentions, forwarded
             FROM messages
             WHERE chat_id = ?1 AND (ts < ?2 OR (ts = ?2 AND id < ?3))
             ORDER BY ts DESC, id DESC LIMIT ?4",
        )?;
        let id_bound = if before.is_some() { id } else { "\u{10FFFF}" };
        let mut rows: Vec<Message> = st
            .query_map(params![chat, ts, id_bound, limit as i64], row_to_message)?
            .collect::<std::result::Result<_, _>>()?;
        rows.reverse();
        attach_reactions(&conn, chat, &mut rows)?;
        Ok(rows)
    }

    /// One message.
    pub fn message(&self, chat: &str, id: &str) -> Result<Option<Message>> {
        let conn = self.lock()?;
        let mut st = conn.prepare_cached(
            "SELECT chat_id, id, sender, sender_name, from_me, ts, content, thumb, status, expires_at,
                    reply, edited, mentions, forwarded
             FROM messages WHERE chat_id = ?1 AND id = ?2",
        )?;
        let mut m = st.query_row(params![chat, id], row_to_message).optional()?;
        if let Some(m) = &mut m {
            attach_reactions(&conn, chat, std::slice::from_mut(m))?;
        }
        Ok(m)
    }

    /// All chats with their newest message, most recent first (pinned on top).
    pub fn chat_rows(&self) -> Result<Vec<ChatRow>> {
        let conn = self.lock()?;
        let mut st = conn.prepare_cached(
            "SELECT c.id, c.name, c.unread, c.last_ts, c.pinned, c.archived, c.mute_end,
                    m.chat_id, m.id, m.sender, m.sender_name, m.from_me, m.ts, m.content,
                    m.thumb, m.status, m.expires_at, m.reply, m.edited, m.mentions, m.forwarded
             FROM chats c
             LEFT JOIN messages m ON m.rowid = (
                SELECT rowid FROM messages WHERE chat_id = c.id ORDER BY ts DESC, id DESC LIMIT 1
             )
             ORDER BY c.pinned DESC, c.last_ts DESC",
        )?;
        let rows = st
            .query_map([], |r| {
                let meta = ChatMeta {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    unread: r.get(2)?,
                    last_ts: r.get(3)?,
                    pinned: r.get(4)?,
                    archived: r.get(5)?,
                    mute_end: r.get(6)?,
                };
                let last = match r.get::<_, Option<String>>(7)? {
                    Some(_) => Some(row_to_message_at(r, 7)?),
                    None => None,
                };
                Ok(ChatRow { meta, last })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// Records contact names `(jid, source, name)`. An empty address-book
    /// (`Saved`) name removes the saved name; other empty names are ignored.
    pub fn set_contact_names(&self, names: &[(String, NameSource, String)]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        {
            let mut put = tx.prepare_cached(
                "INSERT INTO contact_names (jid, source, name) VALUES (?1, ?2, ?3)
                 ON CONFLICT(jid, source) DO UPDATE SET name = excluded.name",
            )?;
            let mut del =
                tx.prepare_cached("DELETE FROM contact_names WHERE jid = ?1 AND source = ?2")?;
            for (jid, source, name) in names {
                if name.is_empty() {
                    if *source == NameSource::Saved {
                        del.execute(params![jid, source.code()])?;
                    }
                } else {
                    put.execute(params![jid, source.code(), name])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// All known contact names `(jid, source, name)`.
    pub fn contact_names(&self) -> Result<Vec<(String, NameSource, String)>> {
        let conn = self.lock()?;
        let mut st = conn.prepare_cached("SELECT jid, source, name FROM contact_names")?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(j, c, n)| Some((j, NameSource::from_code(c)?, n)))
            .collect())
    }

    /// Persists an archive / pin / mute change (creating the chat row).
    pub fn set_chat_flag(&self, chat: &str, flag: ChatFlag) -> Result<()> {
        let (col, value) = match flag {
            ChatFlag::Archived(v) => ("archived", i64::from(v)),
            ChatFlag::Pinned(v) => ("pinned", i64::from(v)),
            ChatFlag::Muted(end) => ("mute_end", end),
        };
        // `col` is one of three literals above, never user input.
        let sql = format!(
            "INSERT INTO chats (id, {col}) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET {col} = excluded.{col}"
        );
        self.lock()?.execute(&sql, params![chat, value])?;
        Ok(())
    }

    /// "Clear chat": removes the chat's messages (keeping the starred ones
    /// when `keep_starred`), their reactions and receipts. The chat stays.
    pub fn clear_chat(&self, chat: &str, keep_starred: bool) -> Result<()> {
        let keep = if keep_starred {
            " AND id NOT IN (SELECT msg_id FROM starred WHERE chat_id = ?1)"
        } else {
            ""
        };
        let keep_ref = if keep_starred {
            " AND msg_id NOT IN (SELECT msg_id FROM starred WHERE chat_id = ?1)"
        } else {
            ""
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        // Only the two literal filters above are spliced in.
        tx.execute(
            &format!("DELETE FROM reactions WHERE chat_id = ?1{keep_ref}"),
            params![chat],
        )?;
        tx.execute(
            &format!("DELETE FROM receipts WHERE chat_id = ?1{keep_ref}"),
            params![chat],
        )?;
        tx.execute(
            &format!("DELETE FROM receipt_times WHERE chat_id = ?1{keep_ref}"),
            params![chat],
        )?;
        tx.execute(
            &format!("DELETE FROM messages WHERE chat_id = ?1{keep}"),
            params![chat],
        )?;
        if !keep_starred {
            tx.execute("DELETE FROM starred WHERE chat_id = ?1", params![chat])?;
        }
        tx.execute("UPDATE chats SET unread = 0 WHERE id = ?1", params![chat])?;
        tx.commit()?;
        Ok(())
    }

    /// "Delete chat": the chat, its messages and everything hanging off
    /// them. List memberships stay: they belong to the phone.
    pub fn delete_chat(&self, chat: &str) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        for sql in [
            "DELETE FROM reactions WHERE chat_id = ?1",
            "DELETE FROM receipts WHERE chat_id = ?1",
            "DELETE FROM receipt_times WHERE chat_id = ?1",
            "DELETE FROM messages WHERE chat_id = ?1",
            "DELETE FROM starred WHERE chat_id = ?1",
            "DELETE FROM chats WHERE id = ?1",
        ] {
            tx.execute(sql, params![chat])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Creates or updates a list definition.
    pub fn save_list(&self, def: &ListDef) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO lists (id, name, color, kind, ord) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, color = excluded.color,
                kind = excluded.kind, ord = excluded.ord",
            params![def.id, def.name, def.color, def.kind.code(), def.order],
        )?;
        Ok(())
    }

    /// Deletes a list and its members.
    pub fn delete_list(&self, id: &str) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM lists WHERE id = ?1", params![id])?;
        tx.execute("DELETE FROM list_chats WHERE list_id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    /// Adds / removes a chat from a list.
    pub fn set_list_member(&self, list: &str, chat: &str, member: bool) -> Result<()> {
        let conn = self.lock()?;
        if member {
            conn.execute(
                "INSERT OR IGNORE INTO list_chats (list_id, chat_id) VALUES (?1, ?2)",
                params![list, chat],
            )?;
        } else {
            conn.execute(
                "DELETE FROM list_chats WHERE list_id = ?1 AND chat_id = ?2",
                params![list, chat],
            )?;
        }
        Ok(())
    }

    /// All lists and their `(list, chat)` memberships.
    #[allow(clippy::type_complexity)]
    pub fn load_lists(&self) -> Result<(Vec<ListDef>, Vec<(String, String)>)> {
        let conn = self.lock()?;
        let mut st = conn.prepare_cached("SELECT id, name, color, kind, ord FROM lists")?;
        let defs = st
            .query_map([], |r| {
                Ok(ListDef {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    color: r.get(2)?,
                    kind: ListKind::from_code(r.get(3)?),
                    order: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut st = conn.prepare_cached("SELECT list_id, chat_id FROM list_chats")?;
        let members = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok((defs, members))
    }

    /// Non-secret settings (own JID, last open chat).
    pub fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Reads a setting.
    pub fn kv(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .lock()?
            .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Deletes disappearing messages whose timer ran out. Returns the count.
    pub fn prune_expired(&self, now: i64) -> Result<usize> {
        Ok(self.lock()?.execute(
            "DELETE FROM messages WHERE expires_at IS NOT NULL AND expires_at <= ?1",
            params![now],
        )?)
    }

    /// Persists LID → phone-number pairs.
    pub fn set_lid_pairs(&self, pairs: &[(String, String)]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT INTO lid_map (lid, pn) VALUES (?1, ?2)
                 ON CONFLICT(lid) DO UPDATE SET pn = excluded.pn",
            )?;
            for (lid, pn) in pairs {
                st.execute(params![lid, pn])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// All known LID → phone-number pairs.
    pub fn lid_pairs(&self) -> Result<Vec<(String, String)>> {
        let conn = self.lock()?;
        let mut st = conn.prepare_cached("SELECT lid, pn FROM lid_map")?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    /// Wipes everything (logout).
    pub fn clear(&self) -> Result<()> {
        self.lock()?.execute_batch(
            "DELETE FROM messages; DELETE FROM chats; DELETE FROM contact_names; DELETE FROM kv;
             DELETE FROM lid_map; DELETE FROM reactions; DELETE FROM receipts; DELETE FROM receipt_times;
             DELETE FROM lists; DELETE FROM list_chats; DELETE FROM statuses; DELETE FROM starred;",
        )?;
        Ok(())
    }

    /// Records, replaces or (empty emoji) removes one reactor's reaction.
    /// Older reactions never overwrite newer ones. Returns true when the
    /// stored state changed.
    pub fn apply_reaction(
        &self,
        chat: &str,
        target: &str,
        reactor: &str,
        emoji: &str,
        ts: i64,
    ) -> Result<bool> {
        let conn = self.lock()?;
        if emoji.is_empty() {
            let n = conn.execute(
                "DELETE FROM reactions
                 WHERE chat_id = ?1 AND msg_id = ?2 AND reactor = ?3 AND ts <= ?4",
                params![chat, target, reactor, ts],
            )?;
            return Ok(n > 0);
        }
        if !valid_reaction(reactor, emoji) {
            return Ok(false);
        }
        let n = conn.execute(
            "INSERT INTO reactions (chat_id, msg_id, reactor, emoji, ts) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(chat_id, msg_id, reactor) DO UPDATE SET emoji = excluded.emoji, ts = excluded.ts
             WHERE excluded.ts >= reactions.ts",
            params![chat, target, reactor, emoji, ts],
        )?;
        Ok(n > 0)
    }

    /// Applies an edit from `sender`. Only the original sender may edit
    /// (same rule as revokes) and deleted messages stay deleted. Returns the
    /// updated message when the edit was applied.
    pub fn apply_edit(
        &self,
        chat: &str,
        id: &str,
        sender: &str,
        from_me: bool,
        content: &Content,
    ) -> Result<Option<Message>> {
        let Some(orig) = self.message(chat, id)? else {
            return Ok(None);
        };
        if matches!(orig.content, Content::Deleted) {
            return Ok(None);
        }
        let allowed = edit_allowed(orig.from_me, &orig.sender, sender, from_me);
        let Some(merged) = crate::net::types::merge_edit(&orig.content, content) else {
            return Ok(None);
        };
        if !allowed {
            return Ok(None);
        }
        let (json, _) = encode_content(&merged)?;
        self.lock()?.execute(
            "UPDATE messages SET kind = ?3, content = ?4, edited = 1, search = ?5
             WHERE chat_id = ?1 AND id = ?2",
            params![chat, id, merged.kind_tag(), json, search_text(&merged)],
        )?;
        self.message(chat, id)
    }

    /// Removes one message locally ("delete for me").
    pub fn delete_message(&self, chat: &str, id: &str) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        for sql in [
            "DELETE FROM messages WHERE chat_id = ?1 AND id = ?2",
            "DELETE FROM reactions WHERE chat_id = ?1 AND msg_id = ?2",
            "DELETE FROM receipts WHERE chat_id = ?1 AND msg_id = ?2",
            "DELETE FROM receipt_times WHERE chat_id = ?1 AND msg_id = ?2",
        ] {
            tx.execute(sql, params![chat, id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Case-insensitive substring search over message text, newest first.
    /// `chat = None` searches every chat.
    pub fn search_messages(
        &self,
        chat: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let q = query.trim().to_lowercase();
        if q.chars().count() < 2 {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, MAX_SEARCH_HITS) as i64;
        let conn = self.lock()?;
        let mut st = conn.prepare_cached(
            "SELECT chat_id, id, ts, from_me, sender, content FROM messages
             WHERE search IS NOT NULL AND instr(search, ?1) > 0
               AND (?2 IS NULL OR chat_id = ?2)
             ORDER BY ts DESC, id DESC LIMIT ?3",
        )?;
        let hits = st
            .query_map(params![q, chat, limit], |r| {
                let json: String = r.get(5)?;
                // Show the text as written, not the lower-cased search copy.
                let text = serde_json::from_str::<Content>(&json)
                    .ok()
                    .and_then(|c| display_text(&c))
                    .unwrap_or_default();
                Ok(SearchHit {
                    chat: r.get(0)?,
                    id: r.get(1)?,
                    ts: r.get(2)?,
                    from_me: r.get(3)?,
                    sender: r.get(4)?,
                    snippet: snippet_around(&text, &q),
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(hits)
    }

    /// Messages around `(ts, id)`: up to `before` older and `after` newer
    /// ones plus the anchor itself.
    pub fn load_around(
        &self,
        chat: &str,
        ts: i64,
        id: &str,
        before: usize,
        after: usize,
    ) -> Result<Window> {
        let conn = self.lock()?;
        let cols = "chat_id, id, sender, sender_name, from_me, ts, content, thumb, status,
                    expires_at, reply, edited, mentions, forwarded";
        let mut older = conn
            .prepare_cached(&format!(
                "SELECT {cols} FROM messages
                 WHERE chat_id = ?1 AND (ts < ?2 OR (ts = ?2 AND id < ?3))
                 ORDER BY ts DESC, id DESC LIMIT ?4"
            ))?
            .query_map(params![chat, ts, id, before as i64 + 1], row_to_message)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let has_older = older.len() > before;
        older.truncate(before);
        older.reverse();
        let newer = conn
            .prepare_cached(&format!(
                "SELECT {cols} FROM messages
                 WHERE chat_id = ?1 AND (ts > ?2 OR (ts = ?2 AND id >= ?3))
                 ORDER BY ts ASC, id ASC LIMIT ?4"
            ))?
            .query_map(params![chat, ts, id, after as i64 + 2], row_to_message)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let has_newer = newer.len() > after + 1;
        let mut messages = older;
        messages.extend(newer.into_iter().take(after + 1));
        attach_reactions(&conn, chat, &mut messages)?;
        Ok(Window {
            messages,
            has_older,
            has_newer,
        })
    }

    /// The page of messages after the `(ts, id)` cursor, oldest first.
    pub fn load_newer(
        &self,
        chat: &str,
        after: (i64, &str),
        limit: usize,
    ) -> Result<(Vec<Message>, bool)> {
        let conn = self.lock()?;
        let mut rows = conn
            .prepare_cached(
                "SELECT chat_id, id, sender, sender_name, from_me, ts, content, thumb, status,
                        expires_at, reply, edited, mentions, forwarded
                 FROM messages
                 WHERE chat_id = ?1 AND (ts > ?2 OR (ts = ?2 AND id > ?3))
                 ORDER BY ts ASC, id ASC LIMIT ?4",
            )?
            .query_map(
                params![chat, after.0, after.1, limit as i64 + 1],
                row_to_message,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        attach_reactions(&conn, chat, &mut rows)?;
        Ok((rows, more))
    }

    /// Records one group participant's receipt for each id and re-derives
    /// the aggregate tick of our messages. `size` is the number of other
    /// participants; without it nothing is promoted yet. Returns the
    /// `(id, new_status)` pairs that changed.
    pub fn apply_group_receipt(
        &self,
        chat: &str,
        ids: &[String],
        participant: Option<(&str, ReceiptStatus)>,
        size: Option<usize>,
    ) -> Result<Vec<(String, ReceiptStatus)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut changed = Vec::new();
        {
            let mut record = tx.prepare_cached(
                "INSERT INTO receipts (chat_id, msg_id, participant, status) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(chat_id, msg_id, participant) DO UPDATE SET status = excluded.status
                 WHERE excluded.status > receipts.status",
            )?;
            let mut own = tx.prepare_cached(
                "SELECT status FROM messages WHERE chat_id = ?1 AND id = ?2 AND from_me = 1",
            )?;
            let mut counts = tx.prepare_cached(
                "SELECT COALESCE(SUM(status >= 2), 0), COALESCE(SUM(status >= 3), 0)
                 FROM receipts WHERE chat_id = ?1 AND msg_id = ?2",
            )?;
            let mut set = tx
                .prepare_cached("UPDATE messages SET status = ?3 WHERE chat_id = ?1 AND id = ?2")?;
            for id in ids {
                let Some(cur) = own
                    .query_row(params![chat, id], |r| r.get::<_, i64>(0))
                    .optional()?
                else {
                    continue;
                };
                if let Some((p, status)) = participant {
                    record.execute(params![chat, id, p, status.to_i64()])?;
                }
                let Some(size) = size else { continue };
                let (delivered, read): (i64, i64) =
                    counts.query_row(params![chat, id], |r| Ok((r.get(0)?, r.get(1)?)))?;
                let cur = ReceiptStatus::from_i64(cur);
                let Some(agg) =
                    crate::app::state::group_status(size, delivered as usize, read as usize)
                else {
                    continue;
                };
                let next = cur.advance(agg);
                if next != cur {
                    set.execute(params![chat, id, next.to_i64()])?;
                    changed.push((id.clone(), next));
                }
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Records when a delivered / read / played receipt of our messages
    /// arrived (`participant` is `""` in direct chats). The first time of
    /// each step wins; other people's messages are skipped.
    pub fn record_receipt_times(
        &self,
        chat: &str,
        ids: &[String],
        participant: &str,
        status: ReceiptStatus,
        ts: i64,
    ) -> Result<()> {
        if !matches!(
            status,
            ReceiptStatus::Delivered | ReceiptStatus::Read | ReceiptStatus::Played
        ) {
            return Ok(());
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        {
            let mut ins = tx.prepare_cached(
                "INSERT OR IGNORE INTO receipt_times (chat_id, msg_id, participant, status, ts)
                 SELECT ?1, ?2, ?3, ?4, ?5
                 WHERE EXISTS (SELECT 1 FROM messages
                               WHERE chat_id = ?1 AND id = ?2 AND from_me = 1)",
            )?;
            for id in ids {
                ins.execute(params![chat, id, participant, status.to_i64(), ts])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Everything known about who received / read one of our messages:
    /// one row per participant (`""` in direct chats) with its furthest
    /// status and the time of each step, when it was seen arriving.
    pub fn receipt_details(&self, chat: &str, id: &str) -> Result<Vec<ReceiptDetail>> {
        let conn = self.lock()?;
        let mut out: std::collections::BTreeMap<String, ReceiptDetail> = Default::default();
        let mut times = conn.prepare_cached(
            "SELECT participant, status, ts FROM receipt_times WHERE chat_id = ?1 AND msg_id = ?2",
        )?;
        let rows = times.query_map(params![chat, id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                ReceiptStatus::from_i64(r.get(1)?),
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (p, status, ts) = row?;
            let d = out
                .entry(p.clone())
                .or_insert_with(|| ReceiptDetail::new(p));
            d.status = d.status.advance(status);
            match status {
                ReceiptStatus::Delivered => d.delivered = Some(ts),
                ReceiptStatus::Read => d.read = Some(ts),
                ReceiptStatus::Played => d.played = Some(ts),
                _ => {}
            }
        }
        let mut st = conn.prepare_cached(
            "SELECT participant, status FROM receipts WHERE chat_id = ?1 AND msg_id = ?2",
        )?;
        let rows = st.query_map(params![chat, id], |r| {
            Ok((r.get::<_, String>(0)?, ReceiptStatus::from_i64(r.get(1)?)))
        })?;
        for row in rows {
            let (p, status) = row?;
            let d = out
                .entry(p.clone())
                .or_insert_with(|| ReceiptDetail::new(p));
            d.status = d.status.advance(status);
        }
        Ok(out.into_values().collect())
    }

    /// Per-participant receipts of a message: `(participant, status)`.
    pub fn receipts_of(&self, chat: &str, id: &str) -> Result<Vec<(String, ReceiptStatus)>> {
        let conn = self.lock()?;
        let rows = conn
            .prepare_cached(
                "SELECT participant, status FROM receipts WHERE chat_id = ?1 AND msg_id = ?2
                 ORDER BY participant",
            )?
            .query_map(params![chat, id], |r| {
                Ok((r.get(0)?, ReceiptStatus::from_i64(r.get(1)?)))
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }
}

/// One participant's receipts of one of our messages ([`Store::receipt_details`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptDetail {
    /// Bare JID, `""` in direct chats.
    pub participant: String,
    /// Furthest step seen.
    pub status: ReceiptStatus,
    /// Unix seconds of each step, when its receipt was seen arriving.
    pub delivered: Option<i64>,
    pub read: Option<i64>,
    pub played: Option<i64>,
}

impl ReceiptDetail {
    fn new(participant: String) -> Self {
        Self {
            participant,
            status: ReceiptStatus::Sent,
            delivered: None,
            read: None,
            played: None,
        }
    }
}

/// An edit only applies when the editor sent the original message: us (from
/// any of our devices) or the same remote sender.
pub fn edit_allowed(orig_from_me: bool, orig_sender: &str, sender: &str, from_me: bool) -> bool {
    if orig_from_me || from_me {
        return orig_from_me && from_me;
    }
    !sender.is_empty() && sender == orig_sender
}

/// "Delete for everyone" only applies when the revoker sent the original
/// message: our own (from any of our devices) or the same remote sender.
/// Group-admin revokes cannot be verified here and are ignored.
pub fn revoke_allowed(orig_from_me: bool, orig_sender: &str, revoke: &Message) -> bool {
    if orig_from_me || revoke.from_me {
        return orig_from_me && revoke.from_me;
    }
    !revoke.sender.is_empty() && revoke.sender == orig_sender
}

fn valid_reaction(reactor: &str, emoji: &str) -> bool {
    !reactor.is_empty() && !emoji.is_empty() && emoji.len() <= MAX_REACTION_BYTES
}

/// Lower-cased text the search looks at.
pub fn search_text(c: &Content) -> Option<String> {
    display_text(c).map(|t| t.to_lowercase())
}

/// The text of a message the search matches against, as written.
pub fn display_text(c: &Content) -> Option<String> {
    let t = match c {
        Content::Text(t) | Content::Link { text: t, .. } => t.as_str(),
        Content::Image { caption, .. }
        | Content::Video { caption, .. }
        | Content::Document {
            caption: caption @ Some(_),
            ..
        } => caption.as_deref()?,
        Content::Document { file_name, .. } => file_name.as_str(),
        _ => return None,
    };
    (!t.is_empty()).then(|| t.to_string())
}

/// A short excerpt of `text` around the first (case-insensitive) occurrence
/// of `q` (already lower-cased).
pub fn snippet_around(text: &str, q: &str) -> String {
    const BEFORE: usize = 24;
    const TOTAL: usize = 80;
    let lower = text.to_lowercase();
    let Some(pos) = lower.find(q) else {
        return text.chars().take(TOTAL).collect();
    };
    // Char index of the match (lower-casing keeps char counts in practice).
    let chars_before = lower[..pos].chars().count();
    let skip = chars_before.saturating_sub(BEFORE);
    let mut out: String = text.chars().skip(skip).take(TOTAL).collect();
    if skip > 0 {
        out.insert(0, '…');
    }
    if text.chars().count() > skip + TOTAL {
        out.push('…');
    }
    out.replace('\n', " ")
}

fn backfill_search(conn: &Connection) -> Result<()> {
    let mut rows = Vec::new();
    {
        let mut st =
            conn.prepare("SELECT chat_id, id, content FROM messages WHERE search IS NULL")?;
        let mut q = st.query([])?;
        while let Some(r) = q.next()? {
            let (chat, id, json): (String, String, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
            if let Some(t) = serde_json::from_str::<Content>(&json)
                .ok()
                .and_then(|c| search_text(&c))
            {
                rows.push((chat, id, t));
            }
        }
    }
    let mut set = conn.prepare("UPDATE messages SET search = ?3 WHERE chat_id = ?1 AND id = ?2")?;
    for (chat, id, t) in rows {
        set.execute(params![chat, id, t])?;
    }
    Ok(())
}

/// Fills `reactions` of `msgs` (all in `chat`) with one query.
fn attach_reactions(conn: &Connection, chat: &str, msgs: &mut [Message]) -> Result<()> {
    if msgs.is_empty() {
        return Ok(());
    }
    // Fetch by id set through a json array so the statement stays cacheable
    // whatever the page size.
    let ids: Vec<&str> = msgs.iter().map(|m| m.id.as_str()).collect();
    let json = serde_json::to_string(&ids).map_err(|e| Error::Protocol(e.to_string()))?;
    let mut st = conn.prepare_cached(
        "SELECT msg_id, reactor, emoji, ts FROM reactions
         WHERE chat_id = ?1 AND msg_id IN (SELECT value FROM json_each(?2))
         ORDER BY ts ASC, reactor ASC",
    )?;
    let mut by_id: HashMap<String, Vec<crate::net::types::Reaction>> = HashMap::new();
    let mut rows = st.query(params![chat, json])?;
    while let Some(r) = rows.next()? {
        by_id
            .entry(r.get(0)?)
            .or_default()
            .push(crate::net::types::Reaction {
                reactor: r.get(1)?,
                emoji: r.get(2)?,
                ts: r.get(3)?,
            });
    }
    for m in msgs {
        if let Some(v) = by_id.remove(&m.id) {
            m.reactions = v;
        }
    }
    Ok(())
}

fn encode_content(c: &Content) -> Result<(String, Option<Vec<u8>>)> {
    let json = serde_json::to_string(c).map_err(|e| Error::Protocol(e.to_string()))?;
    let thumb = match c {
        Content::Image { thumbnail, .. } | Content::Video { thumbnail, .. } => thumbnail.clone(),
        Content::Link { preview, .. } => preview.thumbnail.clone(),
        _ => None,
    };
    Ok((json, thumb))
}

fn row_to_message(r: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    row_to_message_at(r, 0)
}

fn row_to_message_at(r: &rusqlite::Row<'_>, o: usize) -> rusqlite::Result<Message> {
    let json: String = r.get(o + 6)?;
    let thumb: Option<Vec<u8>> = r.get(o + 7)?;
    let mut content: Content = serde_json::from_str(&json)
        .unwrap_or_else(|_| Content::Unsupported("unreadable cache entry".into()));
    match &mut content {
        Content::Image { thumbnail, .. } | Content::Video { thumbnail, .. } => *thumbnail = thumb,
        Content::Link { preview, .. } => preview.thumbnail = thumb,
        _ => {}
    }
    Ok(Message {
        chat: r.get(o)?,
        id: r.get(o + 1)?,
        sender: r.get(o + 2)?,
        sender_name: r.get(o + 3)?,
        from_me: r.get(o + 4)?,
        timestamp: r.get(o + 5)?,
        content,
        status: ReceiptStatus::from_i64(r.get(o + 8)?),
        expires_at: r.get(o + 9)?,
        reply: r
            .get::<_, Option<String>>(o + 10)?
            .and_then(|j| serde_json::from_str(&j).ok()),
        edited: r.get(o + 11)?,
        reactions: Vec::new(),
        mentions: r
            .get::<_, Option<String>>(o + 12)?
            .map(|j| decode_mentions(&j))
            .unwrap_or_default(),
        forwarded: r.get(o + 13)?,
    })
}

/// Mentioned JIDs as stored: a JSON array, NULL when there are none.
fn encode_mentions(m: &[String]) -> Option<String> {
    (!m.is_empty())
        .then(|| serde_json::to_string(m).ok())
        .flatten()
}

/// Reads stored mentions; garbage reads as none.
fn decode_mentions(json: &str) -> Vec<String> {
    let mut v: Vec<String> = serde_json::from_str(json).unwrap_or_default();
    v.truncate(crate::net::types::MAX_MENTIONS);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(chat: &str, id: &str, ts: i64, from_me: bool, text: &str) -> Message {
        Message {
            id: id.into(),
            chat: chat.into(),
            sender: if from_me { "me".into() } else { chat.into() },
            sender_name: None,
            from_me,
            timestamp: ts,
            content: Content::Text(text.into()),
            status: if from_me {
                ReceiptStatus::Sent
            } else {
                ReceiptStatus::Read
            },
            expires_at: None,
            reply: None,
            edited: false,
            forwarded: None,
            mentions: Vec::new(),
            reactions: Vec::new(),
        }
    }

    #[test]
    fn mentions_round_trip_through_the_cache() {
        let s = Store::open_in_memory().unwrap();
        let mut m = msg("g@g.us", "1", 10, false, "@123456789012345 hi");
        m.mentions = vec!["123456789012345@lid".into()];
        let plain = msg("g@g.us", "2", 11, false, "no mentions");
        s.insert_messages(&[m.clone(), plain]).unwrap();
        let page = s.load_messages("g@g.us", None, 10).unwrap();
        assert_eq!(page[0].mentions, m.mentions);
        assert!(page[1].mentions.is_empty());
        assert_eq!(s.chat_rows().unwrap()[0].last.as_ref().unwrap().id, "2");
        assert!(decode_mentions("not json").is_empty());
        assert_eq!(encode_mentions(&[]), None);
    }

    #[test]
    fn a_reuploaded_media_path_is_kept() {
        let s = Store::open_in_memory().unwrap();
        let mref = MediaRef {
            direct_path: "/old".into(),
            media_key: vec![1; 32],
            file_sha256: vec![2; 32],
            file_enc_sha256: vec![3; 32],
            file_length: 10,
            kind: crate::net::MediaKind::Image,
            mimetype: "image/jpeg".into(),
        };
        let mut m = msg("c", "p", 10, false, "");
        m.content = Content::Image {
            caption: Some("hi".into()),
            width: 4,
            height: 3,
            thumbnail: Some(vec![7]),
            media: Some(mref.clone()),
        };
        s.insert_messages(&[m]).unwrap();
        let fresh = MediaRef {
            direct_path: "/new".into(),
            ..mref
        };
        assert!(s.set_media_ref("c", "p", &fresh).unwrap());
        let got = s.message("c", "p").unwrap().unwrap();
        assert_eq!(got.content.media(), Some(&fresh));
        assert!(matches!(
            got.content,
            Content::Image { thumbnail: Some(ref t), caption: Some(_), .. } if t == &[7]
        ));
        assert!(!s.set_media_ref("c", "missing", &fresh).unwrap());
        s.insert_messages(&[msg("c", "t", 11, false, "text")])
            .unwrap();
        assert!(!s.set_media_ref("c", "t", &fresh).unwrap());
    }

    #[test]
    fn a_poster_frame_is_kept_as_the_video_thumbnail() {
        let s = Store::open_in_memory().unwrap();
        let mut m = msg("c", "v", 10, false, "");
        m.content = Content::Video {
            caption: None,
            seconds: 3,
            width: 320,
            height: 180,
            thumbnail: None,
            media: None,
            gif: true,
        };
        s.insert_messages(&[m]).unwrap();
        s.set_thumb("c", "v", &[1, 2, 3]).unwrap();
        // An inline thumbnail that is already there is never replaced.
        s.set_thumb("c", "v", &[9]).unwrap();
        let got = s.message("c", "v").unwrap().unwrap();
        assert!(matches!(
            got.content,
            Content::Video { thumbnail: Some(ref t), gif: true, .. } if t == &[1, 2, 3]
        ));
    }

    #[test]
    fn media_counts_and_galleries_per_chat() {
        let s = Store::open_in_memory().unwrap();
        let mut img = msg("c", "i", 10, false, "");
        img.content = Content::Image {
            caption: None,
            width: 1,
            height: 1,
            thumbnail: Some(vec![1]),
            media: None,
        };
        let mut doc = msg("c", "d", 11, false, "");
        doc.content = Content::Document {
            file_name: "a.pdf".into(),
            mimetype: "application/pdf".into(),
            size: 1,
            pages: None,
            caption: None,
            media: None,
        };
        let text = msg("c", "t", 12, false, "hi");
        let other = msg("o", "x", 13, false, "hi");
        s.insert_messages(&[img, doc, text, other]).unwrap();
        assert_eq!(s.media_counts("c").unwrap(), (1, 0, 1));
        assert_eq!(s.media_counts("none").unwrap(), (0, 0, 0));
        let g = s.messages_of_kind("c", &["image", "video"], 10).unwrap();
        assert_eq!(g.len(), 1);
        assert!(matches!(
            g[0].content,
            Content::Image {
                thumbnail: Some(_),
                ..
            }
        ));
        assert_eq!(
            s.messages_of_kind("c", &["document"], 10).unwrap()[0].id,
            "d"
        );
    }

    #[test]
    fn migrations_set_version() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.schema_version().unwrap(), MIGRATIONS.len() as i64);
    }

    #[test]
    fn insert_is_idempotent_and_creates_chat() {
        let s = Store::open_in_memory().unwrap();
        let m = msg("a@s.whatsapp.net", "1", 10, false, "hi");
        assert_eq!(
            s.insert_messages(std::slice::from_ref(&m)).unwrap(),
            vec!["1"]
        );
        assert!(s.insert_messages(&[m]).unwrap().is_empty());
        let rows = s.chat_rows().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].meta.last_ts, 10);
        assert_eq!(
            rows[0].last.as_ref().unwrap().content,
            Content::Text("hi".into())
        );
    }

    #[test]
    fn paging_returns_oldest_first() {
        let s = Store::open_in_memory().unwrap();
        let msgs: Vec<_> = (0..10)
            .map(|i| msg("c", &format!("{i:02}"), 100 + i, i % 2 == 0, "x"))
            .collect();
        s.insert_messages(&msgs).unwrap();
        let last3 = s.load_messages("c", None, 3).unwrap();
        assert_eq!(
            last3.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["07", "08", "09"]
        );
        let before = s.load_messages("c", Some((107, "07")), 3).unwrap();
        assert_eq!(
            before.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["04", "05", "06"]
        );
    }

    #[test]
    fn receipts_are_monotonic_in_store() {
        let s = Store::open_in_memory().unwrap();
        s.insert_messages(&[msg("c", "1", 1, true, "x")]).unwrap();
        let ch = s
            .apply_receipt("c", &["1".into(), "missing".into()], ReceiptStatus::Read)
            .unwrap();
        assert_eq!(ch, vec![("1".to_string(), ReceiptStatus::Read)]);
        assert!(
            s.apply_receipt("c", &["1".into()], ReceiptStatus::Delivered)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            s.message("c", "1").unwrap().unwrap().status,
            ReceiptStatus::Read
        );
        // Re-inserting an older copy (history sync) does not downgrade.
        s.insert_messages(&[msg("c", "1", 1, true, "x")]).unwrap();
        assert_eq!(
            s.message("c", "1").unwrap().unwrap().status,
            ReceiptStatus::Read
        );
    }

    #[test]
    fn deletion_replaces_content() {
        let s = Store::open_in_memory().unwrap();
        s.insert_messages(&[msg("c", "1", 1, false, "oops")])
            .unwrap();
        let mut del = msg("c", "1", 1, false, "");
        del.content = Content::Deleted;
        s.insert_messages(&[del]).unwrap();
        assert_eq!(
            s.message("c", "1").unwrap().unwrap().content,
            Content::Deleted
        );
    }

    #[test]
    fn revokes_only_apply_to_the_revokers_own_messages() {
        let s = Store::open_in_memory().unwrap();
        let mut orig = msg("g@g.us", "1", 1, false, "hello");
        orig.sender = "alice@s.whatsapp.net".into();
        s.insert_messages(&[orig]).unwrap();
        // Bob (e.g. a group admin) cannot delete Alice's message.
        let mut bob = msg("g@g.us", "1", 2, false, "");
        bob.sender = "bob@s.whatsapp.net".into();
        bob.content = Content::Deleted;
        s.insert_messages(&[bob]).unwrap();
        assert_eq!(
            s.message("g@g.us", "1").unwrap().unwrap().content,
            Content::Text("hello".into())
        );
        // A revoke claiming to be ours cannot delete someone else's message.
        let mut me = msg("g@g.us", "1", 2, true, "");
        me.content = Content::Deleted;
        s.insert_messages(&[me]).unwrap();
        assert_eq!(
            s.message("g@g.us", "1").unwrap().unwrap().content,
            Content::Text("hello".into())
        );
        // Alice can.
        let mut alice = msg("g@g.us", "1", 2, false, "");
        alice.sender = "alice@s.whatsapp.net".into();
        alice.content = Content::Deleted;
        s.insert_messages(&[alice]).unwrap();
        assert_eq!(
            s.message("g@g.us", "1").unwrap().unwrap().content,
            Content::Deleted
        );
        // Our own message deleted from another of our devices.
        s.insert_messages(&[msg("c", "m", 1, true, "oops")])
            .unwrap();
        let mut del = msg("c", "m", 2, true, "");
        del.content = Content::Deleted;
        s.insert_messages(&[del.clone()]).unwrap();
        assert_eq!(
            s.message("c", "m").unwrap().unwrap().content,
            Content::Deleted
        );
        // Unknown target: nothing inserted.
        del.id = "never-seen".into();
        assert!(s.insert_messages(&[del]).unwrap().is_empty());
        assert!(s.message("c", "never-seen").unwrap().is_none());
    }

    #[test]
    fn expired_messages_are_pruned_and_lid_map_persists() {
        let s = Store::open_in_memory().unwrap();
        let mut m = msg("c", "e", 10, false, "self-destruct");
        m.expires_at = Some(100);
        s.insert_messages(&[m, msg("c", "k", 11, false, "keep")])
            .unwrap();
        assert_eq!(s.message("c", "e").unwrap().unwrap().expires_at, Some(100));
        assert_eq!(s.prune_expired(99).unwrap(), 0);
        assert_eq!(s.prune_expired(100).unwrap(), 1);
        assert!(s.message("c", "e").unwrap().is_none());
        assert!(s.message("c", "k").unwrap().is_some());
        s.set_lid_pairs(&[("1@lid".into(), "34600@s.whatsapp.net".into())])
            .unwrap();
        assert_eq!(s.lid_pairs().unwrap().len(), 1);
    }

    #[test]
    fn chat_merge_keeps_names_and_orders_pinned_first() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_chats(&[
            ChatMeta {
                id: "a".into(),
                name: Some("Ana".into()),
                last_ts: 50,
                ..Default::default()
            },
            ChatMeta {
                id: "b".into(),
                name: None,
                last_ts: 10,
                pinned: true,
                ..Default::default()
            },
        ])
        .unwrap();
        s.upsert_chats(&[ChatMeta {
            id: "a".into(),
            name: None,
            last_ts: 20,
            unread: 3,
            ..Default::default()
        }])
        .unwrap();
        let rows = s.chat_rows().unwrap();
        assert_eq!(rows[0].meta.id, "b");
        assert_eq!(rows[1].meta.name.as_deref(), Some("Ana"));
        assert_eq!(rows[1].meta.last_ts, 50);
        assert_eq!(rows[1].meta.unread, 3);
    }

    #[test]
    fn image_thumbnail_is_stored_out_of_band() {
        let s = Store::open_in_memory().unwrap();
        let mut m = msg("c", "img", 1, false, "");
        m.content = Content::Image {
            caption: Some("cap".into()),
            width: 10,
            height: 20,
            thumbnail: Some(vec![1, 2, 3]),
            media: None,
        };
        s.insert_messages(std::slice::from_ref(&m)).unwrap();
        assert_eq!(s.message("c", "img").unwrap().unwrap(), m);
    }

    #[test]
    fn contacts_kv_and_clear() {
        let s = Store::open_in_memory().unwrap();
        s.set_contact_names(&[("a".into(), NameSource::Push, "Ana".into())])
            .unwrap();
        s.set_kv("own_jid", "me").unwrap();
        assert_eq!(
            s.contact_names().unwrap(),
            vec![("a".to_string(), NameSource::Push, "Ana".to_string())]
        );
        assert_eq!(s.kv("own_jid").unwrap().as_deref(), Some("me"));
        s.save_list(&ListDef {
            id: "1".into(),
            name: "Family".into(),
            kind: ListKind::Custom,
            ..Default::default()
        })
        .unwrap();
        s.clear().unwrap();
        assert!(s.contact_names().unwrap().is_empty());
        assert!(s.kv("own_jid").unwrap().is_none());
        assert!(s.load_lists().unwrap().0.is_empty());
    }

    #[test]
    fn contact_names_keep_one_row_per_source() {
        let s = Store::open_in_memory().unwrap();
        s.set_contact_names(&[
            ("a".into(), NameSource::Push, "Anita".into()),
            ("a".into(), NameSource::Saved, "Ana".into()),
            ("a".into(), NameSource::Saved, "Ana G".into()),
            ("a".into(), NameSource::Business, "Ana Shop".into()),
        ])
        .unwrap();
        let mut rows = s.contact_names().unwrap();
        rows.sort_by_key(|(_, src, _)| *src);
        assert_eq!(
            rows,
            vec![
                ("a".into(), NameSource::Push, "Anita".into()),
                ("a".into(), NameSource::Business, "Ana Shop".into()),
                ("a".into(), NameSource::Saved, "Ana G".into()),
            ]
        );
        // An empty saved name removes it; an empty push name is ignored.
        s.set_contact_names(&[
            ("a".into(), NameSource::Saved, String::new()),
            ("a".into(), NameSource::Push, String::new()),
        ])
        .unwrap();
        assert_eq!(s.contact_names().unwrap().len(), 2);
    }

    #[test]
    fn migration_4_keeps_old_contact_names_as_push_names() {
        let conn = Connection::open_in_memory().unwrap();
        for (i, sql) in MIGRATIONS.iter().take(3).enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", i as i64 + 1)
                .unwrap();
        }
        conn.execute(
            "INSERT INTO contacts (jid, name) VALUES ('a', 'Old Name')",
            [],
        )
        .unwrap();
        let s = Store::init(conn).unwrap();
        assert_eq!(s.schema_version().unwrap(), MIGRATIONS.len() as i64);
        assert_eq!(
            s.contact_names().unwrap(),
            vec![("a".to_string(), NameSource::Push, "Old Name".to_string())]
        );
    }

    #[test]
    fn chat_flags_create_and_update_rows() {
        let s = Store::open_in_memory().unwrap();
        s.set_chat_flag("a", ChatFlag::Archived(true)).unwrap();
        s.set_chat_flag("a", ChatFlag::Pinned(true)).unwrap();
        s.set_chat_flag("a", ChatFlag::Muted(1_900_000_000))
            .unwrap();
        let row = s.chat_rows().unwrap().remove(0);
        assert!(row.meta.archived && row.meta.pinned);
        assert_eq!(row.meta.mute_end, 1_900_000_000);
        s.set_chat_flag("a", ChatFlag::Archived(false)).unwrap();
        assert!(!s.chat_rows().unwrap()[0].meta.archived);
        assert!(s.chat_rows().unwrap()[0].meta.pinned);
    }

    #[test]
    fn forwarded_mark_round_trips_and_old_rows_read_as_not_forwarded() {
        let conn = Connection::open_in_memory().unwrap();
        for (i, sql) in MIGRATIONS.iter().take(7).enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", i as i64 + 1)
                .unwrap();
        }
        conn.execute_batch(
            "INSERT INTO messages (chat_id, id, sender, from_me, ts, kind, content, status)
             VALUES ('c', 'old', 'c', 0, 1, 'text', '{\"Text\":\"hi\"}', 3);",
        )
        .unwrap();
        let s = Store::init(conn).unwrap();
        assert_eq!(s.schema_version().unwrap(), MIGRATIONS.len() as i64);
        let mut m = msg("c", "new", 2, false, "fwd");
        m.forwarded = Some(4);
        s.insert_messages(&[m]).unwrap();
        let page = s.load_messages("c", None, 10).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].forwarded, None);
        assert_eq!(page[1].forwarded, Some(4));
        assert_eq!(s.message("c", "new").unwrap().unwrap().forwarded, Some(4));
        let row = s.chat_rows().unwrap().remove(0);
        assert_eq!(row.last.unwrap().forwarded, Some(4));
    }

    #[test]
    fn migration_7_keeps_old_mutes_as_always() {
        let conn = Connection::open_in_memory().unwrap();
        for (i, sql) in MIGRATIONS.iter().take(6).enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", i as i64 + 1)
                .unwrap();
        }
        conn.execute_batch(
            "INSERT INTO chats (id, muted) VALUES ('m', 1);
             INSERT INTO chats (id, muted) VALUES ('u', 0);",
        )
        .unwrap();
        let s = Store::init(conn).unwrap();
        let rows = s.chat_rows().unwrap();
        let end = |id: &str| rows.iter().find(|r| r.meta.id == id).unwrap().meta.mute_end;
        assert_eq!(end("m"), crate::net::MUTE_FOREVER);
        assert_eq!(end("u"), crate::net::MUTE_OFF);
    }

    #[test]
    fn clear_chat_keeps_the_chat_and_optionally_the_starred() {
        let s = Store::open_in_memory().unwrap();
        s.insert_messages(&[
            msg("a", "1", 10, false, "one"),
            msg("a", "2", 20, true, "two"),
            msg("b", "3", 30, false, "other chat"),
        ])
        .unwrap();
        s.set_unread("a", 2).unwrap();
        s.set_star("a", "1", true, 50).unwrap();
        s.apply_reaction("a", "2", "x@s.whatsapp.net", "👍", 40)
            .unwrap();
        s.clear_chat("a", true).unwrap();
        let left: Vec<_> = s
            .load_messages("a", None, 50)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(left, vec!["1".to_string()], "the starred one stays");
        assert!(s.is_starred("a", "1").unwrap());
        let a = s.chat_rows().unwrap();
        let a = a.iter().find(|r| r.meta.id == "a").unwrap();
        assert_eq!(a.meta.unread, 0);
        s.clear_chat("a", false).unwrap();
        assert!(s.load_messages("a", None, 50).unwrap().is_empty());
        assert!(!s.is_starred("a", "1").unwrap());
        assert!(s.chat_rows().unwrap().iter().any(|r| r.meta.id == "a"));
        assert_eq!(s.load_messages("b", None, 50).unwrap().len(), 1);
    }

    #[test]
    fn delete_chat_drops_the_row_and_its_messages() {
        let s = Store::open_in_memory().unwrap();
        s.insert_messages(&[
            msg("a", "1", 10, false, "one"),
            msg("b", "2", 20, false, "two"),
        ])
        .unwrap();
        s.set_star("a", "1", true, 50).unwrap();
        s.delete_chat("a").unwrap();
        assert!(s.load_messages("a", None, 50).unwrap().is_empty());
        assert!(!s.is_starred("a", "1").unwrap());
        let ids: Vec<_> = s
            .chat_rows()
            .unwrap()
            .into_iter()
            .map(|r| r.meta.id)
            .collect();
        assert_eq!(ids, vec!["b".to_string()]);
    }

    #[test]
    fn lists_and_members_round_trip() {
        let s = Store::open_in_memory().unwrap();
        let def = ListDef {
            id: "1".into(),
            name: "Family".into(),
            color: 3,
            kind: ListKind::Custom,
            order: 2,
        };
        s.save_list(&def).unwrap();
        s.set_list_member("1", "a", true).unwrap();
        s.set_list_member("1", "a", true).unwrap();
        s.set_list_member("1", "b", true).unwrap();
        s.set_list_member("1", "b", false).unwrap();
        let (defs, members) = s.load_lists().unwrap();
        assert_eq!(defs, vec![def.clone()]);
        assert_eq!(members, vec![("1".to_string(), "a".to_string())]);
        // Rename keeps the members; deleting removes both.
        s.save_list(&ListDef {
            name: "Fam".into(),
            ..def
        })
        .unwrap();
        assert_eq!(s.load_lists().unwrap().0[0].name, "Fam");
        s.delete_list("1").unwrap();
        let (defs, members) = s.load_lists().unwrap();
        assert!(defs.is_empty() && members.is_empty());
    }

    #[test]
    fn link_preview_thumbnail_is_stored_and_restored() {
        use crate::net::types::LinkPreview;
        let s = Store::open_in_memory().unwrap();
        let m = Message {
            id: "l1".into(),
            chat: "c".into(),
            sender: "x".into(),
            sender_name: None,
            from_me: false,
            timestamp: 10,
            content: Content::Link {
                text: "see https://example.com".into(),
                preview: LinkPreview {
                    url: "https://example.com".into(),
                    title: "Example".into(),
                    description: "d".into(),
                    thumbnail: Some(vec![9, 8, 7]),
                },
            },
            status: ReceiptStatus::Read,
            expires_at: None,
            reply: None,
            edited: false,
            forwarded: None,
            mentions: Vec::new(),
            reactions: Vec::new(),
        };
        s.insert_messages(std::slice::from_ref(&m)).unwrap();
        let back = s.message("c", "l1").unwrap().unwrap();
        assert_eq!(back, m);
        // Links are searchable by their text.
        assert_eq!(s.search_messages(None, "example", 10).unwrap().len(), 1);
    }

    #[test]
    fn cache_file_is_plain_sqlite_not_sqlcipher_keyed() {
        // The cache is opened without a key, so it must stay readable by a
        // fresh connection (it is a cache, not a secret store).
        let dir = std::env::temp_dir().join(format!("ferrochat-test-{}", std::process::id()));
        let path = dir.join("cache.sqlite3");
        let s = Store::open(&path).unwrap();
        s.set_kv("k", "v").unwrap();
        drop(s);
        let s2 = Store::open(&path).unwrap();
        assert_eq!(s2.kv("k").unwrap().as_deref(), Some("v"));
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- parity features -------------------------------------------------

    use crate::net::types::{Quote, Reaction};

    fn store_with(msgs: &[Message]) -> Store {
        let s = Store::open_in_memory().unwrap();
        s.insert_messages(msgs).unwrap();
        s
    }

    #[test]
    fn replies_and_edit_flag_roundtrip() {
        let mut m = msg("c", "2", 20, false, "answer");
        m.reply = Some(Quote {
            id: "1".into(),
            sender: "c".into(),
            preview: "question".into(),
        });
        let s = store_with(&[msg("c", "1", 10, true, "question"), m.clone()]);
        let got = s.message("c", "2").unwrap().unwrap();
        assert_eq!(got.reply, m.reply);
        assert!(!got.edited);
        assert_eq!(s.message("c", "1").unwrap().unwrap().reply, None);
    }

    #[test]
    fn reactions_replace_remove_and_load_with_messages() {
        let s = store_with(&[msg("c", "1", 10, true, "hi"), msg("c", "2", 20, true, "yo")]);
        assert!(s.apply_reaction("c", "1", "ana", "👍", 100).unwrap());
        assert!(s.apply_reaction("c", "1", "bob", "❤️", 101).unwrap());
        // Same reactor changes their mind.
        assert!(s.apply_reaction("c", "1", "ana", "😂", 102).unwrap());
        // A stale update (older timestamp) never overwrites a newer one.
        assert!(!s.apply_reaction("c", "1", "ana", "😮", 50).unwrap());
        let m = s.load_messages("c", None, 10).unwrap();
        let r = &m[0].reactions;
        assert_eq!(r.len(), 2);
        assert!(r.iter().any(|x| x.reactor == "ana" && x.emoji == "😂"));
        assert!(m[1].reactions.is_empty());
        // A stale removal is ignored, a fresh one removes.
        assert!(!s.apply_reaction("c", "1", "ana", "", 10).unwrap());
        assert!(s.apply_reaction("c", "1", "ana", "", 200).unwrap());
        assert_eq!(s.message("c", "1").unwrap().unwrap().reactions.len(), 1);
        // Removing something that is not there changes nothing.
        assert!(!s.apply_reaction("c", "1", "zed", "", 300).unwrap());
    }

    #[test]
    fn hostile_reactions_are_rejected() {
        let s = store_with(&[msg("c", "1", 10, true, "hi")]);
        let huge = "x".repeat(MAX_REACTION_BYTES + 1);
        assert!(!s.apply_reaction("c", "1", "ana", &huge, 1).unwrap());
        assert!(!s.apply_reaction("c", "1", "", "👍", 1).unwrap());
        assert!(s.message("c", "1").unwrap().unwrap().reactions.is_empty());
    }

    #[test]
    fn reaction_may_arrive_before_its_message() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.apply_reaction("c", "late", "ana", "👍", 5).unwrap());
        s.insert_messages(&[msg("c", "late", 1, false, "hi")])
            .unwrap();
        assert_eq!(s.message("c", "late").unwrap().unwrap().reactions.len(), 1);
    }

    #[test]
    fn history_reactions_are_persisted_with_the_message() {
        let mut m = msg("c", "1", 10, false, "hi");
        m.reactions = vec![
            Reaction {
                reactor: "ana".into(),
                emoji: "👍".into(),
                ts: 5,
            },
            Reaction {
                reactor: String::new(),
                emoji: "x".into(),
                ts: 5,
            },
        ];
        let s = store_with(&[m]);
        let got = s.message("c", "1").unwrap().unwrap();
        assert_eq!(got.reactions.len(), 1, "anonymous reaction dropped");
    }

    #[test]
    fn edits_apply_only_from_the_original_sender() {
        let s = store_with(&[
            msg("c", "mine", 10, true, "teh"),
            msg("c", "theirs", 11, false, "orig"),
        ]);
        // Someone else cannot edit my message.
        assert!(
            s.apply_edit("c", "mine", "c", false, &Content::Text("x".into()))
                .unwrap()
                .is_none()
        );
        let e = s
            .apply_edit("c", "mine", "me", true, &Content::Text("the".into()))
            .unwrap()
            .unwrap();
        assert_eq!(e.content, Content::Text("the".into()));
        assert!(e.edited);
        // Not even I can edit someone else's message.
        assert!(
            s.apply_edit("c", "theirs", "me", true, &Content::Text("x".into()))
                .unwrap()
                .is_none()
        );
        assert!(
            s.apply_edit("c", "theirs", "other", false, &Content::Text("x".into()))
                .unwrap()
                .is_none()
        );
        // The real sender can.
        assert!(
            s.apply_edit("c", "theirs", "c", false, &Content::Text("new".into()))
                .unwrap()
                .is_some()
        );
        // Unknown ids are ignored.
        assert!(
            s.apply_edit("c", "nope", "c", false, &Content::Text("x".into()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn deleted_messages_cannot_be_edited_back() {
        let s = store_with(&[msg("c", "1", 10, true, "secret")]);
        let mut del = msg("c", "1", 10, true, "");
        del.content = Content::Deleted;
        s.insert_messages(&[del]).unwrap();
        assert!(
            s.apply_edit("c", "1", "me", true, &Content::Text("back".into()))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            s.message("c", "1").unwrap().unwrap().content,
            Content::Deleted
        );
    }

    #[test]
    fn caption_edit_keeps_the_media_reference() {
        let img = Content::Image {
            caption: Some("old".into()),
            width: 1,
            height: 1,
            thumbnail: Some(vec![1]),
            media: Some(crate::net::types::MediaRef {
                direct_path: "/p".into(),
                media_key: vec![1; 32],
                file_sha256: vec![],
                file_enc_sha256: vec![],
                file_length: 1,
                kind: crate::net::types::MediaKind::Image,
                mimetype: "image/jpeg".into(),
            }),
        };
        let mut m = msg("c", "1", 10, true, "");
        m.content = img;
        let s = store_with(&[m]);
        let e = s
            .apply_edit("c", "1", "me", true, &Content::Text("new caption".into()))
            .unwrap()
            .unwrap();
        match e.content {
            Content::Image { caption, media, .. } => {
                assert_eq!(caption.as_deref(), Some("new caption"));
                assert!(media.is_some());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn delete_message_removes_row_reactions_and_receipts() {
        let s = store_with(&[
            msg("c", "1", 10, true, "bye"),
            msg("c", "2", 20, true, "stay"),
        ]);
        s.apply_reaction("c", "1", "ana", "👍", 1).unwrap();
        s.apply_group_receipt("c", &["1".into()], Some(("p", ReceiptStatus::Read)), None)
            .unwrap();
        s.delete_message("c", "1").unwrap();
        assert!(s.message("c", "1").unwrap().is_none());
        assert!(s.receipts_of("c", "1").unwrap().is_empty());
        assert!(s.message("c", "2").unwrap().is_some());
        // The chat keeps working.
        assert_eq!(s.load_messages("c", None, 10).unwrap().len(), 1);
    }

    #[test]
    fn search_is_case_insensitive_scoped_and_bounded() {
        let mut caption = msg("b", "4", 40, false, "");
        caption.content = Content::Document {
            file_name: "Tax Return.PDF".into(),
            mimetype: "application/pdf".into(),
            size: 1,
            pages: None,
            caption: None,
            media: None,
        };
        let s = store_with(&[
            msg("a", "1", 10, false, "Hola Mundo"),
            msg("a", "2", 20, true, "otro HOLA aquí"),
            msg("b", "3", 30, false, "hola desde b"),
            caption,
            msg("a", "5", 50, false, "x"),
        ]);
        let all = s.search_messages(None, "HOLA", 10).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].id, "3", "newest first");
        let a = s.search_messages(Some("a"), "hola", 10).unwrap();
        assert_eq!(
            a.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
            ["2", "1"]
        );
        assert!(a[0].from_me);
        assert_eq!(s.search_messages(None, "tax return", 10).unwrap().len(), 1);
        assert!(
            s.search_messages(None, "h", 10).unwrap().is_empty(),
            "too short"
        );
        assert!(s.search_messages(None, "   ", 10).unwrap().is_empty());
        assert_eq!(s.search_messages(None, "hola", 1).unwrap().len(), 1);
        // SQL metacharacters are data.
        assert!(
            s.search_messages(None, "%'; drop table messages;--", 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(s.load_messages("a", None, 10).unwrap().len(), 3);
    }

    #[test]
    fn search_sees_edits_and_ignores_deleted_text() {
        let s = store_with(&[msg("a", "1", 10, true, "typo here")]);
        s.apply_edit("a", "1", "me", true, &Content::Text("fixed text".into()))
            .unwrap();
        assert!(s.search_messages(None, "typo", 10).unwrap().is_empty());
        assert_eq!(s.search_messages(None, "fixed", 10).unwrap().len(), 1);
        let mut del = msg("a", "1", 10, true, "");
        del.content = Content::Deleted;
        s.insert_messages(&[del]).unwrap();
        assert!(s.search_messages(None, "fixed", 10).unwrap().is_empty());
    }

    #[test]
    fn snippets_are_centered_and_single_line() {
        let text = format!("{} needle {}", "a".repeat(100), "b".repeat(100));
        let sn = snippet_around(&text, "needle");
        assert!(sn.contains("needle"));
        assert!(sn.starts_with('…') && sn.ends_with('…'));
        assert!(sn.chars().count() <= 82);
        assert_eq!(snippet_around("short needle", "needle"), "short needle");
        assert!(!snippet_around("a\nneedle\nb", "needle").contains('\n'));
        assert_eq!(snippet_around("héllo wörld", "wörld"), "héllo wörld");
        // Case is preserved in what is shown.
        assert_eq!(snippet_around("Hola Mundo", "mundo"), "Hola Mundo");
    }

    #[test]
    fn search_backfills_rows_written_before_v3() {
        // A v2 database with one text row and no `search` column.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("schema.sql")).unwrap();
        conn.execute_batch(include_str!("migration_2.sql")).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute(
            "INSERT INTO messages (chat_id, id, sender, from_me, ts, kind, content, status)
             VALUES ('c', '1', 's', 0, 5, 'text', ?1, 3)",
            params![serde_json::to_string(&Content::Text("Legacy Body".into())).unwrap()],
        )
        .unwrap();
        let s = Store::init(conn).unwrap();
        assert_eq!(s.schema_version().unwrap(), MIGRATIONS.len() as i64);
        assert_eq!(s.search_messages(None, "legacy", 5).unwrap().len(), 1);
        let m = s.message("c", "1").unwrap().unwrap();
        assert!(!m.edited && m.reply.is_none() && m.reactions.is_empty());
    }

    #[test]
    fn load_around_and_newer_page_through_history() {
        let msgs: Vec<Message> = (1..=100)
            .map(|i| msg("c", &format!("{i:03}"), i, false, &format!("m{i}")))
            .collect();
        let s = store_with(&msgs);
        let w = s.load_around("c", 50, "050", 5, 5).unwrap();
        let ids: Vec<_> = w.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids.first(), Some(&"045"));
        assert_eq!(ids.last(), Some(&"055"));
        assert_eq!(ids.len(), 11);
        assert!(w.has_older && w.has_newer);
        // At the very end nothing is newer; at the start nothing is older.
        let end = s.load_around("c", 100, "100", 5, 5).unwrap();
        assert!(!end.has_newer && end.has_older);
        let start = s.load_around("c", 1, "001", 5, 5).unwrap();
        assert!(!start.has_older && start.has_newer);
        let (page, more) = s.load_newer("c", (55, "055"), 20).unwrap();
        assert_eq!(page.len(), 20);
        assert_eq!(page[0].id, "056");
        assert!(more);
        let (tail, more) = s.load_newer("c", (95, "095"), 20).unwrap();
        assert_eq!(tail.len(), 5);
        assert!(!more);
    }

    #[test]
    fn group_ticks_turn_blue_only_when_everyone_read() {
        let s = store_with(&[msg("g@g.us", "m", 10, true, "hi")]);
        let ids = vec!["m".to_string()];
        let apply = |p: &str, st: ReceiptStatus, size: Option<usize>| {
            s.apply_group_receipt("g@g.us", &ids, Some((p, st)), size)
                .unwrap()
        };
        // 3 other participants. One delivery: still a single tick.
        assert!(apply("a", ReceiptStatus::Delivered, Some(3)).is_empty());
        assert!(apply("b", ReceiptStatus::Delivered, Some(3)).is_empty());
        // Everyone received it: two grey ticks.
        assert_eq!(
            apply("c", ReceiptStatus::Delivered, Some(3)),
            vec![("m".to_string(), ReceiptStatus::Delivered)]
        );
        // Two readers: still grey.
        assert!(apply("a", ReceiptStatus::Read, Some(3)).is_empty());
        assert!(apply("b", ReceiptStatus::Read, Some(3)).is_empty());
        // The last one reads: blue.
        assert_eq!(
            apply("c", ReceiptStatus::Read, Some(3)),
            vec![("m".to_string(), ReceiptStatus::Read)]
        );
        // A late duplicate delivery never downgrades.
        assert!(apply("a", ReceiptStatus::Delivered, Some(3)).is_empty());
        assert_eq!(
            s.message("g@g.us", "m").unwrap().unwrap().status,
            ReceiptStatus::Read
        );
        assert_eq!(s.receipts_of("g@g.us", "m").unwrap().len(), 3);
    }

    #[test]
    fn group_ticks_wait_for_a_known_group_size() {
        let s = store_with(&[msg("g@g.us", "m", 10, true, "hi")]);
        let ids = vec!["m".to_string()];
        assert!(
            s.apply_group_receipt("g@g.us", &ids, Some(("a", ReceiptStatus::Read)), None)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            s.message("g@g.us", "m").unwrap().unwrap().status,
            ReceiptStatus::Sent
        );
        // Once the size is known, the stored receipts are reconciled.
        let changed = s
            .apply_group_receipt("g@g.us", &ids, None, Some(1))
            .unwrap();
        assert_eq!(changed, vec![("m".to_string(), ReceiptStatus::Read)]);
    }

    #[test]
    fn group_receipts_for_other_peoples_messages_are_ignored() {
        let s = store_with(&[msg("g@g.us", "in", 10, false, "hi")]);
        let ids = vec!["in".to_string()];
        assert!(
            s.apply_group_receipt("g@g.us", &ids, Some(("a", ReceiptStatus::Read)), Some(1))
                .unwrap()
                .is_empty()
        );
        assert!(s.receipts_of("g@g.us", "in").unwrap().is_empty());
    }
}
