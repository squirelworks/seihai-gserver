//! The NPC-server link (stage 5, skeleton).
//!
//! `seihai-npcserver` logs in on its own port with our own message set,
//! framed like gen 5, and documented in `PROTOCOL.md` in both repos (see the
//! handoff's `05-npcserver.md`). With `npcserver = auto` in `server.txt` the
//! gserver starts it and hands it a one-time token.
//!
//! The login it builds on is stage 1's: `login.rs` (parse), `codec.rs`
//! (frames) and `server.rs` (one task per connection).
