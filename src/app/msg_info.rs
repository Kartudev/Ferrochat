//! "Message info" for our own messages, like WhatsApp Desktop's panel: when
//! the message was sent, delivered, read (or played); in groups who has read
//! it and who has only received it, with times.
//!
//! Times come from the receipts seen arriving (`receipt_times`, schema 9),
//! so messages from before that (or receipts that arrived while Ferrochat
//! was closed and were never replayed) show their status without a time.

use iced::Task;

use super::message::{Done, Message, Ui};
use super::{App, store_task};
use crate::net::{self, ReceiptStatus};
use crate::store::ReceiptDetail;

/// The open "Message info" panel.
#[derive(Debug, Clone)]
pub struct MsgInfo {
    pub chat: String,
    pub id: String,
    /// `None` while the cache is being read.
    pub rows: Option<Vec<ReceiptDetail>>,
}

/// One line of the panel: a participant (empty in direct chats) and the
/// time to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub participant: String,
    pub at: Option<i64>,
}

/// What the panel shows, derived from the stored receipts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Summary {
    /// Played (voice notes) by, newest first.
    pub played: Vec<Line>,
    /// Read by, newest first (in groups, without those in `played`).
    pub read: Vec<Line>,
    /// Delivered to and not read yet, newest first.
    pub delivered: Vec<Line>,
    /// Group members that have not received it yet, when the size is known.
    pub waiting: Option<usize>,
}

fn newest_first(v: &mut [Line]) {
    v.sort_by(|a, b| {
        b.at.cmp(&a.at)
            .then_with(|| a.participant.cmp(&b.participant))
    });
}

/// Builds the panel's content. Direct chats have at most one row; when it
/// is missing the message's own status still says how far it got (without
/// a time). `group_size` is the number of other members.
pub fn summarize(
    rows: &[ReceiptDetail],
    group: bool,
    status: ReceiptStatus,
    group_size: Option<usize>,
) -> Summary {
    let mut s = Summary::default();
    if !group {
        let row = rows.iter().find(|r| r.participant.is_empty());
        let reached = row.map_or(status, |r| status.advance(r.status));
        let line = |at: Option<i64>| Line {
            participant: String::new(),
            at,
        };
        if reached >= ReceiptStatus::Played {
            s.played.push(line(row.and_then(|r| r.played)));
        }
        if reached >= ReceiptStatus::Read {
            s.read.push(line(row.and_then(|r| r.read.or(r.played))));
        }
        if reached >= ReceiptStatus::Delivered {
            s.delivered
                .push(line(row.and_then(|r| r.delivered.or(r.read).or(r.played))));
        }
        return s;
    }
    for r in rows.iter().filter(|r| !r.participant.is_empty()) {
        let line = |at| Line {
            participant: r.participant.clone(),
            at,
        };
        match r.status {
            ReceiptStatus::Played => s.played.push(line(r.played.or(r.read))),
            ReceiptStatus::Read => s.read.push(line(r.read)),
            ReceiptStatus::Delivered => s.delivered.push(line(r.delivered)),
            _ => {}
        }
    }
    newest_first(&mut s.played);
    newest_first(&mut s.read);
    newest_first(&mut s.delivered);
    s.waiting =
        group_size.map(|n| n.saturating_sub(s.played.len() + s.read.len() + s.delivered.len()));
    s
}

impl App {
    /// Records receipt times of our messages and refreshes an open panel
    /// they concern. `timestamp` 0 (local states) means now.
    pub(super) fn record_receipt_times(
        &mut self,
        chat: &str,
        ids: &[String],
        participant: Option<&str>,
        status: ReceiptStatus,
        timestamp: i64,
    ) -> Task<Message> {
        if !matches!(
            status,
            ReceiptStatus::Delivered | ReceiptStatus::Read | ReceiptStatus::Played
        ) {
            return Task::none();
        }
        let ts = if timestamp > 0 {
            timestamp
        } else {
            chrono::Utc::now().timestamp()
        };
        let participant = participant
            .filter(|_| net::is_group_jid(chat))
            .map(|p| self.lid_map.normalize(p))
            .unwrap_or_default();
        let (c, i) = (chat.to_string(), ids.to_vec());
        let store = store_task(&self.store, move |s| {
            s.record_receipt_times(&c, &i, &participant, status, ts)
        });
        let reload = match &self.msg_info {
            Some(m) if m.chat == chat && ids.contains(&m.id) => self.load_msg_info(),
            _ => Task::none(),
        };
        // The panel reloads after the write (tasks of a chain run in order).
        store.chain(reload)
    }

