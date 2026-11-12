//! The ping (issue #44): `PLO_PING` (108) out on a timer, the client's
//! `PLI_PONG` (162) back, the round trip timed.
//!
//! Only builds whose profile handles 108 are sent it: 4.20r6 and 4.20r8
//! in `seihai-core` v0.0.6. Nothing before 4.20 and no 5.x reads it. The
//! Graal_analysis note `protocol/unknown-ids-2026-11-06.md` also lists
//! 6.01r1 and 6.03r7, but their profiles do not have 108 yet, so they wait
//! for the profile. The layout is `{108}{GCHAR 0}{GSHORT value}`; with sub
//! 0 the client answers `{162}{'!'}{GSHORT min(value, 0x6FFF)}`. Sub 1 is
//! never sent: what the client does with it was not traced. No lab has
//! seen the answer on the wire yet.
//!
//! An RC sends 162 empty every 60 s with no 108 first; `rc.rs` takes that
//! quietly, and an empty 162 here is no answer.
//!
//! `config/server.txt`: `ping` is the interval in seconds (30 by default,
//! 0 for none) and `ping_misses` the pings in a row left unanswered before
//! the client is dropped (0, the default, never drops). Each round trip is
//! in the log when the server runs with `--verbose`.

use crate::wire::{gchar, gshort, ids::PLO_PING, msg};
use seihai_core::protocol::read_gshort;
use std::time::{Duration, Instant};

/// The highest value a client sends back.
pub const MAX_VALUE: u16 = 0x6FFF;

/// One connection's pings.
#[derive(Debug, Default)]
pub struct Pinger {
    last: u16,
    /// The ping in flight: its value and when it went.
    out: Option<(u16, Instant)>,
    /// Pings in a row that got no answer before the next one went.
    pub missed: u32,
}

impl Pinger {
    /// The next `PLO_PING`. A ping still in flight counts as missed.
    pub fn ping(&mut self, now: Instant) -> Vec<u8> {
        if self.out.is_some() {
            self.missed += 1;
        }
        self.last = if self.last >= MAX_VALUE {
            1
        } else {
            self.last + 1
        };
        self.out = Some((self.last, now));
        let mut p = vec![gchar(0)];
        gshort(self.last, &mut p);
        msg(PLO_PING, &p)
    }

    /// A `PLI_PONG` payload: the round trip when it answers the ping in
    /// flight. An empty one (an RC's keepalive) or a stale value is none.
    pub fn answer(&mut self, payload: &[u8], now: Instant) -> Option<Duration> {
        let value = read_gshort(payload.get(1..)?)?;
        let (sent, at) = self.out?;
        if value != sent {
            return None;
        }
        self.out = None;
        self.missed = 0;
        Some(now.saturating_duration_since(at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seihai_core::protocol::split_messages;

    fn pong(v: u16) -> Vec<u8> {
        let mut p = vec![b'!'];
        gshort(v, &mut p);
        p
    }

    #[test]
    fn pings_answers_and_misses() {
        let t0 = Instant::now();
        let mut p = Pinger::default();
        let m = p.ping(t0);
        let m = &split_messages(&m)[0];
        assert_eq!(m.id, PLO_PING);
        assert_eq!(m.payload, [0x20, 0x20, 0x21]);
        assert_eq!(p.answer(b"", t0), None, "an RC keepalive");
        assert_eq!(p.answer(&pong(2), t0), None, "not this ping");
        let rtt = p.answer(&pong(1), t0 + Duration::from_millis(40));
        assert_eq!(rtt, Some(Duration::from_millis(40)));
        assert_eq!(p.answer(&pong(1), t0), None, "answered once");
        p.ping(t0);
        p.ping(t0);
        p.ping(t0);
        assert_eq!(p.missed, 2);
        assert!(p.answer(&pong(4), t0).is_some());
        assert_eq!(p.missed, 0);
        // The value wraps below the client's cap.
        p.last = MAX_VALUE;
        p.ping(t0);
        assert!(p.answer(&pong(1), t0).is_some());
    }
}
