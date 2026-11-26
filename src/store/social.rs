//! Cache of statuses and starred messages.

use rusqlite::{OptionalExtension, params};

use super::{Store, attach_reactions, row_to_message};
use crate::error::{Error, Result};
use crate::net::social::{STATUS_TTL_SECS, StarredMessage, StatusBody, StatusItem};

/// Most status rows kept (oldest go first). Thumbnails are a few kB each.
pub const MAX_STATUS_ROWS: usize = 1500;
/// Most starred messages listed.
pub const MAX_STARRED: usize = 500;

/// A cached status and whether the user has seen it.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredStatus {
    /// The status.
    pub item: StatusItem,
    /// Seen by the user.
    pub viewed: bool,
}

impl Store {
    /// Stores statuses that are still live at `now`. Existing rows keep
    /// their "viewed" flag. Returns the number of new rows.
    pub fn insert_statuses(&self, items: &[StatusItem], now: i64) -> Result<usize> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut added = 0;
        {
            let mut ins = tx.prepare_cached(
                "INSERT OR IGNORE INTO statuses
                    (author, id, ts, push_name, from_me, kind, body, thumb, viewed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
            )?;
            for it in items.iter().filter(|i| i.is_live(now)) {
                let body =
                    serde_json::to_string(&it.body).map_err(|e| Error::Protocol(e.to_string()))?;
                added += ins.execute(params![
                    it.author,
                    it.id,
                    it.timestamp,
                    it.author_name,
                    it.from_me,
                    it.body.kind_tag(),
                    body,
                    it.body.thumbnail(),
                ])?;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// Live statuses, oldest first.
    pub fn load_statuses(&self, now: i64) -> Result<Vec<StoredStatus>> {
        let conn = self.lock()?;
        let mut st = conn.prepare_cached(
            "SELECT author, id, ts, push_name, from_me, body, thumb, viewed
             FROM statuses WHERE ts > ?1 ORDER BY ts ASC, id ASC",
        )?;
        let rows = st
            .query_map(params![now - STATUS_TTL_SECS], |r| {
                let json: String = r.get(5)?;
                let thumb: Option<Vec<u8>> = r.get(6)?;
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, bool>(4)?,
                    json,
                    thumb,
                    r.get::<_, bool>(7)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (author, id, timestamp, author_name, from_me, json, thumb, viewed) in rows {
            // An unreadable body (a future format) is skipped, not fatal.
            let Ok(mut body) = serde_json::from_str::<StatusBody>(&json) else {
                continue;
            };
            match &mut body {
                StatusBody::Image { thumbnail, .. } | StatusBody::Video { thumbnail, .. } => {
                    *thumbnail = thumb;
                }
                StatusBody::Text { .. } => {}
            }
            out.push(StoredStatus {
                item: StatusItem {
                    id,
                    author,
                    author_name,
                    from_me,
                    timestamp,
                    body,
                },
                viewed,
            });
        }
        Ok(out)
    }

    /// Marks statuses of `author` as seen.
    pub fn mark_statuses_viewed(&self, author: &str, ids: &[String]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        {
            let mut up =
                tx.prepare_cached("UPDATE statuses SET viewed = 1 WHERE author = ?1 AND id = ?2")?;
            for id in ids {
                up.execute(params![author, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Removes one status (revoked by its author).
    pub fn delete_status(&self, author: &str, id: &str) -> Result<()> {
        self.lock()?.execute(
            "DELETE FROM statuses WHERE author = ?1 AND id = ?2",
            params![author, id],
        )?;
        Ok(())
    }

    /// Drops expired statuses and everything beyond `max` rows (oldest
    /// first). Returns the number of rows removed.
    pub fn prune_statuses(&self, now: i64, max: usize) -> Result<usize> {
        let conn = self.lock()?;
        let mut n = conn.execute(
            "DELETE FROM statuses WHERE ts <= ?1",
            params![now - STATUS_TTL_SECS],
        )?;
        n += conn.execute(
            "DELETE FROM statuses WHERE rowid IN (
                SELECT rowid FROM statuses ORDER BY ts DESC LIMIT -1 OFFSET ?1)",
            params![max as i64],
        )?;
        Ok(n)
    }

    /// Records or removes a star. Returns true when the state changed.
    pub fn set_star(&self, chat: &str, id: &str, starred: bool, ts: i64) -> Result<bool> {
        let conn = self.lock()?;
        let n = if starred {
            conn.execute(
                "INSERT OR IGNORE INTO starred (chat_id, msg_id, ts) VALUES (?1, ?2, ?3)",
                params![chat, id, ts],
            )?
        } else {
            conn.execute(
                "DELETE FROM starred WHERE chat_id = ?1 AND msg_id = ?2",
                params![chat, id],
            )?
        };
        Ok(n > 0)
    }

    /// Is the message starred?
    pub fn is_starred(&self, chat: &str, id: &str) -> Result<bool> {
        Ok(self
            .lock()?
            .query_row(
                "SELECT 1 FROM starred WHERE chat_id = ?1 AND msg_id = ?2",
                params![chat, id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Ids of the starred messages among `ids` of `chat`.
    pub fn starred_among(&self, chat: &str, ids: &[String]) -> Result<Vec<String>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let json = serde_json::to_string(ids).map_err(|e| Error::Protocol(e.to_string()))?;
        let conn = self.lock()?;
        let mut st = conn.prepare_cached(
            "SELECT msg_id FROM starred
             WHERE chat_id = ?1 AND msg_id IN (SELECT value FROM json_each(?2))",
        )?;
        let v = st
            .query_map(params![chat, json], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    /// Every starred message across chats, most recently starred first.
    /// Stars whose message is not in the cache (deleted, expired) are left
    /// out.
    pub fn starred_messages(&self, limit: usize) -> Result<Vec<StarredMessage>> {
        let limit = limit.clamp(1, MAX_STARRED) as i64;
        let conn = self.lock()?;
        let mut st = conn.prepare_cached(
            "SELECT m.chat_id, m.id, m.sender, m.sender_name, m.from_me, m.ts, m.content, m.thumb,
                    m.status, m.expires_at, m.reply, m.edited, m.mentions, m.forwarded, s.ts
             FROM starred s JOIN messages m ON m.chat_id = s.chat_id AND m.id = s.msg_id
             ORDER BY s.ts DESC, m.ts DESC LIMIT ?1",
        )?;
        let rows = st
            .query_map(params![limit], |r| {
                Ok(StarredMessage {
                    message: row_to_message(r)?,
                    starred_at: r.get(14)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(st);
        let mut rows = rows;
        // Reactions are not needed in the list, but the message type carries
        // them; attach per chat so the rows are complete.
        let mut chats: Vec<String> = rows.iter().map(|s| s.message.chat.clone()).collect();
        chats.sort();
        chats.dedup();
        for chat in chats {
            let mut msgs: Vec<_> = rows
                .iter()
                .filter(|s| s.message.chat == chat)
                .map(|s| s.message.clone())
                .collect();
            attach_reactions(&conn, &chat, &mut msgs)?;
            for m in msgs {
                if let Some(s) = rows
                    .iter_mut()
                    .find(|s| s.message.chat == chat && s.message.id == m.id)
                {
                    s.message.reactions = m.reactions;
                }
            }
        }
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::types::{Content, Message, ReceiptStatus};

    fn item(id: &str, author: &str, ts: i64) -> StatusItem {
        StatusItem {
            id: id.into(),
            author: author.into(),
            author_name: Some("Lu".into()),
            from_me: false,
            timestamp: ts,
            body: StatusBody::Image {
                caption: Some("c".into()),
                width: 1,
                height: 1,
                thumbnail: Some(vec![1, 2, 3]),
                media: None,
            },
        }
    }

    fn msg(chat: &str, id: &str, ts: i64) -> Message {
        Message {
            id: id.into(),
            chat: chat.into(),
            sender: "x@s.whatsapp.net".into(),
            sender_name: None,
            from_me: false,
            timestamp: ts,
            content: Content::Text(format!("body {id}")),
            status: ReceiptStatus::Read,
            expires_at: None,
            reply: None,
            edited: false,
            forwarded: None,
            mentions: Vec::new(),
            reactions: Vec::new(),
        }
    }

    #[test]
    fn statuses_round_trip_with_thumbnail_and_viewed_flag() {
        let s = Store::open_in_memory().unwrap();
        let now = 1_000_000;
        assert_eq!(
            s.insert_statuses(&[item("1", "a", now - 10)], now).unwrap(),
            1
        );
        // Redelivery is idempotent and keeps the viewed flag.
        s.mark_statuses_viewed("a", &["1".into()]).unwrap();
        assert_eq!(
            s.insert_statuses(&[item("1", "a", now - 10)], now).unwrap(),
            0
        );
        let v = s.load_statuses(now).unwrap();
        assert_eq!(v.len(), 1);
        assert!(v[0].viewed);
        assert_eq!(v[0].item.body.thumbnail(), Some(&[1u8, 2, 3][..]));
        assert_eq!(v[0].item.author_name.as_deref(), Some("Lu"));
    }

    #[test]
    fn expired_statuses_are_neither_stored_nor_loaded_and_prune_removes_them() {
        let s = Store::open_in_memory().unwrap();
        let now = 10_000_000;
        let old = now - STATUS_TTL_SECS - 1;
        assert_eq!(s.insert_statuses(&[item("old", "a", old)], now).unwrap(), 0);
        // Stored while live, expired later.
        s.insert_statuses(&[item("soon", "a", now - STATUS_TTL_SECS + 5)], now)
            .unwrap();
        assert_eq!(s.load_statuses(now).unwrap().len(), 1);
        assert!(s.load_statuses(now + 10).unwrap().is_empty());
        assert_eq!(s.prune_statuses(now + 10, 100).unwrap(), 1);
    }

    #[test]
    fn status_rows_are_bounded_oldest_first() {
        let s = Store::open_in_memory().unwrap();
        let now = 1_000_000;
        let items: Vec<_> = (0..10)
            .map(|i| item(&format!("{i}"), "a", now - 100 + i))
            .collect();
        s.insert_statuses(&items, now).unwrap();
        s.prune_statuses(now, 4).unwrap();
        let ids: Vec<_> = s
            .load_statuses(now)
            .unwrap()
            .into_iter()
            .map(|r| r.item.id)
            .collect();
        assert_eq!(ids, ["6", "7", "8", "9"]);
    }

    #[test]
    fn revoked_statuses_are_deleted() {
        let s = Store::open_in_memory().unwrap();
        let now = 1_000_000;
        s.insert_statuses(&[item("1", "a", now - 1), item("2", "a", now - 1)], now)
            .unwrap();
        s.delete_status("a", "1").unwrap();
        let v = s.load_statuses(now).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].item.id, "2");
    }

    #[test]
    fn stars_toggle_and_list_newest_first_across_chats() {
        let s = Store::open_in_memory().unwrap();
        s.insert_messages(&[
            msg("c1", "m1", 10),
            msg("c2", "m2", 20),
            msg("c1", "m3", 30),
        ])
        .unwrap();
        assert!(s.set_star("c1", "m1", true, 100).unwrap());
        assert!(
            !s.set_star("c1", "m1", true, 101).unwrap(),
            "already starred"
        );
        assert!(s.set_star("c2", "m2", true, 200).unwrap());
        let list = s.starred_messages(50).unwrap();
        let ids: Vec<_> = list.iter().map(|m| m.message.id.as_str()).collect();
        assert_eq!(ids, ["m2", "m1"]);
        assert!(s.is_starred("c1", "m1").unwrap());
        assert_eq!(
            s.starred_among("c1", &["m1".into(), "m3".into()]).unwrap(),
            ["m1"]
        );
        assert!(s.set_star("c1", "m1", false, 300).unwrap());
        assert_eq!(s.starred_messages(50).unwrap().len(), 1);
        assert!(!s.is_starred("c1", "m1").unwrap());
    }

    #[test]
    fn stars_of_missing_messages_are_not_listed() {
        let s = Store::open_in_memory().unwrap();
        s.set_star("c1", "ghost", true, 1).unwrap();
        assert!(s.starred_messages(10).unwrap().is_empty());
    }

    #[test]
    fn the_legacy_status_chat_is_dropped_by_the_migration() {
        use crate::store::MIGRATIONS;
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for sql in MIGRATIONS.iter().take(4) {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 4).unwrap();
        conn.execute(
            "INSERT INTO chats (id, last_ts) VALUES ('status@broadcast', 5), ('real@s.whatsapp.net', 5)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (chat_id, id, sender, from_me, ts, kind, content, status, edited)
             VALUES ('status@broadcast', 'x', 'a', 0, 5, 'text', '{}', 0, 0)",
            [],
        )
        .unwrap();
        let s = Store::init(conn).unwrap();
        let rows = s.chat_rows().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].meta.id, "real@s.whatsapp.net");
        assert!(s.message("status@broadcast", "x").unwrap().is_none());
    }
}
