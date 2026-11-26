//! Crate-wide error type and redaction helpers.

use std::fmt;

/// Every fallible API in `net`, `store` and `auth` returns this error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Transport-level failure (socket, HTTP, DNS).
    #[error("network error: {0}")]
    Net(String),
    /// The server or library returned something we could not interpret.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// Pairing / session problems.
    #[error("authentication error: {0}")]
    Auth(String),
    /// Local SQLite cache failure.
    #[error("store error: {0}")]
    Store(#[from] rusqlite::Error),
    /// OS keyring (Secret Service / Keychain / Credential Manager) failure.
    #[error("keyring error: {0}")]
    Keyring(String),
    /// File system failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// Media decoding / playback failure.
    #[error("media error: {0}")]
    Media(String),
    /// The CDN no longer has this media (404 / 410): ask for a re-upload.
    #[error("media expired on the server")]
    MediaExpired,
    /// A re-upload was asked for and the phone no longer has the media.
    #[error("media no longer available: {0}")]
    MediaGone(String),
    /// The operation was cancelled (window closed, logout in progress, ...).
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    /// A short, user-facing message that never contains secrets, phone
    /// numbers or message bodies.
    pub fn user_message(&self) -> String {
        match self {
            Error::Net(_) => "Could not reach WhatsApp. Check your connection.".into(),
            Error::Protocol(_) => "WhatsApp sent something Ferrochat did not understand.".into(),
            Error::Auth(_) => "Your session is no longer valid. Link this device again.".into(),
            Error::Store(_) => "The local message cache could not be read.".into(),
            Error::Keyring(_) => {
                "The system keyring is not available. Ferrochat keeps its session key \
                 only in a Secret Service and never on disk in plain text. On KDE Plasma: \
                 open System Settings → KDE Wallet, enable the wallet subsystem and make \
                 sure kwalletd6 (or kwalletd5) is running; the KDE Secret Service bridge \
                 (ksecretd / kwallet ≥ 6.x) must be active. On GNOME: install and unlock \
                 gnome-keyring. Then press Try again."
                    .into()
            }
            Error::Io(_) => "A local file could not be read or written.".into(),
            Error::Media(_) => "This media file could not be played or shown.".into(),
            Error::MediaExpired => "This media expired on WhatsApp's servers.".into(),
            Error::MediaGone(_) => {
                "This media is no longer available. Ask the sender to send it again.".into()
            }
            Error::Cancelled => "Cancelled.".into(),
        }
    }
}

/// Crate-wide result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Wraps a value so it is never printed. Use it in `tracing` fields:
/// `info!(body = %Redacted(&text))`.
pub struct Redacted<T>(pub T);

impl<T: AsRef<str>> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED len={}]", self.0.as_ref().chars().count())
    }
}

impl<T: AsRef<str>> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Masks a JID / phone number for logs: keeps the server part and the last
/// two digits of the user part (`•••••••12@s.whatsapp.net`).
pub fn mask_jid(jid: &str) -> String {
    let (user, server) = match jid.split_once('@') {
        Some((u, s)) => (u, Some(s)),
        None => (jid, None),
    };
    let user = user.split([':', '.']).next().unwrap_or(user);
    let n = user.chars().count();
    let tail: String = user.chars().skip(n.saturating_sub(2)).collect();
    let masked = format!("{}{}", "•".repeat(n.saturating_sub(2).min(10)), tail);
    match server {
        Some(s) => format!("{masked}@{s}"),
        None => masked,
    }
}

/// `Display` adapter around [`mask_jid`].
pub struct MaskedJid<'a>(pub &'a str);

impl fmt::Display for MaskedJid<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&mask_jid(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_hides_body() {
        let s = format!("{}", Redacted("hello secret world"));
        assert!(!s.contains("secret"));
        assert_eq!(s, "[REDACTED len=18]");
    }

    #[test]
    fn masks_phone_jid() {
        assert_eq!(
            mask_jid("34612345678@s.whatsapp.net"),
            "•••••••••78@s.whatsapp.net"
        );
        assert_eq!(
            mask_jid("34612345678:12@s.whatsapp.net"),
            "•••••••••78@s.whatsapp.net"
        );
        assert_eq!(mask_jid("12"), "12");
        assert!(!mask_jid("34612345678@s.whatsapp.net").contains("612345"));
    }

    #[test]
    fn user_messages_are_secret_free() {
        let e = Error::Net("token=abcdef1234 host=x".into());
        assert!(!e.user_message().contains("abcdef"));
    }
}
