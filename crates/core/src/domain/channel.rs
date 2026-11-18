//! Realtime channels (SPEC §13.3): ephemeral messages and presence between
//! the members of a named channel such as `room:42`.

use serde::{Deserialize, Serialize};

/// Longest channel key (the part after `:`).
pub const MAX_KEY_LEN: usize = 128;
/// Largest presence `meta` (JSON bytes).
pub const MAX_META_BYTES: usize = 1024;

/// Limits of a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelOptions {
    /// Members per channel instance.
    pub max_members: u32,
    /// Messages per second per connection.
    pub rate_per_s: u32,
    /// Senders receive their own messages.
    pub echo: bool,
    /// Largest message `data` (JSON bytes).
    pub max_payload: usize,
}

impl Default for ChannelOptions {
    fn default() -> Self {
        Self {
            max_members: 1000,
            rate_per_s: 20,
            echo: false,
            max_payload: 16 * 1024,
        }
    }
}

/// A channel definition (`_channels`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelDef {
    /// Id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// `lobby` (one channel) or `room:*` (one per key).
    pub pattern: String,
    /// Who may join; `None` is superusers only, `""` everyone.
    pub join_rule: Option<String>,
    /// Who may publish; `None` is superusers only, `""` every member.
    pub publish_rule: Option<String>,
    /// Track presence.
    pub presence: bool,
    /// Limits.
    pub options: ChannelOptions,
    /// Creation time.
    pub created: String,
    /// Last change.
    pub updated: String,
}

/// Whether `p` is a valid pattern: a lowercase name, optionally `:*`.
pub fn valid_pattern(p: &str) -> bool {
    let base = p.strip_suffix(":*").unwrap_or(p);
    !base.is_empty()
        && base.len() <= 64
        && base.starts_with(|c: char| c.is_ascii_lowercase())
        && base
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

impl ChannelDef {
    /// For `channel` (e.g. `room:42`): `None` when it does not match,
    /// `Some(None)` for a channel without key, `Some(Some(key))` otherwise.
    pub fn key_of(&self, channel: &str) -> Option<Option<String>> {
        match self.pattern.strip_suffix(":*") {
            Some(prefix) => {
                let key = channel.strip_prefix(prefix)?.strip_prefix(':')?;
                let ok = !key.is_empty()
                    && key.len() <= MAX_KEY_LEN
                    && key.chars().all(|c| !c.is_control() && !c.is_whitespace());
                ok.then(|| Some(key.to_owned()))
            }
            None => (channel == self.pattern).then_some(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(p: &str) -> ChannelDef {
        ChannelDef {
            id: "x".into(),
            name: "x".into(),
            pattern: p.into(),
            join_rule: None,
            publish_rule: None,
            presence: true,
            options: ChannelOptions::default(),
            created: String::new(),
            updated: String::new(),
        }
    }

    #[test]
    fn patterns_and_keys() {
        assert!(
            valid_pattern("room:*") && valid_pattern("lobby") && valid_pattern("doc-cursors:*")
        );
        assert!(
            !valid_pattern("Room")
                && !valid_pattern("room:x")
                && !valid_pattern(":*")
                && !valid_pattern("a b")
        );
        let rooms = def("room:*");
        assert_eq!(rooms.key_of("room:42"), Some(Some("42".into())));
        assert_eq!(rooms.key_of("room:"), None);
        assert_eq!(rooms.key_of("roomy:1"), None);
        assert_eq!(rooms.key_of("room"), None);
        assert_eq!(rooms.key_of(&format!("room:{}", "k".repeat(129))), None);
        let lobby = def("lobby");
        assert_eq!(lobby.key_of("lobby"), Some(None));
        assert_eq!(lobby.key_of("lobby:1"), None);
    }
}
