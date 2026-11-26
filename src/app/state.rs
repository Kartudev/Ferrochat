//! Pure state types and reducers (unit-tested without iced).

use std::collections::{HashMap, HashSet, VecDeque};

use crate::net::types::{
    ChatFlag, ChatId, ChatMeta, Content, ListDef, ListKind, Message, NameSource, Reaction,
    ReceiptStatus, fallback_name, is_group_jid, merge_edit,
};
use crate::store::ChatRow;

/// Max messages kept in memory for the open conversation (architecture §3).
pub const WINDOW: usize = 200;

/// Chat list filter chips.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Filter {
    /// Everything.
    #[default]
    All,
    /// Chats with unread messages.
    Unread,
    /// The Favourites list.
    Favourites,
    /// Group chats.
    Groups,
    /// A user-created list (its id).
    List(String),
}

impl Filter {
    /// Chip label of a built-in filter (custom lists use their own name).
    pub fn label(&self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Unread => "Unread",
            Filter::Favourites => "Favourites",
            Filter::Groups => "Groups",
            Filter::List(_) => "List",
        }
    }

    /// The built-in chips in display order.
    pub const BUILTIN: [Filter; 4] = [
        Filter::All,
        Filter::Unread,
        Filter::Favourites,
        Filter::Groups,
    ];
}

/// Reserved list id for favourites kept only on this device (used when the
/// phone has not synced a Favourites list we can write to).
pub const LOCAL_FAVOURITES: &str = "ferrochat:favourites";

/// Contact names by source. Display priority: address book, business
/// verified name, push name.
#[derive(Debug, Clone, Default)]
pub struct NameBook {
    saved: HashMap<String, String>,
    business: HashMap<String, String>,
    push: HashMap<String, String>,
    /// LID ↔ phone-number JID (both directions): an address-book entry may
    /// be keyed by either id while the chat uses the other.
    alias: HashMap<String, String>,
}

/// Where names come from, for the info-level diagnostics. Counts only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NameStats {
    /// Entries per source.
    pub saved: usize,
    /// Business verified names.
    pub business: usize,
    /// Push names.
    pub push: usize,
    /// One-to-one chats in the list.
    pub direct_chats: usize,
    /// ... shown with an address-book name.
    pub chats_saved: usize,
    /// ... with a business name.
    pub chats_business: usize,
    /// ... with a push name ("~ Name").
    pub chats_push: usize,
    /// ... with the name the chat record carries.
    pub chats_chat_name: usize,
    /// ... with the number only.
    pub chats_number: usize,
}

impl NameBook {
    /// Builds from stored `(jid, source, name)` rows.
    pub fn from_rows(rows: Vec<(String, NameSource, String)>) -> Self {
        let mut b = Self::default();
        for (jid, source, name) in rows {
            b.set(&jid, source, &name);
        }
        b
    }

    fn map(&self, source: NameSource) -> &HashMap<String, String> {
        match source {
            NameSource::Saved => &self.saved,
            NameSource::Business => &self.business,
            NameSource::Push => &self.push,
        }
    }

    /// Records a name; an empty name removes it. Returns true if it changed.
    pub fn set(&mut self, jid: &str, source: NameSource, name: &str) -> bool {
        let map = match source {
            NameSource::Saved => &mut self.saved,
            NameSource::Business => &mut self.business,
            NameSource::Push => &mut self.push,
        };
        if name.is_empty() {
            return map.remove(jid).is_some();
        }
        if map.get(jid).is_some_and(|n| n == name) {
            return false;
        }
        map.insert(jid.to_string(), name.to_string());
        true
    }

    /// The name of `jid` from one source.
    pub fn get(&self, jid: &str, source: NameSource) -> Option<&str> {
        self.map(source).get(jid).map(String::as_str)
    }

    /// Records that `lid` and `pn` are the same person.
    pub fn link(&mut self, lid: &str, pn: &str) {
        self.alias.insert(lid.to_string(), pn.to_string());
        self.alias.insert(pn.to_string(), lid.to_string());
    }

    /// The other id (LID or phone-number JID) of the same person, if known.
    pub fn alias_of(&self, jid: &str) -> Option<&str> {
        self.alias.get(jid).map(String::as_str)
    }

    /// Every address-book name as `(jid, name)`.
    pub fn saved(&self) -> impl Iterator<Item = (&str, &str)> {
        self.saved.iter().map(|(j, n)| (j.as_str(), n.as_str()))
    }

    /// The best known name and where it comes from, looking at the other id
    /// of the same person (LID / phone number) too: a saved name under
    /// either id beats a push name under either.
    pub fn best(&self, jid: &str) -> Option<(NameSource, &str)> {
        let other = self.alias.get(jid);
        [NameSource::Saved, NameSource::Business, NameSource::Push]
            .into_iter()
            .find_map(|src| {
                self.get(jid, src)
                    .or_else(|| other.and_then(|o| self.get(o, src)))
                    .map(|n| (src, n))
            })
    }

    /// Number of entries per source `(saved, business, push)`.
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.saved.len(), self.business.len(), self.push.len())
    }
}

/// How a chat or sender is called, with the reason (decides the styling:
/// push names get a "~" like in the official app).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// Saved in the phone's address book (or a group subject).
    Saved(String),
    /// Business verified name.
    Business(String),
    /// The name the person chose for itself.
    Push(String),
    /// Name carried by the chat record only.
    ChatName(String),
    /// Only the number (or "Group") is known.
    Number(String),
}

impl Resolved {
    /// Text to show: push names are prefixed with "~ ".
    pub fn label(&self) -> String {
        match self {
            Resolved::Push(n) => format!("~ {n}"),
            Resolved::Saved(n)
            | Resolved::Business(n)
            | Resolved::ChatName(n)
            | Resolved::Number(n) => n.clone(),
        }
    }

    /// Text without the "~" (typing indicators, notifications, mentions).
    pub fn plain(&self) -> String {
        match self {
            Resolved::Saved(n)
            | Resolved::Business(n)
            | Resolved::Push(n)
            | Resolved::ChatName(n)
            | Resolved::Number(n) => n.clone(),
        }
    }
}

/// One row of the chat list.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatEntry {
    /// Metadata.
    pub meta: ChatMeta,
    /// Newest message.
    pub last: Option<Message>,
}

impl ChatEntry {
    /// True for group chats.
    pub fn is_group(&self) -> bool {
        is_group_jid(&self.meta.id)
    }
}

/// The chat list plus the contact-name directory.
#[derive(Debug, Clone, Default)]
pub struct ChatList {
    /// Rows, sorted pinned-first then by recency. Read-only outside; use the
    /// methods to mutate so the index stays in sync.
    pub entries: Vec<ChatEntry>,
    /// JID → names by source.
    pub names: NameBook,
    /// Lists synced through app state (custom lists, favourites, others).
    pub lists: Vec<ListDef>,
    /// List id → chats in it.
    members: HashMap<String, HashSet<String>>,
    /// JID → position in `entries` (O(1) lookups during history sync).
    index: HashMap<String, usize>,
    /// Chats marked unread by hand (here or on another device) since start:
    /// reading them must tell the other devices too.
    marked_unread: HashSet<String>,
}

impl ChatList {
    /// Builds from cache rows.
    pub fn from_rows(rows: Vec<ChatRow>, names: NameBook) -> Self {
        let mut list = Self {
            entries: rows
                .into_iter()
                .filter(|r| !crate::net::social::is_status_jid(&r.meta.id))
                .map(|r| ChatEntry {
                    meta: r.meta,
                    last: r.last,
                })
                .collect(),
            names,
            lists: Vec::new(),
            members: HashMap::new(),
            index: HashMap::new(),
            marked_unread: HashSet::new(),
        };
        list.sort();
        list
    }

    /// Loads lists and memberships from the cache.
    pub fn load_lists(&mut self, defs: Vec<ListDef>, members: Vec<(String, String)>) {
        self.lists = defs;
        self.sort_lists();
        self.members.clear();
        for (l, c) in members {
            self.members.entry(l).or_default().insert(c);
        }
    }

    fn sort_lists(&mut self) {
        self.lists
            .sort_by(|a, b| a.order.cmp(&b.order).then(a.name.cmp(&b.name)));
    }

    fn reindex(&mut self) {
        self.index.clear();
        for (i, e) in self.entries.iter().enumerate() {
            self.index.insert(e.meta.id.clone(), i);
        }
    }

