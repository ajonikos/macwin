//! Plain-text clipboard synchronisation between the two machines. Pure
//! logic: the driver reports local clipboard changes and the peer's
//! updates, and executes the answers (send to the peer, write locally).
//!
//! Ordering: every local change gets the next value of a Lamport clock. A
//! received update is applied only when its version `(clock, origin)` is
//! newer than the version of the local clipboard; equal clocks go to the
//! listener's update. Both machines therefore settle on the same text even
//! when the user copies on both at once, and late or reordered updates
//! never overwrite a newer copy.
//!
//! Echoes: the driver does not report its own writes (it remembers the
//! change count after writing, and backends tag macwin's writes, which
//! arrive here as [`LocalClipboard::Ours`]). As a last line of defence, a
//! local text equal to the one both sides already share is not resent, so
//! a clipboard manager that re-copies our writes cannot start a loop.
//!
//! Content is never logged: the [`fmt::Debug`] impls print sizes only.

use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::engine::TransportRole;
use crate::msg::{ClipboardMsg, ClipboardText};

/// What the local clipboard holds after a change.
#[derive(Clone, PartialEq, Eq)]
pub enum LocalClipboard {
    /// macwin wrote it (the backend found its origin marker).
    Ours,
    /// The source app marked it as concealed or transient (password
    /// managers do): never shared.
    Sensitive,
    /// No plain text (an image, files, or an empty clipboard).
    NoText,
    /// Text over the size limit; `bytes` is a lower bound of its size.
    TooLarge {
        bytes: usize,
    },
    Text(String),
}

impl fmt::Debug for LocalClipboard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LocalClipboard::Ours => f.write_str("Ours"),
            LocalClipboard::Sensitive => f.write_str("Sensitive"),
            LocalClipboard::NoText => f.write_str("NoText"),
            LocalClipboard::TooLarge { bytes } => write!(f, "TooLarge({bytes} bytes)"),
            LocalClipboard::Text(t) => write!(f, "Text({} bytes)", t.len()),
        }
    }
}

/// What happened to a local change, for logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    /// The text was sent to the peer.
    Shared,
    /// macwin's own write: nothing to do.
    Ours,
    /// The text both sides already share (re-copied): nothing to do.
    Echo,
    /// Not shared; the peer is told that something else was copied.
    Sensitive,
    NoText,
    TooLarge,
}

/// The answer to a local change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalOutcome {
    /// Send this to the peer.
    pub send: Option<ClipboardMsg>,
    pub kind: LocalKind,
}

/// The answer to an update from the peer.
#[derive(Clone, PartialEq, Eq)]
pub enum RemoteOutcome {
    /// Write this text to the local clipboard.
    Write(String),
    /// The peer copied something that is not shared; nothing to write.
    Noted,
    /// Older than the local clipboard: ignored.
    Stale,
    /// Larger than the local limit: ignored (but it is the newest version).
    TooLarge,
}

impl fmt::Debug for RemoteOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RemoteOutcome::Write(t) => write!(f, "Write({} bytes)", t.len()),
            RemoteOutcome::Noted => f.write_str("Noted"),
            RemoteOutcome::Stale => f.write_str("Stale"),
            RemoteOutcome::TooLarge => f.write_str("TooLarge"),
        }
    }
}

/// A clipboard version; the derived order is (clock, origin), and the
/// listener's version wins a tie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    clock: u64,
    by_listener: bool,
}

/// Synchronisation state of one session.
#[derive(Debug)]
pub struct ClipboardSync {
    local_is_listener: bool,
    max_text_bytes: usize,
    clock: u64,
    /// Version of what the local clipboard holds, if known.
    current: Option<Version>,
    /// Hash of the text both machines are known to hold.
    shared: Option<u64>,
}

