//! Login scheduling: a per-address login rate and a cap on password checks
//! running at once (issue #17).
//!
//! A password check is PBKDF2 at 100 000 rounds, about 0.15 CPU-seconds.
//! Without a cap, a burst of logins (a restart, a network blip, a ban wave
//! reconnecting) fans out over the blocking pool, hundreds at once, and
//! every login in the burst finishes late. With the cap the burst queues
//! and logins finish in order. The rounds stay as they are: this is about
//! scheduling, not a weaker hash.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// The table is pruned of full buckets once it holds this many addresses.
const PRUNE_AT: usize = 4096;

/// Logins a minute per address: a token bucket per address, as large as
/// the rate, refilled at the rate.
pub struct LoginRate {
    per_minute: u32,
    loopback: bool,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

impl LoginRate {
    /// `per_minute` 0 lets every login through. `loopback` false leaves
    /// loopback addresses out of the limit.
    pub fn new(per_minute: u32, loopback: bool) -> Self {
        LoginRate {
            per_minute,
            loopback,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Take one login for `ip`; false when it is over its rate.
    pub fn allow(&self, ip: IpAddr) -> bool {
        self.allow_at(ip, Instant::now())
    }

    fn allow_at(&self, ip: IpAddr, now: Instant) -> bool {
        let ip = key(ip);
        if self.per_minute == 0 || (ip.is_loopback() && !self.loopback) {
            return true;
        }
        let cap = f64::from(self.per_minute);
        let per_sec = cap / 60.0;
        let fill = |b: &Bucket| {
            (b.tokens + per_sec * now.saturating_duration_since(b.at).as_secs_f64()).min(cap)
        };
        let mut m = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() >= PRUNE_AT {
            m.retain(|_, b| fill(b) < cap);
        }
        let b = m.entry(ip).or_insert(Bucket {
            tokens: cap,
            at: now,
        });
        b.tokens = fill(b);
        b.at = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// The address a login is counted under: IPv4 as it is (a v4-mapped IPv6
/// address counts as its IPv4 one), IPv6 by its /64, which one host or
/// home network usually holds whole.
fn key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None if v6.is_loopback() => ip,
            None => {
                let s = v6.segments();
                IpAddr::from([s[0], s[1], s[2], s[3], 0, 0, 0, 0])
            }
        },
    }
}

/// At most `slots` password checks at once, counted so a lab run can see
/// the most that ran together.
pub struct PasswordChecks {
    slots: Arc<Semaphore>,
    running: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

/// One running password check; the slot is free again when it drops.
pub struct CheckSlot {
    _permit: OwnedSemaphorePermit,
    running: Arc<AtomicUsize>,
}

impl Drop for CheckSlot {
    fn drop(&mut self) {
        self.running.fetch_sub(1, Ordering::SeqCst);
    }
}

impl PasswordChecks {
    pub fn new(slots: usize) -> Self {
        PasswordChecks {
            slots: Arc::new(Semaphore::new(slots.max(1))),
            running: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Wait for a free slot. Move the slot into the blocking task, so it is
    /// held until the hash is done even when the connection goes away.
    pub async fn slot(&self) -> CheckSlot {
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .expect("the password-check semaphore is never closed");
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        CheckSlot {
            _permit: permit,
            running: self.running.clone(),
        }
    }

    /// The most password checks that ran at once since the start.
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn rate_refills_over_the_minute() {
        let r = LoginRate::new(3, false);
        let ip: IpAddr = "192.0.2.7".parse().unwrap();
        let other: IpAddr = "192.0.2.8".parse().unwrap();
        let t = Instant::now();
        assert!((0..3).all(|_| r.allow_at(ip, t)));
        assert!(!r.allow_at(ip, t));
        // Another address has its own bucket.
        assert!(r.allow_at(other, t));
        // One login back every 20 s at 3 a minute.
        assert!(!r.allow_at(ip, t + Duration::from_secs(19)));
        assert!(r.allow_at(ip, t + Duration::from_secs(21)));
        assert!(!r.allow_at(ip, t + Duration::from_secs(22)));
        // A long wait fills the bucket, but no further than its size.
        let later = t + Duration::from_secs(3600);
        assert!((0..3).all(|_| r.allow_at(ip, later)));
        assert!(!r.allow_at(ip, later));
    }

    #[test]
    fn loopback_and_off() {
        let t = Instant::now();
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        let r = LoginRate::new(1, false);
        assert!((0..100).all(|_| r.allow_at(lo, t)));
        let r = LoginRate::new(1, true);
        assert!(r.allow_at(lo, t));
        assert!(!r.allow_at(lo, t));
        let r = LoginRate::new(0, true);
        assert!((0..100).all(|_| r.allow_at(lo, t)));
    }

    #[test]
    fn ipv6_counts_by_its_64() {
        let r = LoginRate::new(1, false);
        let t = Instant::now();
        assert!(r.allow_at("2001:db8:1:2::1".parse().unwrap(), t));
        assert!(!r.allow_at("2001:db8:1:2::ffff".parse().unwrap(), t));
        assert!(r.allow_at("2001:db8:1:3::1".parse().unwrap(), t));
        // v4-mapped counts as the IPv4 address.
        assert!(r.allow_at("192.0.2.9".parse().unwrap(), t));
        assert!(!r.allow_at("::ffff:192.0.2.9".parse().unwrap(), t));
    }

    #[tokio::test]
    async fn checks_never_run_past_their_slots() {
        let c = Arc::new(PasswordChecks::new(2));
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let c = c.clone();
                tokio::spawn(async move {
                    let slot = c.slot().await;
                    tokio::task::spawn_blocking(move || {
                        let _slot = slot;
                        std::thread::sleep(Duration::from_millis(20));
                    })
                    .await
                    .unwrap();
                })
            })
            .collect();
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(c.peak(), 2);
    }
}