    /// Re-sorts: pinned first, then newest activity.
    pub fn sort(&mut self) {
        self.entries.sort_by(|a, b| {
            b.meta
                .pinned
                .cmp(&a.meta.pinned)
                .then(b.meta.last_ts.cmp(&a.meta.last_ts))
                .then(a.meta.id.cmp(&b.meta.id))
        });
        self.reindex();
    }

    /// How `jid` is called. Groups use their subject. People: saved name,
    /// then the business verified name, then the push name, then whatever
    /// name the chat record carries, then the formatted number.
    /// `push_hint` is a push name seen on a message, used when the book has
    /// no name yet.
    pub fn resolve(&self, jid: &str, push_hint: Option<&str>) -> Resolved {
        let chat_name = self
            .get(jid)
            .and_then(|e| e.meta.name.clone())
            .filter(|n| !n.is_empty());
        if is_group_jid(jid) {
            return match chat_name {
                Some(n) => Resolved::Saved(n),
                None => Resolved::Number(fallback_name(jid)),
            };
        }
        match self.names.best(jid) {
            Some((NameSource::Saved, n)) => return Resolved::Saved(n.to_string()),
            Some((NameSource::Business, n)) => return Resolved::Business(n.to_string()),
            Some((NameSource::Push, n)) => return Resolved::Push(n.to_string()),
            None => {}
        }
        if let Some(n) = push_hint.filter(|n| !n.is_empty()) {
            return Resolved::Push(n.to_string());
        }
        match chat_name {
            Some(n) => Resolved::ChatName(n),
            None => Resolved::Number(fallback_name(jid)),
        }
    }

    /// The JID an "@<user>" in a message's text refers to: the mentioned
    /// JID with that user part (`contextInfo.mentionedJid`), else a guess
    /// for rows cached before mentions were stored: a LID we know, else the
    /// phone number.
    pub fn mention_target(&self, user: &str, mentions: &[String]) -> String {
        if let Some(j) = mentions.iter().find(|j| j.split('@').next() == Some(user)) {
            return j.clone();
        }
        let lid = format!("{user}@lid");
        if self.names.alias_of(&lid).is_some() || self.names.best(&lid).is_some() {
            return lid;
        }
        format!("{user}@s.whatsapp.net")
    }

    /// How a mention is shown, without the "@": "You", the saved, business
    /// or push name (same order as [`resolve`](Self::resolve), over the LID
    /// and its phone number), else the formatted phone number. A LID nobody
    /// has named and whose number is unknown never shows as digits: it reads
    /// "Unknown".
    pub fn mention_label(&self, user: &str, mentions: &[String], own: &[&str]) -> String {
        let jid = self.mention_target(user, mentions);
        let other = self.names.alias_of(&jid).map(str::to_string);
        if own
            .iter()
            .any(|o| *o == jid || other.as_deref() == Some(*o))
        {
            return "You".into();
        }
        let pn = if jid.ends_with("@lid") {
            other.filter(|o| o.ends_with("@s.whatsapp.net"))
        } else {
            Some(jid.clone())
        };
        match self.resolve(&jid, None) {
            Resolved::Saved(n) | Resolved::Business(n) | Resolved::Push(n) => n,
            Resolved::ChatName(_) | Resolved::Number(_) => match pn {
                Some(pn) => match self.resolve(&pn, None) {
                    Resolved::Saved(n)
                    | Resolved::Business(n)
                    | Resolved::Push(n)
                    | Resolved::ChatName(n) => n,
                    Resolved::Number(_) => fallback_name(&pn),
                },
                None => "Unknown".into(),
            },
        }
    }

