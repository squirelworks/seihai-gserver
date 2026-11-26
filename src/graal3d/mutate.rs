//! Lab only: one seeded mutation of what the server sends each 3D client,
//! for fuzzing the real clients in a lab VM (`graal3d_mutate`). Off by
//! default; never on a public server.
//!
//! The accept mutations change the connect accept. The others go in one
//! data packet, 2 seconds after the client's mission start is done, and
//! the log gets what changed and the packet's bytes. The seeds pick
//! values with this server's own generator, so a seed here does not give
//! the same packet as the same seed on seihai-gserver-v2.

use super::handshake::Profile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutation {
    /// The accept's protocol number is wrong.
    AcceptProtocol,
    /// The accept's class table drops, adds or renames a class.
    AcceptClassTable,
    /// An event with a class id the table does not have.
    EventUnknownClass,
    /// An event whose int14 length does not match its payload.
    EventBadLength,
    /// An update for a ghost index never created.
    GhostUncreated,
    /// Two deletes of an index that holds no ghost.
    GhostDoubleDelete,
    /// A ghost section with the wrong 4-bit index width.
    GhostIndexWidth,
    /// The control object names a ghost of the wrong class.
    ControlWrongClass,
    /// The control object names an index that holds no ghost.
    ControlDeleted,
}

const NAMES: [(Mutation, &str); 9] = [
    (Mutation::AcceptProtocol, "accept-protocol"),
    (Mutation::AcceptClassTable, "accept-classtable"),
    (Mutation::EventUnknownClass, "event-unknown-class"),
    (Mutation::EventBadLength, "event-bad-length"),
    (Mutation::GhostUncreated, "ghost-uncreated"),
    (Mutation::GhostDoubleDelete, "ghost-double-delete"),
    (Mutation::GhostIndexWidth, "ghost-index-width"),
    (Mutation::ControlWrongClass, "control-wrong-class"),
    (Mutation::ControlDeleted, "control-deleted"),
];

impl Mutation {
    pub fn name(self) -> &'static str {
        NAMES
            .iter()
            .find(|(m, _)| *m == self)
            .map_or("none", |(_, n)| n)
    }

    pub fn in_accept(self) -> bool {
        matches!(self, Mutation::AcceptProtocol | Mutation::AcceptClassTable)
    }
}

/// `ghost-uncreated`, `accept-protocol:42`: the kind and its seed (1 by
/// default). `None` for `off`, an empty value or an unknown kind.
pub fn parse(text: &str) -> Option<(Mutation, u32)> {
    let (kind, seed) = match text.split_once(':') {
        Some((k, s)) => (k, s.parse().ok()?),
        None => (text, 1),
    };
    NAMES
        .iter()
        .find(|(_, n)| *n == kind)
        .map(|(m, _)| (*m, seed))
}

/// The seeded generator, fixed so a seed gives the same packet on every
/// run and build of this server: state = seed x 0x9E3779B9 (wrapping) with
/// the low bit set; each draw is xorshift32 (x ^= x << 13; x ^= x >> 17;
/// x ^= x << 5) and returns the new state; `below(n)` is a draw mod n. A
/// mutation draws in the order its kind lists in `docs/GRAAL3D.md`.
#[derive(Debug, Clone)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Rng(seed.wrapping_mul(0x9e37_79b9) | 1)
    }

    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// 0 to n - 1 (n > 0).
    pub fn below(&mut self, n: u32) -> u32 {
        self.next_u32() % n.max(1)
    }
}

/// What an accept mutation changes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AcceptTweak {
    pub protocol: Option<u32>,
    pub drop_events: usize,
    pub extra_events: Vec<&'static str>,
    pub rename_object: Option<(usize, &'static str)>,
}

/// The accept tweak for an accept mutation (default for the others), and
/// a line saying what it changed.
pub fn accept_tweak(m: Mutation, seed: u32, profile: &Profile) -> (AcceptTweak, String) {
    let mut t = AcceptTweak::default();
    let mut rng = Rng::new(seed);
    let said = match m {
        Mutation::AcceptProtocol => {
            let p = profile.protocol;
            let values = [p - 1, p + 1, 0, u32::MAX, p + 100];
            let v = values[rng.below(values.len() as u32) as usize];
            t.protocol = Some(v);
            format!("protocol {v} in place of {p}")
        }
        Mutation::AcceptClassTable => match rng.below(3) {
            0 => {
                t.drop_events = 1 + rng.below(3) as usize;
                format!("the last {} event classes left out", t.drop_events)
            }
            1 => {
                t.extra_events = vec!["SeihaiLabEvent"];
                "an extra event class SeihaiLabEvent".into()
            }
            _ => {
                let i = rng.below(profile.objects.len() as u32) as usize;
                t.rename_object = Some((i, "SeihaiLabObject"));
                format!(
                    "object class {i} ({}) renamed SeihaiLabObject",
                    profile.objects.get(i).copied().unwrap_or("?")
                )
            }
        },
        _ => String::new(),
    };
    (t, said)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graal3d::handshake::PROTOCOL_24;

    /// The generator is part of the documented format: these values must
    /// never change, or a logged seed no longer reproduces its packet.
    #[test]
    fn the_generator_is_pinned() {
        let mut r = Rng::new(1);
        let got: Vec<u32> = (0..4).map(|_| r.next_u32()).collect();
        let mut x = 0x9e37_79b9u32 | 1;
        let want: Vec<u32> = (0..4)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x
            })
            .collect();
        assert_eq!(got, want);
        assert_eq!(got[0], 0x510c_4619);
    }

    #[test]
    fn kinds_and_seeds_are_read() {
        assert_eq!(
            parse("ghost-uncreated"),
            Some((Mutation::GhostUncreated, 1))
        );
        assert_eq!(
            parse("accept-protocol:42"),
            Some((Mutation::AcceptProtocol, 42))
        );
        assert_eq!(parse("off"), None);
        assert_eq!(parse("ghost-uncreated:x"), None);
        for (m, n) in NAMES {
            assert_eq!(m.name(), n);
        }
    }

    #[test]
    fn a_seed_gives_the_same_tweak_each_time() {
        let a = accept_tweak(Mutation::AcceptProtocol, 7, &PROTOCOL_24);
        assert_eq!(a, accept_tweak(Mutation::AcceptProtocol, 7, &PROTOCOL_24));
        assert_ne!(a.0.protocol, Some(24));
        assert_eq!(
            accept_tweak(Mutation::GhostUncreated, 7, &PROTOCOL_24).0,
            AcceptTweak::default()
        );
    }
}