    fn load_msg_info(&self) -> Task<Message> {
        let Some(m) = &self.msg_info else {
            return Task::none();
        };
        let (store, chat, id) = (self.store.clone(), m.chat.clone(), m.id.clone());
        Task::perform(
            async move {
                let (c, i) = (chat.clone(), id.clone());
                let rows = tokio::task::spawn_blocking(move || {
                    store.receipt_details(&c, &i).unwrap_or_default()
                })
                .await
                .unwrap_or_default();
                (chat, id, rows)
            },
            |(chat, id, rows)| Message::done(Done::MsgInfo { chat, id, rows }),
        )
    }

    /// Handles the panel's messages; `Err(ui)` for everything else.
    pub(super) fn on_ui_msg_info(&mut self, ui: Ui) -> Result<Task<Message>, Ui> {
        Ok(match ui {
            Ui::MessageInfo(id) => {
                self.popup = None;
                let Some(chat) = self
                    .find_message(&id)
                    .filter(|m| m.from_me)
                    .map(|m| m.chat.clone())
                else {
                    return Ok(Task::none());
                };
                self.info = None;
                self.msg_info = Some(MsgInfo {
                    chat,
                    id,
                    rows: None,
                });
                self.load_msg_info()
            }
            Ui::CloseMessageInfo => {
                self.msg_info = None;
                Task::none()
            }
            other => return Err(other),
        })
    }

    /// The panel's rows arrived (dropped when it moved on).
    pub(super) fn on_msg_info(&mut self, chat: String, id: String, rows: Vec<ReceiptDetail>) {
        if let Some(m) = &mut self.msg_info
            && m.chat == chat
            && m.id == id
        {
            m.rows = Some(rows);
        }
    }

    /// The panel's summary, once loaded.
    pub fn msg_info_summary(&self) -> Option<Summary> {
        let m = self.msg_info.as_ref()?;
        let rows = m.rows.as_ref()?;
        let msg = self.find_message(&m.id)?;
        let group = net::is_group_jid(&m.chat);
        Some(summarize(
            rows,
            group,
            msg.status,
            group
                .then(|| self.group_sizes.get(&m.chat).copied())
                .flatten(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ReceiptStatus::*;

    fn row(p: &str, status: ReceiptStatus, d: Option<i64>, r: Option<i64>) -> ReceiptDetail {
        ReceiptDetail {
            participant: p.into(),
            status,
            delivered: d,
            read: r,
            played: None,
        }
    }

    #[test]
    fn direct_chats_show_each_step_with_its_time() {
        let s = summarize(&[row("", Read, Some(10), Some(20))], false, Read, None);
        assert_eq!(s.read[0].at, Some(20));
        assert_eq!(s.delivered[0].at, Some(10));
        assert!(s.played.is_empty());
        // Only delivered so far.
        let s = summarize(
            &[row("", Delivered, Some(10), None)],
            false,
            Delivered,
            None,
        );
        assert!(s.read.is_empty());
        assert_eq!(s.delivered[0].at, Some(10));
        // Read with no delivered receipt seen: delivered at the read time.
        let s = summarize(&[row("", Read, None, Some(30))], false, Read, None);
        assert_eq!(s.delivered[0].at, Some(30));
    }

    #[test]
    fn old_messages_show_their_status_without_times() {
        let s = summarize(&[], false, Read, None);
        assert_eq!(
            s.read,
            vec![Line {
                participant: String::new(),
                at: None
            }]
        );
        assert_eq!(s.delivered.len(), 1);
        let s = summarize(&[], false, Sent, None);
        assert_eq!(s, Summary::default());
        let s = summarize(&[], false, Played, None);
        assert_eq!(s.played.len(), 1);
    }

    #[test]
    fn groups_list_readers_then_receivers_newest_first() {
        let rows = vec![
            row("a@s.whatsapp.net", Read, Some(5), Some(50)),
            row("b@s.whatsapp.net", Read, Some(6), Some(70)),
            row("c@s.whatsapp.net", Delivered, Some(8), None),
            // A status without a time (from before schema 9).
            row("d@s.whatsapp.net", Delivered, None, None),
            row("", Read, Some(1), Some(2)),
        ];
        let s = summarize(&rows, true, Delivered, Some(6));
        let who = |v: &[Line]| v.iter().map(|l| l.participant.clone()).collect::<Vec<_>>();
        assert_eq!(who(&s.read), ["b@s.whatsapp.net", "a@s.whatsapp.net"]);
        assert_eq!(who(&s.delivered), ["c@s.whatsapp.net", "d@s.whatsapp.net"]);
        assert_eq!(s.waiting, Some(2));
        assert_eq!(summarize(&rows, true, Delivered, None).waiting, None);
    }
}