    /// `text` with every "@<user>" mention replaced by "@Name" (chat list
    /// preview, notifications, quotes).
    pub fn render_mentions(&self, text: &str, mentions: &[String], own: &[&str]) -> String {
        use crate::app::linkify::{Token, tokenize};
        if !text.contains('@') {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        for tok in tokenize(text) {
            match tok {
                Token::Mention { user, .. } => {
                    out.push('@');
                    out.push_str(&self.mention_label(user, mentions, own));
                }
                Token::Text(t) => out.push_str(t),
                Token::Url { text, .. } | Token::Email { text, .. } => out.push_str(text),
            }
        }
        out
    }

    /// One-line preview of a message with its mentions shown as names.
    pub fn preview(&self, m: &Message, own: &[&str]) -> String {
        self.render_mentions(&m.content.preview(), &m.mentions, own)
    }

    /// Diagnostics: how the one-to-one chats are named right now.
    pub fn name_stats(&self) -> NameStats {
        let (saved, business, push) = self.names.counts();
        let mut st = NameStats {
            saved,
            business,
            push,
            ..NameStats::default()
        };
        for e in &self.entries {
            if e.is_group() || !e.meta.id.contains('@') || e.meta.id.ends_with("@broadcast") {
                continue;
            }
            st.direct_chats += 1;
            match self.resolve(&e.meta.id, None) {
                Resolved::Saved(_) => st.chats_saved += 1,
                Resolved::Business(_) => st.chats_business += 1,
                Resolved::Push(_) => st.chats_push += 1,
                Resolved::ChatName(_) => st.chats_chat_name += 1,
                Resolved::Number(_) => st.chats_number += 1,
            }
        }
        st
    }

    /// Display name for a chat or a sender ("~ Name" for push names).
    pub fn display_name(&self, jid: &str) -> String {
        self.resolve(jid, None).label()
    }

    /// Like [`display_name`](Self::display_name) with a push name seen on
    /// the message as a last resort (group sender labels, previews).
    pub fn sender_name(&self, jid: &str, push_hint: Option<&str>) -> String {
        self.resolve(jid, push_hint).label()
    }

    /// Name without the "~" marker.
    pub fn plain_name(&self, jid: &str) -> String {
        self.resolve(jid, None).plain()
    }

    /// Records a contact name; true when something changed.
    pub fn set_name(&mut self, jid: &str, source: NameSource, name: &str) -> bool {
        self.names.set(jid, source, name)
    }

    /// Learns push names carried by messages. Returns the rows to persist.
    pub fn learn_push_names(&mut self, msgs: &[Message]) -> Vec<(String, NameSource, String)> {
        let mut out = Vec::new();
        for m in msgs {
            if !m.from_me
                && let Some(n) = &m.sender_name
                && self.names.set(&m.sender, NameSource::Push, n)
            {
                out.push((m.sender.clone(), NameSource::Push, n.clone()));
            }
        }
        out
    }

    // ----------------------------------------------------------------- lists

    /// Applies a list created / edited / deleted on the phone (or by us).
    pub fn apply_list_edit(&mut self, def: ListDef, deleted: bool) {
        if deleted {
            self.lists.retain(|l| l.id != def.id);
            self.members.remove(&def.id);
            return;
        }
        match self.lists.iter_mut().find(|l| l.id == def.id) {
            Some(l) => {
                // A partial edit (no name) keeps the known one.
                let name = if def.name.is_empty() {
                    l.name.clone()
                } else {
                    def.name.clone()
                };
                *l = ListDef { name, ..def };
            }
            None => self.lists.push(def),
        }
        self.sort_lists();
    }

    /// Applies a membership change.
    pub fn apply_list_chat(&mut self, list: &str, chat: &str, member: bool) {
        if member {
            self.members
                .entry(list.to_string())
                .or_default()
                .insert(chat.to_string());
        } else if let Some(set) = self.members.get_mut(list) {
            set.remove(chat);
        }
    }

    /// User-created lists in display order.
    pub fn custom_lists(&self) -> impl Iterator<Item = &ListDef> {
        self.lists.iter().filter(|l| l.kind == ListKind::Custom)
    }

    /// The list with this id.
    pub fn list(&self, id: &str) -> Option<&ListDef> {
        self.lists.iter().find(|l| l.id == id)
    }

    /// Is `chat` in list `id`?
    pub fn in_list(&self, id: &str, chat: &str) -> bool {
        self.members.get(id).is_some_and(|s| s.contains(chat))
    }

    /// Id of the synced Favourites list, if the phone has one.
    pub fn favourites_list(&self) -> Option<&str> {
        self.lists
            .iter()
            .find(|l| l.kind == ListKind::Favourites)
            .map(|l| l.id.as_str())
    }

    /// Is `chat` a favourite (synced list or local favourite)?
    pub fn is_favourite(&self, chat: &str) -> bool {
        self.in_list(LOCAL_FAVOURITES, chat)
            || self
                .favourites_list()
                .is_some_and(|id| self.in_list(id, chat))
    }

    /// The list id a favourite toggle writes to: the synced list when it
    /// exists (so the phone sees it), else the local one.
    pub fn favourites_target(&self) -> &str {
        self.favourites_list().unwrap_or(LOCAL_FAVOURITES)
    }

    /// An unused list id: the next integer, like the official client.
    pub fn next_list_id(&self) -> String {
        let max = self
            .lists
            .iter()
            .filter_map(|l| l.id.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        (max + 1).to_string()
    }

    /// Order index for a new list (after all others).
    pub fn next_list_order(&self) -> i32 {
        self.lists
            .iter()
            .map(|l| l.order)
            .max()
            .map_or(0, |m| m + 1)
    }

    /// Archive / pin / mute changed (here or on another device).
    pub fn apply_flag(&mut self, chat: &str, flag: ChatFlag) {
        let e = self.entry_mut(chat);
        match flag {
            ChatFlag::Archived(v) => e.meta.archived = v,
            ChatFlag::Pinned(v) => e.meta.pinned = v,
            ChatFlag::Muted(end) => e.meta.mute_end = end,
        }
        self.sort();
    }

    /// Mutable entry, created if missing.
    fn entry_mut(&mut self, chat: &str) -> &mut ChatEntry {
        if let Some(&i) = self.index.get(chat) {
            return &mut self.entries[i];
        }
        self.index.insert(chat.to_string(), self.entries.len());
        self.entries.push(ChatEntry {
            meta: ChatMeta {
                id: chat.to_string(),
                ..Default::default()
            },
            last: None,
        });
        let last = self.entries.len() - 1;
        &mut self.entries[last]
    }

    /// Finds an entry.
    pub fn get(&self, chat: &str) -> Option<&ChatEntry> {
        self.index.get(chat).and_then(|&i| self.entries.get(i))
    }

    fn get_mut(&mut self, chat: &str) -> Option<&mut ChatEntry> {
        let i = *self.index.get(chat)?;
        self.entries.get_mut(i)
    }

    /// Merges history-sync metadata.
    pub fn apply_history(&mut self, chats: &[ChatMeta], names: &[(String, String)]) {
        for (jid, name) in names {
            self.names.set(jid, NameSource::Push, name);
        }
        for c in chats {
            let e = self.entry_mut(&c.id);
            let name = c.name.clone().or_else(|| e.meta.name.clone());
            let last_ts = e.meta.last_ts.max(c.last_ts);
            e.meta = ChatMeta {
                name,
                last_ts,
                ..c.clone()
            };
        }
        self.sort();
    }

    /// Applies incoming/outgoing messages. Incoming messages whose id is in
    /// `fresh` (newly stored: live or drained from the offline queue) count
    /// as unread unless that chat is open and the window focused. History
    /// passes `None`: it carries its own unread counters.
    pub fn apply_messages(
        &mut self,
        msgs: &[Message],
        open_chat: Option<&str>,
        focused: bool,
        fresh: Option<&HashSet<String>>,
    ) {
        for m in msgs {
            let viewing = open_chat == Some(m.chat.as_str()) && focused;
            let e = self.entry_mut(&m.chat);
            let newer = e
                .last
                .as_ref()
                .is_none_or(|l| (m.timestamp, &m.id) >= (l.timestamp, &l.id));
            let is_fresh = fresh.is_some_and(|f| f.contains(&m.id));
            if is_fresh && !m.from_me && !viewing && !matches!(m.content, Content::Deleted) {
                e.meta.unread += 1;
            }
            if newer {
                e.last = Some(m.clone());
                e.meta.last_ts = e.meta.last_ts.max(m.timestamp);
            } else if let Some(l) = &mut e.last
                && l.id == m.id
            {
                l.content = m.content.clone();
            }
        }
        self.sort();
    }

    /// Receipt for the preview tick.
    pub fn apply_receipt(&mut self, chat: &str, ids: &[String], status: ReceiptStatus) {
        if let Some(e) = self.get_mut(chat)
            && let Some(l) = &mut e.last
            && l.from_me
            && ids.contains(&l.id)
        {
            l.status = l.status.advance(status);
        }
    }

    /// Group aggregate receipts for the preview tick.
    pub fn set_status(&mut self, chat: &str, id: &str, status: ReceiptStatus) {
        if let Some(e) = self.get_mut(chat)
            && let Some(l) = &mut e.last
            && l.from_me
            && l.id == id
        {
            l.status = l.status.advance(status);
        }
    }

    /// An edit changed the newest message of a chat.
    pub fn apply_edit(
        &mut self,
        chat: &str,
        id: &str,
        sender: &str,
        from_me: bool,
        content: &Content,
    ) {
        if let Some(e) = self.get_mut(chat)
            && let Some(l) = &mut e.last
            && l.id == id
            && crate::store::edit_allowed(l.from_me, &l.sender, sender, from_me)
            && let Some(merged) = merge_edit(&l.content, content)
        {
            l.content = merged;
            l.edited = true;
        }
    }

    /// Replaces the preview message (after a local delete).
    pub fn set_last(&mut self, chat: &str, last: Option<Message>) {
        if let Some(e) = self.get_mut(chat) {
            e.last = last;
        }
    }

    /// True when `id` is the newest message of `chat`.
    pub fn is_last(&self, chat: &str, id: &str) -> bool {
        self.get(chat)
            .and_then(|e| e.last.as_ref())
            .is_some_and(|l| l.id == id)
    }

    /// Clears the unread badge.
    /// Clears the badge. True when the chat had been marked unread by hand,
    /// so the other devices must hear that it was read.
    pub fn mark_read(&mut self, chat: &str) -> bool {
        if let Some(e) = self.get_mut(chat) {
            e.meta.unread = 0;
        }
        self.marked_unread.remove(chat)
    }

    /// Sets the badge (restoring it after a failed sync).
    pub fn set_unread(&mut self, chat: &str, unread: u32) {
        if let Some(e) = self.get_mut(chat) {
            e.meta.unread = unread;
        }
    }

    /// "Clear chat": the row stays, without a last message or badge.
    pub fn clear_chat(&mut self, chat: &str) {
        if let Some(e) = self.get_mut(chat) {
            e.meta.unread = 0;
            e.last = None;
        }
        self.marked_unread.remove(chat);
    }

    /// "Delete chat": the row goes. True when it existed.
    pub fn delete_chat(&mut self, chat: &str) -> bool {
        self.marked_unread.remove(chat);
        let Some(i) = self.index.get(chat).copied() else {
            return false;
        };
        self.entries.remove(i);
        self.reindex();
        true
    }

    /// Rows of the main list matching the search query and filter. Archived
    /// chats are hidden unless the query is non-empty (search finds them, as
    /// in the official app).
    pub fn filtered(&self, query: &str, filter: Filter) -> Vec<&ChatEntry> {
        self.filtered_view(query, &filter, false)
    }

    /// The archived list (`archived == true`) or the main list.
    pub fn filtered_view(&self, query: &str, filter: &Filter, archived: bool) -> Vec<&ChatEntry> {
        let q = query.trim().to_lowercase();
        self.entries
            .iter()
            .filter(|e| !q.is_empty() || e.meta.archived == archived)
            .filter(|e| match filter {
                Filter::All => true,
                Filter::Unread => e.meta.unread > 0,
                Filter::Favourites => self.is_favourite(&e.meta.id),
                Filter::Groups => e.is_group(),
                Filter::List(id) => self.in_list(id, &e.meta.id),
            })
            .filter(|e| {
                q.is_empty()
                    || self.display_name(&e.meta.id).to_lowercase().contains(&q)
                    || e.last
                        .as_ref()
                        .is_some_and(|l| l.content.preview().to_lowercase().contains(&q))
            })
            .collect()
    }

    /// Unread chats among the archived ones (badge on the "Archived" row).
    /// The number on the rail's Chats icon: unread chats, like WhatsApp
    /// Desktop (chats, not messages; archived and muted chats and status /
    /// channel pseudo-chats excluded).
    pub fn unread_chats(&self, now: i64) -> usize {
        self.entries
            .iter()
            .filter(|e| {
                e.meta.unread > 0
                    && !e.meta.archived
                    && !e.meta.is_muted(now)
                    && !crate::net::is_pseudo_chat(&e.meta.id)
            })
            .count()
    }

    /// Marks a chat unread by hand, here or on another device (keeps a
    /// real count).
    pub fn mark_unread(&mut self, chat: &str) {
        if let Some(e) = self.get_mut(chat) {
            if e.meta.unread == 0 {
                e.meta.unread = 1;
            }
            self.marked_unread.insert(chat.to_string());
        }
    }

    pub fn archived_unread(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.meta.archived && e.meta.unread > 0)
            .count()
    }

    /// Pinned chats (WhatsApp allows three).
    pub fn pinned_count(&self) -> usize {
        self.entries.iter().filter(|e| e.meta.pinned).count()
    }

    /// Number of archived chats (for the "Archived" row).
    pub fn archived_count(&self) -> usize {
        self.entries.iter().filter(|e| e.meta.archived).count()
    }
}

/// The open conversation's message window.
#[derive(Debug, Clone, Default)]
pub struct Conversation {
    /// Chat id.
    pub chat: ChatId,
    /// Messages, oldest first, at most [`WINDOW`].
    pub messages: VecDeque<Message>,
    /// More history exists in the cache.
    pub has_more: bool,
    /// The window was opened around an old message (search hit, reply jump):
    /// newer messages exist in the cache but are not loaded.
    pub has_newer: bool,
    /// A page load (older or newer) is in flight: scroll events that would
    /// ask again are ignored until it lands.
    pub loading: bool,
    /// The "N unread messages" divider: the first unread message (once
    /// known) and how many there are. Stays until the chat is left.
    pub unread: Option<UnreadDivider>,
}

/// Where the "N unread messages" divider goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadDivider {
    /// First unread message; `None` until the page holding it is loaded.
    pub first: Option<String>,
    /// Unread messages counted.
    pub count: usize,
}