fn hash_text(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

impl ClipboardSync {
    /// `role` is this machine's transport role in the session (it breaks
    /// ties); texts longer than `max_text_bytes` are neither sent nor
    /// written.
    pub fn new(role: TransportRole, max_text_bytes: usize) -> ClipboardSync {
        ClipboardSync {
            local_is_listener: role == TransportRole::Listener,
            max_text_bytes,
            clock: 0,
            current: None,
            shared: None,
        }
    }

    fn next_local_version(&mut self) -> Version {
        self.clock = self.clock.saturating_add(1);
        let v = Version {
            clock: self.clock,
            by_listener: self.local_is_listener,
        };
        self.current = Some(v);
        v
    }

    /// The local clipboard changed (and not by macwin's last write).
    pub fn on_local_change(&mut self, content: LocalClipboard) -> LocalOutcome {
        let withheld = match content {
            LocalClipboard::Ours => {
                return LocalOutcome {
                    send: None,
                    kind: LocalKind::Ours,
                };
            }
            LocalClipboard::Text(text) if text.len() <= self.max_text_bytes => {
                let hash = hash_text(&text);
                if self.shared == Some(hash) {
                    return LocalOutcome {
                        send: None,
                        kind: LocalKind::Echo,
                    };
                }
                let v = self.next_local_version();
                self.shared = Some(hash);
                return LocalOutcome {
                    send: Some(ClipboardMsg::Text(ClipboardText {
                        clock: v.clock,
                        text,
                    })),
                    kind: LocalKind::Shared,
                };
            }
            LocalClipboard::Text(_) | LocalClipboard::TooLarge { .. } => LocalKind::TooLarge,
            LocalClipboard::Sensitive => LocalKind::Sensitive,
            LocalClipboard::NoText => LocalKind::NoText,
        };
        // Something newer than the shared text is on the clipboard now: tell
        // the peer, so neither side treats the old text as shared any more.
        let v = self.next_local_version();
        self.shared = None;
        LocalOutcome {
            send: Some(ClipboardMsg::Other { clock: v.clock }),
            kind: withheld,
        }
    }

    /// The peer sent an update.
    pub fn on_remote(&mut self, msg: ClipboardMsg) -> RemoteOutcome {
        let clock = match &msg {
            ClipboardMsg::Text(t) => t.clock,
            ClipboardMsg::Other { clock } => *clock,
        };
        self.clock = self.clock.max(clock);
        let v = Version {
            clock,
            by_listener: !self.local_is_listener,
        };
        if self.current.is_some_and(|cur| v <= cur) {
            return RemoteOutcome::Stale;
        }
        self.current = Some(v);
        match msg {
            ClipboardMsg::Other { .. } => {
                self.shared = None;
                RemoteOutcome::Noted
            }
            ClipboardMsg::Text(t) if t.text.len() > self.max_text_bytes => {
                self.shared = None;
                RemoteOutcome::TooLarge
            }
            ClipboardMsg::Text(t) => {
                self.shared = Some(hash_text(&t.text));
                RemoteOutcome::Write(t.text)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn text(s: &str) -> LocalClipboard {
        LocalClipboard::Text(s.into())
    }

    fn sent_text(o: &LocalOutcome) -> Option<(u64, &str)> {
        match &o.send {
            Some(ClipboardMsg::Text(t)) => Some((t.clock, t.text.as_str())),
            _ => None,
        }
    }

    fn msg(clock: u64, s: &str) -> ClipboardMsg {
        ClipboardMsg::Text(ClipboardText {
            clock,
            text: s.into(),
        })
    }

    #[test]
    fn local_texts_are_sent_with_increasing_clocks() {
        let mut s = ClipboardSync::new(TransportRole::Dialer, 100);
        let a = s.on_local_change(text("a"));
        assert_eq!((sent_text(&a), a.kind), (Some((1, "a")), LocalKind::Shared));
        let b = s.on_local_change(text("b"));
        assert_eq!(sent_text(&b), Some((2, "b")));
    }

    #[test]
    fn own_writes_and_echoes_are_not_resent() {
        let mut s = ClipboardSync::new(TransportRole::Dialer, 100);
        let ours = s.on_local_change(LocalClipboard::Ours);
        assert_eq!((ours.send, ours.kind), (None, LocalKind::Ours));
        s.on_local_change(text("a"));
        let again = s.on_local_change(text("a"));
        assert_eq!((again.send, again.kind), (None, LocalKind::Echo));
        // A text received from the peer and re-copied locally is an echo too.
        assert_eq!(
            s.on_remote(msg(5, "remote")),
            RemoteOutcome::Write("remote".into())
        );
        assert_eq!(s.on_local_change(text("remote")).kind, LocalKind::Echo);
        // The clock did not move for ignored changes.
        assert_eq!(sent_text(&s.on_local_change(text("x"))), Some((6, "x")));
    }

    #[test]
    fn withheld_content_sends_a_contentless_notice() {
        let mut s = ClipboardSync::new(TransportRole::Listener, 4);
        s.on_local_change(text("a"));
        for (content, kind) in [
            (LocalClipboard::Sensitive, LocalKind::Sensitive),
            (LocalClipboard::NoText, LocalKind::NoText),
            (LocalClipboard::TooLarge { bytes: 9 }, LocalKind::TooLarge),
            (text("too long"), LocalKind::TooLarge),
        ] {
            let o = s.on_local_change(content);
            assert_eq!(o.kind, kind);
            assert!(matches!(o.send, Some(ClipboardMsg::Other { .. })));
        }
        // "a" is no longer known to be shared, so copying it again sends it.
        assert_eq!(sent_text(&s.on_local_change(text("a"))), Some((6, "a")));
    }

    #[test]
    fn stale_updates_are_ignored_and_clocks_advance() {
        let mut s = ClipboardSync::new(TransportRole::Dialer, 100);
        s.on_local_change(text("a"));
        s.on_local_change(text("b")); // clock 2
        assert_eq!(s.on_remote(msg(1, "old")), RemoteOutcome::Stale);
        assert_eq!(
            s.on_remote(msg(10, "new")),
            RemoteOutcome::Write("new".into())
        );
        assert_eq!(s.on_remote(msg(9, "late")), RemoteOutcome::Stale);
        // Lamport: the next local change is newer than everything seen.
        assert_eq!(sent_text(&s.on_local_change(text("c"))), Some((11, "c")));
    }

    #[test]
    fn remote_notices_and_oversized_texts_update_the_version_only() {
        let mut s = ClipboardSync::new(TransportRole::Dialer, 3);
        s.on_local_change(text("a"));
        assert_eq!(
            s.on_remote(ClipboardMsg::Other { clock: 4 }),
            RemoteOutcome::Noted
        );
        // "a" was replaced on the peer: copying it again must resend it.
        assert_eq!(sent_text(&s.on_local_change(text("a"))), Some((5, "a")));
        assert_eq!(s.on_remote(msg(7, "long")), RemoteOutcome::TooLarge);
        assert_eq!(s.on_remote(msg(6, "abc")), RemoteOutcome::Stale);
    }

    #[test]
    fn simultaneous_copies_converge_on_the_listeners_text() {
        let mut dialer = ClipboardSync::new(TransportRole::Dialer, 100);
        let mut listener = ClipboardSync::new(TransportRole::Listener, 100);
        let from_dialer = dialer.on_local_change(text("d")).send.unwrap();
        let from_listener = listener.on_local_change(text("l")).send.unwrap();
        assert_eq!(
            dialer.on_remote(from_listener),
            RemoteOutcome::Write("l".into())
        );
        assert_eq!(listener.on_remote(from_dialer), RemoteOutcome::Stale);
    }

    #[test]
    fn debug_output_never_contains_text() {
        let secret = "hunter2";
        for dbg in [
            format!("{:?}", text(secret)),
            format!("{:?}", RemoteOutcome::Write(secret.into())),
            format!("{:?}", msg(1, secret)),
        ] {
            assert!(!dbg.contains(secret), "{dbg}");
        }
    }

    // ---- convergence under arbitrary interleavings and reordering ----

    #[derive(Debug, Clone)]
    enum Op {
        /// The user copies on side 0 or 1: `Some(text)` or something unshared.
        Copy(usize, Option<u8>),
        /// Deliver the in-flight message at this index (mod len) to side.
        Deliver(usize, usize),
    }

    fn arb_op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0usize..2, prop::option::of(0u8..4)).prop_map(|(s, t)| Op::Copy(s, t)),
            (0usize..2, any::<usize>()).prop_map(|(s, i)| Op::Deliver(s, i)),
        ]
    }

    struct Side {
        sync: ClipboardSync,
        /// What the side's clipboard shows (`None`: not shared text).
        clipboard: Option<String>,
        /// Messages in flight towards this side (any order).
        inbox: Vec<ClipboardMsg>,
    }

    fn deliver(sides: &mut [Side; 2], to: usize, index: usize) {
        if sides[to].inbox.is_empty() {
            return;
        }
        let i = index % sides[to].inbox.len();
        let m = sides[to].inbox.swap_remove(i);
        if let RemoteOutcome::Write(t) = sides[to].sync.on_remote(m) {
            sides[to].clipboard = Some(t);
        }
    }

    proptest! {
        #[test]
        fn prop_both_sides_converge(ops in prop::collection::vec(arb_op(), 0..60)) {
            let mut sides = [
                Side { sync: ClipboardSync::new(TransportRole::Dialer, 100), clipboard: None, inbox: vec![] },
                Side { sync: ClipboardSync::new(TransportRole::Listener, 100), clipboard: None, inbox: vec![] },
            ];
            let mut last_copy: Option<(Version, Option<String>)> = None;
            for op in ops {
                match op {
                    Op::Copy(s, t) => {
                        let content = match t {
                            Some(n) => LocalClipboard::Text(format!("t{n}")),
                            None => LocalClipboard::NoText,
                        };
                        let shown = t.map(|n| format!("t{n}"));
                        let out = sides[s].sync.on_local_change(content);
                        sides[s].clipboard = shown.clone();
                        if let Some(m) = out.send {
                            sides[1 - s].inbox.push(m);
                        }
                        let v = sides[s].sync.current.unwrap();
                        if last_copy.as_ref().is_none_or(|(lv, _)| v > *lv) {
                            last_copy = Some((v, shown));
                        }
                    }
                    Op::Deliver(s, i) => deliver(&mut sides, s, i),
                }
            }
            for s in 0..2 {
                while !sides[s].inbox.is_empty() {
                    deliver(&mut sides, s, 0);
                }
            }
            prop_assert_eq!(sides[0].sync.current, sides[1].sync.current);
            // When the newest copy is shared text, both clipboards show it.
            if let Some((_, Some(newest))) = last_copy {
                prop_assert_eq!(sides[0].clipboard.as_deref(), Some(newest.as_str()));
                prop_assert_eq!(sides[1].clipboard.as_deref(), Some(newest.as_str()));
            }
        }
    }
}