/// Distance (pixels) from the newest end within which the conversation
/// counts as scrolled to the bottom.
pub const BOTTOM_SLACK: f32 = 48.0;

/// Whether the user is looking at the newest messages of the open chat:
/// window focused, the newest page loaded and scrolled to the bottom. Only
/// then does an arriving message count as read (WhatsApp Desktop).
pub fn is_reading(focused: bool, scroll_from_newest: f32, has_newer: bool) -> bool {
    focused && !has_newer && scroll_from_newest <= BOTTOM_SLACK
}

/// The id of the oldest of the `count` newest incoming messages: where the
/// unread divider goes.
pub fn first_unread(msgs: &VecDeque<Message>, count: usize) -> Option<String> {
    if count == 0 {
        return None;
    }
    msgs.iter()
        .rev()
        .filter(|m| !m.from_me)
        .take(count)
        .last()
        .map(|m| m.id.clone())
}

/// The JID to address a receipt to: the one the message came from (a LID
/// for LID-addressed chats and group members), not the phone number it was
/// normalised to for display. Unknown: the JID itself.
pub fn wire_jid<'a>(wire: &'a HashMap<String, String>, jid: &'a str) -> &'a str {
    wire.get(jid).map_or(jid, String::as_str)
}

impl Conversation {
    /// New window from cached messages.
    pub fn new(chat: ChatId, msgs: Vec<Message>, has_more: bool) -> Self {
        let mut c = Self {
            chat,
            messages: VecDeque::new(),
            has_more,
            has_newer: false,
            loading: false,
            unread: None,
        };
        c.merge(msgs);
        c
    }

    fn position(&self, id: &str) -> Option<usize> {
        self.messages.iter().position(|m| m.id == id)
    }

    /// Inserts messages in timestamp order, merging duplicates (status via
    /// the receipt state machine, deletions replace content). Trims the
    /// oldest when over [`WINDOW`].
    pub fn merge(&mut self, msgs: Vec<Message>) {
        for m in msgs {
            if m.chat != self.chat {
                continue;
            }
            if let Some(i) = self.position(&m.id) {
                let cur = &mut self.messages[i];
                cur.status = cur.status.advance(m.status);
                let deleted = matches!(m.content, Content::Deleted)
                    && crate::store::revoke_allowed(cur.from_me, &cur.sender, &m);
                let gained_media = cur.content.media().is_none() && m.content.media().is_some();
                if deleted || gained_media {
                    cur.content = m.content;
                }
                if cur.reactions.is_empty() && !m.reactions.is_empty() {
                    cur.reactions = m.reactions;
                }
                continue;
            }
            if matches!(m.content, Content::Deleted) {
                // Revoke for a message outside the window / never seen.
                continue;
            }
            let idx = self
                .messages
                .iter()
                .rposition(|x| (x.timestamp, &x.id) <= (m.timestamp, &m.id))
                .map(|i| i + 1)
                .unwrap_or(0);
            self.messages.insert(idx, m);
        }
        while self.messages.len() > WINDOW {
            self.messages.pop_front();
            self.has_more = true;
        }
    }

    /// Prepends an older page.
    pub fn prepend(&mut self, older: Vec<Message>, has_more: bool) {
        for m in older.into_iter().rev() {
            if self.position(&m.id).is_none() {
                self.messages.push_front(m);
            }
        }
        while self.messages.len() > WINDOW {
            self.messages.pop_back();
            // The newest left the window: scrolling back down reloads them.
            self.has_newer = true;
        }
        self.has_more = has_more;
        self.loading = false;
    }

    /// Applies a receipt to outgoing messages.
    pub fn apply_receipt(&mut self, ids: &[String], status: ReceiptStatus) -> bool {
        let mut changed = false;
        for m in self
            .messages
            .iter_mut()
            .filter(|m| m.from_me && ids.contains(&m.id))
        {
            let next = m.status.advance(status);
            changed |= next != m.status;
            m.status = next;
        }
        changed
    }

    /// Applies a reaction (empty emoji removes it). Newer wins.
    pub fn apply_reaction(&mut self, target: &str, reactor: &str, emoji: &str, ts: i64) -> bool {
        let Some(i) = self.position(target) else {
            return false;
        };
        apply_reaction(&mut self.messages[i].reactions, reactor, emoji, ts)
    }

    /// Applies an edit if `sender` is the original sender. Returns true
    /// when the message changed.
    pub fn apply_edit(&mut self, id: &str, sender: &str, from_me: bool, content: &Content) -> bool {
        let Some(i) = self.position(id) else {
            return false;
        };
        let cur = &mut self.messages[i];
        let allowed = crate::store::edit_allowed(cur.from_me, &cur.sender, sender, from_me);
        if !allowed || matches!(cur.content, Content::Deleted) {
            return false;
        }
        match merge_edit(&cur.content, content) {
            Some(c) => {
                cur.content = c;
                cur.edited = true;
                true
            }
            None => false,
        }
    }

    /// Removes a message ("delete for me").
    pub fn remove(&mut self, id: &str) -> bool {
        match self.position(id) {
            Some(i) => {
                self.messages.remove(i);
                true
            }
            None => false,
        }
    }

    /// Sets an aggregate status (groups).
    pub fn set_status(&mut self, id: &str, status: ReceiptStatus) {
        if let Some(i) = self.position(id) {
            let m = &mut self.messages[i];
            m.status = m.status.advance(status);
        }
    }

    /// Appends a newer page (after jumping into the middle of history).
    pub fn append_newer(&mut self, newer: Vec<Message>, has_newer: bool) {
        for m in newer {
            if self.position(&m.id).is_none() {
                self.messages.push_back(m);
            }
        }
        while self.messages.len() > WINDOW {
            self.messages.pop_front();
            self.has_more = true;
        }
        self.has_newer = has_newer;
        self.loading = false;
    }

    /// Replaces the window with one around an anchor.
    pub fn replace_window(&mut self, msgs: Vec<Message>, has_more: bool, has_newer: bool) {
        self.messages = msgs.into();
        self.has_more = has_more;
        self.has_newer = has_newer;
    }

    /// Replaces the content of a message (upload finished).
    pub fn set_content(&mut self, id: &str, content: Content) {
        if let Some(i) = self.position(id) {
            self.messages[i].content = content;
        }
    }

    /// Finds a message.
    pub fn get(&self, id: &str) -> Option<&Message> {
        self.messages.iter().find(|m| m.id == id)
    }

    /// Cursor for paging older messages.
    pub fn oldest_cursor(&self) -> Option<(i64, String)> {
        self.messages.front().map(|m| (m.timestamp, m.id.clone()))
    }
}

/// Applies one reactor's reaction to a reaction list: one reaction per
/// reactor, an empty emoji removes it, an older timestamp never wins.
pub fn apply_reaction(list: &mut Vec<Reaction>, reactor: &str, emoji: &str, ts: i64) -> bool {
    let pos = list.iter().position(|r| r.reactor == reactor);
    if let Some(i) = pos
        && list[i].ts > ts
    {
        return false;
    }
    if emoji.is_empty() {
        return match pos {
            Some(i) => {
                list.remove(i);
                true
            }
            None => false,
        };
    }
    match pos {
        Some(i) => {
            let changed = list[i].emoji != emoji;
            list[i].emoji = emoji.to_string();
            list[i].ts = ts;
            changed
        }
        None => {
            list.push(Reaction {
                reactor: reactor.to_string(),
                emoji: emoji.to_string(),
                ts,
            });
            true
        }
    }
}

/// Reaction badge contents: distinct emoji with their counts, most used
/// first (ties keep first-seen order), plus the total.
pub fn reaction_summary(list: &[Reaction]) -> (Vec<(String, usize)>, usize) {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for r in list {
        match counts.iter_mut().find(|(e, _)| *e == r.emoji) {
            Some((_, n)) => *n += 1,
            None => counts.push((r.emoji.clone(), 1)),
        }
    }
    counts.sort_by_key(|c| std::cmp::Reverse(c.1));
    (counts, list.len())
}

/// Tick of a group message from per-participant receipts: blue only once
/// every other participant read it, grey double ticks once all received it.
/// `None` when nothing should change yet (or the group size is unknown/zero).
pub fn group_status(size: usize, delivered: usize, read: usize) -> Option<ReceiptStatus> {
    if size == 0 {
        return None;
    }
    if read >= size {
        Some(ReceiptStatus::Read)
    } else if delivered.max(read) >= size {
        Some(ReceiptStatus::Delivered)
    } else {
        None
    }
}

/// Groups incoming message ids per sender for read receipts (groups need
/// the participant). Returns `(sender_for_group, ids)`.
pub fn read_receipt_batches(chat: &str, msgs: &[Message]) -> Vec<(Option<String>, Vec<String>)> {
    let group = is_group_jid(chat);
    let mut out: Vec<(Option<String>, Vec<String>)> = Vec::new();
    for m in msgs.iter().filter(|m| !m.from_me && m.chat == chat) {
        let key = if group { Some(m.sender.clone()) } else { None };
        match out.iter_mut().find(|(k, _)| *k == key) {
            Some((_, ids)) => ids.push(m.id.clone()),
            None => out.push((key, vec![m.id.clone()])),
        }
    }
    out
}

/// Whether a live message should raise a desktop notification.
pub fn should_notify(m: &Message, open_chat: Option<&str>, focused: bool, muted: bool) -> bool {
    if m.from_me || muted || matches!(m.content, Content::Deleted) {
        return false;
    }
    !(focused && open_chat == Some(m.chat.as_str()))
}

/// Day bucket label for date separators ("Today", "Yesterday", weekday,
/// or a date), computed against `today` (local days since epoch).
pub fn day_label(day: i64, today: i64, weekday: &str, date: &str) -> String {
    match today - day {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => weekday.into(),
        _ => date.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::types::MUTE_FOREVER;

    fn m(chat: &str, id: &str, ts: i64, from_me: bool) -> Message {
        Message {
            id: id.into(),
            chat: chat.into(),
            sender: if from_me {
                "me".into()
            } else {
                format!("s-{id}")
            },
            sender_name: Some("Bob".into()),
            from_me,
            timestamp: ts,
            content: Content::Text(format!("msg {id}")),
            status: if from_me {
                ReceiptStatus::Pending
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

    fn fresh(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn list() -> ChatList {
        let mut l = ChatList::default();
        l.apply_history(
            &[
                ChatMeta {
                    id: "a".into(),
                    name: Some("Alice".into()),
                    last_ts: 10,
                    ..Default::default()
                },
                ChatMeta {
                    id: "g@g.us".into(),
                    name: Some("Group".into()),
                    last_ts: 20,
                    unread: 2,
                    ..Default::default()
                },
            ],
            &[],
        );
        l
    }

    #[test]
    fn new_message_moves_chat_to_top_and_counts_unread() {
        let mut l = list();
        assert_eq!(l.entries[0].meta.id, "g@g.us");
        l.apply_messages(&[m("a", "1", 5, false)], None, true, None);
        assert_eq!(l.get("a").unwrap().meta.unread, 0, "history does not count");
        l.apply_messages(&[m("a", "2", 30, false)], None, true, Some(&fresh(&["2"])));
        assert_eq!(l.entries[0].meta.id, "a");
        assert_eq!(l.get("a").unwrap().meta.unread, 1);
        // Same message again (not newly stored) does not double count.
        l.apply_messages(&[m("a", "2", 30, false)], None, true, Some(&fresh(&[])));
        assert_eq!(l.get("a").unwrap().meta.unread, 1);
        // Offline-drained messages older than the preview still count.
        l.apply_messages(&[m("a", "0", 1, false)], None, true, Some(&fresh(&["0"])));
        assert_eq!(l.get("a").unwrap().meta.unread, 2);
        assert_eq!(l.get("a").unwrap().last.as_ref().unwrap().id, "2");
    }

    #[test]
    fn open_focused_chat_does_not_count_unread() {
        let mut l = list();
        l.apply_messages(
            &[m("a", "2", 12, false)],
            Some("a"),
            true,
            Some(&fresh(&["2"])),
        );
        assert_eq!(l.get("a").unwrap().meta.unread, 0);
        l.apply_messages(
            &[m("a", "3", 13, false)],
            Some("a"),
            false,
            Some(&fresh(&["3"])),
        );
        assert_eq!(l.get("a").unwrap().meta.unread, 1);
        l.mark_read("a");
        assert_eq!(l.get("a").unwrap().meta.unread, 0);
    }

    #[test]
    fn filters_and_search() {
        let mut l = list();
        l.apply_messages(&[m("a", "1", 11, false)], None, true, None);
        assert_eq!(l.filtered("", Filter::Groups).len(), 1);
        assert_eq!(l.filtered("", Filter::Unread).len(), 1);
        assert_eq!(l.filtered("ali", Filter::All).len(), 1);
        assert_eq!(l.filtered("msg 1", Filter::All).len(), 1);
        assert_eq!(l.filtered("zzz", Filter::All).len(), 0);
    }

    #[test]
    fn display_names_fall_back() {
        let mut l = list();
        assert_eq!(l.display_name("a"), "Alice");
        assert_eq!(
            l.display_name("34600111222@s.whatsapp.net"),
            "+34 600 111 222"
        );
        l.apply_history(&[], &[("34600111222@s.whatsapp.net".into(), "Zoe".into())]);
        assert_eq!(l.display_name("34600111222@s.whatsapp.net"), "~ Zoe");
    }

    const ANA: &str = "34600111222@s.whatsapp.net";

    #[test]
    fn name_priority_saved_business_push_chat_number() {
        let mut l = ChatList::default();
        // Only the number is known.
        assert_eq!(l.display_name(ANA), "+34 600 111 222");
        // A name on the chat record only.
        l.apply_history(
            &[ChatMeta {
                id: ANA.into(),
                name: Some("Chat record".into()),
                ..Default::default()
            }],
            &[],
        );
        assert_eq!(l.display_name(ANA), "Chat record");
        // A push name beats the chat record and is marked with "~".
        l.set_name(ANA, NameSource::Push, "Anita");
        assert_eq!(l.display_name(ANA), "~ Anita");
        assert_eq!(l.plain_name(ANA), "Anita");
        // A business verified name beats the push name.
        l.set_name(ANA, NameSource::Business, "Ana Shop");
        assert_eq!(l.display_name(ANA), "Ana Shop");
        // The address-book name beats everything, whatever the arrival order.
        l.set_name(ANA, NameSource::Saved, "Ana (trabajo)");
        l.set_name(ANA, NameSource::Push, "Anita 2");
        l.set_name(ANA, NameSource::Business, "Ana Shop 2");
        assert_eq!(l.display_name(ANA), "Ana (trabajo)");
        // Removing the saved name falls back to the next source.
        assert!(l.set_name(ANA, NameSource::Saved, ""));
        assert_eq!(l.display_name(ANA), "Ana Shop 2");
        assert!(!l.set_name(ANA, NameSource::Saved, ""));
    }

    #[test]
    fn push_name_hint_only_when_nothing_better_is_known() {
        let mut l = ChatList::default();
        assert_eq!(l.sender_name(ANA, Some("Hint")), "~ Hint");
        assert_eq!(l.sender_name(ANA, Some("")), "+34 600 111 222");
        l.set_name(ANA, NameSource::Saved, "Ana");
        // A push name carried by a message must not override the saved name.
        assert_eq!(l.sender_name(ANA, Some("Hint")), "Ana");
    }

    #[test]
    fn groups_show_their_subject_not_member_names() {
        let mut l = ChatList::default();
        l.apply_history(
            &[ChatMeta {
                id: "g@g.us".into(),
                name: Some("Familia".into()),
                ..Default::default()
            }],
            &[],
        );
        l.set_name("g@g.us", NameSource::Push, "Whoever");
        assert_eq!(l.display_name("g@g.us"), "Familia");
        assert_eq!(l.display_name("other@g.us"), "Group");
    }

    #[test]
    fn message_push_names_are_learned_but_never_override_saved() {
        let mut l = ChatList::default();
        let mut msg = m(ANA, "1", 10, false);
        msg.sender = ANA.into();
        msg.sender_name = Some("Anita".into());
        let rows = l.learn_push_names(std::slice::from_ref(&msg));
        assert_eq!(
            rows,
            vec![(ANA.to_string(), NameSource::Push, "Anita".to_string())]
        );
        // Seen again: nothing to persist.
        assert!(l.learn_push_names(std::slice::from_ref(&msg)).is_empty());
        l.set_name(ANA, NameSource::Saved, "Ana");
        msg.sender_name = Some("Anita renamed".into());
        l.learn_push_names(std::slice::from_ref(&msg));
        assert_eq!(l.display_name(ANA), "Ana");
    }

    #[test]
    fn name_book_round_trips_rows() {
        let b = NameBook::from_rows(vec![
            ("a".into(), NameSource::Push, "P".into()),
            ("a".into(), NameSource::Saved, "S".into()),
            ("b".into(), NameSource::Business, "B".into()),
        ]);
        assert_eq!(b.best("a"), Some((NameSource::Saved, "S")));
        assert_eq!(b.best("b"), Some((NameSource::Business, "B")));
        assert_eq!(b.best("c"), None);
    }

    fn custom(id: &str, name: &str, order: i32) -> ListDef {
        ListDef {
            id: id.into(),
            name: name.into(),
            kind: ListKind::Custom,
            order,
            ..Default::default()
        }
    }

    #[test]
    fn lists_reducers_and_filter() {
        let mut l = list();
        l.apply_list_edit(custom("2", "Work", 1), false);
        l.apply_list_edit(custom("1", "Family", 0), false);
        let names: Vec<_> = l.custom_lists().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Family", "Work"]);
        assert_eq!(l.next_list_id(), "3");
        assert_eq!(l.next_list_order(), 2);

        l.apply_list_chat("1", "a", true);
        let f = Filter::List("1".into());
        let rows = l.filtered_view("", &f, false);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].meta.id, "a");
        assert!(l.in_list("1", "a"));
        l.apply_list_chat("1", "a", false);
        assert!(l.filtered_view("", &f, false).is_empty());

        // A partial edit (no name) keeps the known name.
        l.apply_list_edit(
            ListDef {
                id: "2".into(),
                order: 5,
                kind: ListKind::Custom,
                ..Default::default()
            },
            false,
        );
        assert_eq!(l.list("2").unwrap().name, "Work");
        assert_eq!(l.list("2").unwrap().order, 5);

        // Deleting removes the list and its members.
        l.apply_list_chat("2", "a", true);
        l.apply_list_edit(custom("2", "", 0), true);
        assert!(l.list("2").is_none());
        assert!(!l.in_list("2", "a"));
    }

    #[test]
    fn favourites_use_the_synced_list_when_there_is_one() {
        let mut l = list();
        assert_eq!(l.favourites_target(), LOCAL_FAVOURITES);
        l.apply_list_chat(LOCAL_FAVOURITES, "a", true);
        assert!(l.is_favourite("a"));
        assert_eq!(l.filtered("", Filter::Favourites).len(), 1);
        // The phone syncs a Favourites list: new favourites go there.
        l.apply_list_edit(
            ListDef {
                id: "fav".into(),
                kind: ListKind::Favourites,
                ..Default::default()
            },
            false,
        );
        assert_eq!(l.favourites_target(), "fav");
        l.apply_list_chat("fav", "g@g.us", true);
        assert!(l.is_favourite("g@g.us"));
        assert_eq!(l.filtered("", Filter::Favourites).len(), 2);
        // Favourites never appear as a custom chip.
        assert_eq!(l.custom_lists().count(), 0);
    }

    #[test]
    fn archive_flag_moves_chats_between_views() {
        let mut l = list();
        assert_eq!(l.filtered_view("", &Filter::All, false).len(), 2);
        assert!(l.filtered_view("", &Filter::All, true).is_empty());
        l.apply_flag("a", ChatFlag::Archived(true));
        assert_eq!(l.archived_count(), 1);
        assert_eq!(l.filtered_view("", &Filter::All, false).len(), 1);
        assert_eq!(l.filtered_view("", &Filter::All, true).len(), 1);
        // Searching finds archived chats too, like the official app.
        assert_eq!(l.filtered_view("ali", &Filter::All, false).len(), 1);
        // Filters apply inside the archive as well.
        assert!(l.filtered_view("", &Filter::Groups, true).is_empty());
        l.apply_flag("a", ChatFlag::Archived(false));
        assert_eq!(l.archived_count(), 0);
        // Pin and mute flags.
        l.apply_flag("a", ChatFlag::Pinned(true));
        assert_eq!(l.pinned_count(), 1);
        assert_eq!(l.entries[0].meta.id, "a", "pinned chats sort first");
        l.apply_flag("a", ChatFlag::Muted(MUTE_FOREVER));
        assert!(l.get("a").unwrap().meta.is_muted(i64::MAX));
        // A flag for an unknown chat creates the row.
        l.apply_flag("new@s.whatsapp.net", ChatFlag::Archived(true));
        assert_eq!(l.archived_count(), 1);
    }

    #[test]
    fn the_rail_badge_counts_unread_chats_not_messages() {
        let mut l = ChatList::default();
        let chat = |id: &str, unread: u32| ChatMeta {
            id: id.into(),
            unread,
            last_ts: 1,
            ..Default::default()
        };
        l.apply_history(
            &[
                chat("a@s.whatsapp.net", 120),
                chat("b@s.whatsapp.net", 3),
                chat("c@s.whatsapp.net", 0),
                chat("status@broadcast", 250),
                chat("123@newsletter", 9),
                chat("muted@s.whatsapp.net", 4),
                chat("arch@s.whatsapp.net", 5),
            ],
            &[],
        );
        l.apply_flag("muted@s.whatsapp.net", ChatFlag::Muted(MUTE_FOREVER));
        l.apply_flag("arch@s.whatsapp.net", ChatFlag::Archived(true));
        assert_eq!(
            l.unread_chats(100),
            2,
            "a and b; not messages, status, channels, muted or archived"
        );
        assert!(!l.mark_read("a@s.whatsapp.net"), "not marked by hand");
        assert_eq!(l.unread_chats(100), 1);
        // Marked unread on the phone: one, without inventing a count.
        l.mark_unread("c@s.whatsapp.net");
        assert_eq!(l.get("c@s.whatsapp.net").unwrap().meta.unread, 1);
        l.mark_unread("b@s.whatsapp.net");
        assert_eq!(l.get("b@s.whatsapp.net").unwrap().meta.unread, 3);
        assert_eq!(l.unread_chats(100), 2);
        // Reading a chat marked by hand must be told to the other devices,
        // once.
        assert!(l.mark_read("c@s.whatsapp.net"));
        assert!(!l.mark_read("c@s.whatsapp.net"));
        // A timed mute stops counting once it ends.
        l.apply_flag("muted@s.whatsapp.net", ChatFlag::Muted(200));
        assert_eq!(l.unread_chats(100), 1);
        assert_eq!(l.unread_chats(200), 2);
    }

    #[test]
    fn clearing_keeps_the_row_and_deleting_drops_it() {
        let mut l = list();
        l.mark_unread("a");
        l.clear_chat("a");
        let a = l.get("a").unwrap();
        assert!(a.last.is_none());
        assert_eq!(a.meta.unread, 0);
        assert!(!l.mark_read("a"), "clearing forgets the manual mark");
        let before = l.entries.len();
        assert!(l.delete_chat("a"));
        assert!(l.get("a").is_none());
        assert_eq!(l.entries.len(), before - 1);
        assert!(!l.delete_chat("a"));
        // The index still finds the others.
        let other = l.entries[0].meta.id.clone();
        assert!(l.get(&other).is_some());
    }

    #[test]
    fn a_saved_name_under_the_other_id_wins_over_a_push_name() {
        let mut b = NameBook::default();
        b.set("1@s.whatsapp.net", NameSource::Push, "Pushy");
        b.set("9@lid", NameSource::Saved, "Saved Name");
        assert_eq!(
            b.best("1@s.whatsapp.net"),
            Some((NameSource::Push, "Pushy"))
        );
        b.link("9@lid", "1@s.whatsapp.net");
        assert_eq!(
            b.best("1@s.whatsapp.net"),
            Some((NameSource::Saved, "Saved Name"))
        );
        assert_eq!(b.best("9@lid"), Some((NameSource::Saved, "Saved Name")));
        // The id's own saved name still beats the alias's.
        b.set("1@s.whatsapp.net", NameSource::Saved, "Own");
        assert_eq!(b.best("1@s.whatsapp.net"), Some((NameSource::Saved, "Own")));
        assert_eq!(b.counts(), (2, 0, 1));
    }

    #[test]
    fn name_stats_count_sources_without_names() {
        let mut l = list();
        l.apply_history(
            &[ChatMeta {
                id: "1@s.whatsapp.net".into(),
                last_ts: 3,
                ..Default::default()
            }],
            &[],
        );
        l.set_name("1@s.whatsapp.net", NameSource::Saved, "X");
        let st = l.name_stats();
        assert_eq!(st.saved, 1);
        assert_eq!(st.chats_saved, 1);
        // "a" is not a JID and the group is not a person: neither counts.
        assert_eq!(st.direct_chats, 1);
        assert!(!format!("{st:?}").contains('X'));
    }

    #[test]
    fn archived_unread_counts_chats() {
        let mut l = list();
        l.apply_flag("g@g.us", ChatFlag::Archived(true));
        assert_eq!(l.archived_unread(), 1);
    }

    #[test]
    fn conversation_orders_and_dedupes() {
        let mut c = Conversation::new("a".into(), vec![m("a", "2", 20, false)], false);
        c.merge(vec![
            m("a", "1", 10, true),
            m("a", "3", 30, false),
            m("b", "x", 1, false),
        ]);
        c.merge(vec![m("a", "2", 20, false)]);
        let ids: Vec<_> = c.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["1", "2", "3"]);
    }

    #[test]
    fn conversation_receipts_and_deletes() {
        let mut c = Conversation::new("a".into(), vec![m("a", "1", 10, true)], false);
        assert!(c.apply_receipt(&["1".into()], ReceiptStatus::Delivered));
        assert!(!c.apply_receipt(&["1".into()], ReceiptStatus::Sent));
        assert_eq!(c.get("1").unwrap().status, ReceiptStatus::Delivered);
        // Someone else cannot revoke my message.
        let mut forged = m("a", "1", 10, false);
        forged.content = Content::Deleted;
        c.merge(vec![forged]);
        assert_ne!(c.get("1").unwrap().content, Content::Deleted);
        let mut del = m("a", "1", 10, true);
        del.content = Content::Deleted;
        c.merge(vec![del.clone()]);
        assert_eq!(c.get("1").unwrap().content, Content::Deleted);
        // Revokes never create bubbles.
        del.id = "unknown".into();
        c.merge(vec![del]);
        assert!(c.get("unknown").is_none());
    }

    #[test]
    fn index_survives_many_chats() {
        let mut l = ChatList::default();
        let chats: Vec<ChatMeta> = (0..5000)
            .map(|i| ChatMeta {
                id: format!("{i}@s.whatsapp.net"),
                last_ts: i,
                ..Default::default()
            })
            .collect();
        l.apply_history(&chats, &[]);
        assert_eq!(l.entries.len(), 5000);
        assert_eq!(l.entries[0].meta.id, "4999@s.whatsapp.net");
        assert_eq!(l.get("17@s.whatsapp.net").unwrap().meta.last_ts, 17);
        l.mark_read("17@s.whatsapp.net");
    }

    #[test]
    fn window_is_bounded() {
        let msgs: Vec<_> = (0..WINDOW as i64 + 50)
            .map(|i| m("a", &format!("{i:05}"), i, false))
            .collect();
        let c = Conversation::new("a".into(), msgs, false);
        assert_eq!(c.messages.len(), WINDOW);
        assert!(c.has_more);
        assert_eq!(c.messages.front().unwrap().id, "00050");
    }

    #[test]
    fn prepend_older_page() {
        let mut c = Conversation::new("a".into(), vec![m("a", "5", 50, false)], true);
        c.prepend(vec![m("a", "3", 30, false), m("a", "4", 40, false)], false);
        let ids: Vec<_> = c.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["3", "4", "5"]);
        assert!(!c.has_more);
        assert_eq!(c.oldest_cursor(), Some((30, "3".into())));
    }

    #[test]
    fn read_receipts_grouped_per_sender_in_groups() {
        let msgs = vec![
            m("g@g.us", "1", 1, false),
            m("g@g.us", "2", 2, false),
            m("g@g.us", "3", 3, true),
        ];
        let b = read_receipt_batches("g@g.us", &msgs);
        assert_eq!(b.len(), 2);
        assert!(b.iter().all(|(s, _)| s.is_some()));
        let dm = vec![m("a", "1", 1, false), m("a", "2", 2, false)];
        assert_eq!(
            read_receipt_batches("a", &dm),
            vec![(None, vec!["1".into(), "2".into()])]
        );
    }

    #[test]
    fn notification_rules() {
        let msg = m("a", "1", 1, false);
        assert!(should_notify(&msg, None, true, false));
        assert!(!should_notify(&msg, Some("a"), true, false));
        assert!(should_notify(&msg, Some("a"), false, false));
        assert!(!should_notify(&msg, None, false, true));
        assert!(!should_notify(&m("a", "2", 1, true), None, false, false));
    }

    #[test]
    fn day_labels() {
        assert_eq!(day_label(10, 10, "Monday", "1/1/2026"), "Today");
        assert_eq!(day_label(9, 10, "Monday", "1/1/2026"), "Yesterday");
        assert_eq!(day_label(6, 10, "Monday", "1/1/2026"), "Monday");
        assert_eq!(day_label(1, 10, "Monday", "1/1/2026"), "1/1/2026");
    }

    // ---- parity features -------------------------------------------------

    #[test]
    fn reactions_one_per_reactor_newest_wins() {
        let mut l: Vec<Reaction> = Vec::new();
        assert!(apply_reaction(&mut l, "ana", "👍", 10));
        assert!(
            !apply_reaction(&mut l, "ana", "👍", 11),
            "same emoji is no change"
        );
        assert!(apply_reaction(&mut l, "ana", "😂", 12));
        assert!(
            !apply_reaction(&mut l, "ana", "😮", 5),
            "stale update ignored"
        );
        assert!(apply_reaction(&mut l, "bob", "😂", 13));
        assert_eq!(l.len(), 2);
        assert!(
            !apply_reaction(&mut l, "ana", "", 1),
            "stale removal ignored"
        );
        assert!(apply_reaction(&mut l, "ana", "", 20));
        assert!(!apply_reaction(&mut l, "ana", "", 21), "nothing to remove");
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn reaction_summary_orders_by_count() {
        let r = |who: &str, e: &str| Reaction {
            reactor: who.into(),
            emoji: e.into(),
            ts: 1,
        };
        let list = vec![r("a", "👍"), r("b", "❤️"), r("c", "❤️"), r("d", "😂")];
        let (counts, total) = reaction_summary(&list);
        assert_eq!(total, 4);
        assert_eq!(counts[0], ("❤️".to_string(), 2));
        assert_eq!(counts[1].0, "👍", "ties keep first-seen order");
        assert_eq!(reaction_summary(&[]), (vec![], 0));
    }

    #[test]
    fn group_status_aggregation() {
        assert_eq!(group_status(0, 5, 5), None, "unknown size");
        assert_eq!(group_status(3, 2, 0), None);
        assert_eq!(group_status(3, 3, 0), Some(ReceiptStatus::Delivered));
        assert_eq!(group_status(3, 3, 2), Some(ReceiptStatus::Delivered));
        assert_eq!(group_status(3, 3, 3), Some(ReceiptStatus::Read));
        // Reads imply deliveries even if the delivery receipts were lost.
        assert_eq!(group_status(2, 0, 2), Some(ReceiptStatus::Read));
        assert_eq!(group_status(2, 0, 1), None);
    }

    #[test]
    fn conversation_edit_remove_and_windows() {
        let mut c = Conversation::new(
            "a".into(),
            vec![m("a", "1", 10, true), m("a", "2", 20, false)],
            false,
        );
        // Edit rules mirror the store: only the original sender.
        assert!(!c.apply_edit("2", "intruder", false, &Content::Text("x".into())));
        assert!(c.apply_edit("2", "s-2", false, &Content::Text("edited".into())));
        assert_eq!(c.get("2").unwrap().content, Content::Text("edited".into()));
        assert!(c.get("2").unwrap().edited);
        assert!(!c.apply_edit("1", "s-1", false, &Content::Text("x".into())));
        assert!(c.apply_edit("1", "me", true, &Content::Text("mine".into())));
        assert!(!c.apply_edit("zz", "me", true, &Content::Text("x".into())));
        assert!(c.remove("1"));
        assert!(!c.remove("1"));
        assert_eq!(c.messages.len(), 1);
        // Newer pages are appended without duplicates and lift `has_newer`.
        c.has_newer = true;
        c.append_newer(vec![m("a", "2", 20, false), m("a", "3", 30, false)], false);
        let ids: Vec<_> = c.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["2", "3"]);
        assert!(!c.has_newer);
        c.replace_window(vec![m("a", "9", 90, false)], true, true);
        assert_eq!(c.messages.len(), 1);
        assert!(c.has_more && c.has_newer);
    }

    #[test]
    fn conversation_reactions_and_group_status() {
        let mut c = Conversation::new("a".into(), vec![m("a", "1", 10, true)], false);
        assert!(c.apply_reaction("1", "ana", "👍", 5));
        assert!(!c.apply_reaction("missing", "ana", "👍", 5));
        assert_eq!(c.get("1").unwrap().reactions.len(), 1);
        c.set_status("1", ReceiptStatus::Delivered);
        c.set_status("1", ReceiptStatus::Sent);
        assert_eq!(c.get("1").unwrap().status, ReceiptStatus::Delivered);
    }

    #[test]
    fn chat_list_follows_edits_and_local_deletes() {
        let mut l = list();
        let mut last = m("a", "5", 50, true);
        last.sender = "me".into();
        l.apply_messages(&[last], None, true, None);
        assert!(l.is_last("a", "5"));
        l.apply_edit("a", "5", "intruder", false, &Content::Text("hax".into()));
        assert_eq!(
            l.get("a").unwrap().last.as_ref().unwrap().content,
            Content::Text("msg 5".into())
        );
        l.apply_edit("a", "5", "me", true, &Content::Text("fixed".into()));
        let shown = l.get("a").unwrap().last.as_ref().unwrap();
        assert_eq!(shown.content, Content::Text("fixed".into()));
        assert!(shown.edited);
        l.set_status("a", "5", ReceiptStatus::Read);
        assert_eq!(
            l.get("a").unwrap().last.as_ref().unwrap().status,
            ReceiptStatus::Read
        );
        l.set_last("a", None);
        assert!(l.get("a").unwrap().last.is_none());
    }
}

#[cfg(test)]
mod mention_tests {
    use super::*;

    const LID: &str = "123456789012345@lid";
    const PN: &str = "34600111222@s.whatsapp.net";

    fn list() -> ChatList {
        ChatList::from_rows(Vec::new(), NameBook::default())
    }

    #[test]
    fn lid_mention_uses_the_saved_name_of_its_phone_number() {
        let mut l = list();
        l.names.link(LID, PN);
        l.set_name(PN, NameSource::Saved, "Ana García");
        let mentions = vec![LID.to_string()];
        assert_eq!(
            l.mention_label("123456789012345", &mentions, &[]),
            "Ana García"
        );
        assert_eq!(
            l.render_mentions("hola @123456789012345!", &mentions, &[]),
            "hola @Ana García!"
        );
    }

    #[test]
    fn name_order_is_saved_business_push() {
        let mut l = list();
        let mentions = vec![LID.to_string()];
        l.set_name(LID, NameSource::Push, "Ani");
        assert_eq!(l.mention_label("123456789012345", &mentions, &[]), "Ani");
        l.set_name(LID, NameSource::Business, "Ana Shop");
        assert_eq!(
            l.mention_label("123456789012345", &mentions, &[]),
            "Ana Shop"
        );
        l.names.link(LID, PN);
        l.set_name(PN, NameSource::Saved, "Ana");
        assert_eq!(l.mention_label("123456789012345", &mentions, &[]), "Ana");
    }

    #[test]
    fn unnamed_mentions_fall_back_to_the_phone_number_never_the_lid() {
        let mut l = list();
        let mentions = vec![LID.to_string()];
        // Unknown LID: no digits at all.
        assert_eq!(
            l.mention_label("123456789012345", &mentions, &[]),
            "Unknown"
        );
        // Known number: formatted.
        l.names.link(LID, PN);
        assert_eq!(
            l.mention_label("123456789012345", &mentions, &[]),
            "+34 600 111 222"
        );
        // A phone-number mention without a name.
        assert_eq!(
            l.mention_label("34600111222", &[PN.to_string()], &[]),
            "+34 600 111 222"
        );
    }

    #[test]
    fn own_mentions_read_you_and_rows_without_mentions_guess() {
        let mut l = list();
        l.names.link(LID, PN);
        assert_eq!(
            l.mention_label("123456789012345", &[LID.to_string()], &[PN]),
            "You"
        );
        // Cached before mentions were stored: a LID we know is found.
        l.set_name(PN, NameSource::Saved, "Ana");
        assert_eq!(l.mention_label("123456789012345", &[], &[]), "Ana");
        assert_eq!(l.mention_label("34600111222", &[], &[]), "Ana");
    }

    #[test]
    fn chat_list_preview_shows_names() {
        let mut l = list();
        l.names.link(LID, PN);
        l.set_name(PN, NameSource::Saved, "Ana");
        let mut msg = Message {
            id: "1".into(),
            chat: "g@g.us".into(),
            sender: PN.into(),
            sender_name: None,
            from_me: false,
            timestamp: 1,
            content: Content::Text("@123456789012345 can you bring the cake?".into()),
            status: ReceiptStatus::Read,
            expires_at: None,
            reply: None,
            edited: false,
            forwarded: None,
            mentions: vec![LID.into()],
            reactions: Vec::new(),
        };
        assert_eq!(l.preview(&msg, &[]), "@Ana can you bring the cake?");
        msg.content = Content::Text("mail me at a@b.com, @x".into());
        assert_eq!(l.preview(&msg, &[]), "mail me at a@b.com, @x");
    }
}

#[cfg(test)]
mod read_state_tests {
    use super::*;

    #[test]
    fn reading_needs_focus_the_newest_page_and_the_bottom() {
        assert!(is_reading(true, 0.0, false));
        assert!(is_reading(true, BOTTOM_SLACK, false));
        assert!(!is_reading(false, 0.0, false));
        assert!(!is_reading(true, BOTTOM_SLACK + 1.0, false));
        assert!(!is_reading(true, 0.0, true));
    }

    #[test]
    fn divider_goes_above_the_oldest_unread_incoming_message() {
        let mk = |id: &str, from_me: bool| Message {
            id: id.into(),
            chat: "c".into(),
            sender: "c".into(),
            sender_name: None,
            from_me,
            timestamp: 1,
            content: Content::Text(String::new()),
            status: ReceiptStatus::Read,
            expires_at: None,
            reply: None,
            edited: false,
            forwarded: None,
            mentions: Vec::new(),
            reactions: Vec::new(),
        };
        let msgs: VecDeque<Message> = [
            mk("a", false),
            mk("b", false),
            mk("me", true),
            mk("c", false),
        ]
        .into();
        assert_eq!(first_unread(&msgs, 2).as_deref(), Some("b"));
        assert_eq!(first_unread(&msgs, 9).as_deref(), Some("a"));
        assert_eq!(first_unread(&msgs, 0), None);
    }

    #[test]
    fn wire_jids_fall_back_to_the_display_jid() {
        let mut w = HashMap::new();
        w.insert("1@s.whatsapp.net".to_string(), "9@lid".to_string());
        assert_eq!(wire_jid(&w, "1@s.whatsapp.net"), "9@lid");
        assert_eq!(wire_jid(&w, "2@s.whatsapp.net"), "2@s.whatsapp.net");
    }
}
