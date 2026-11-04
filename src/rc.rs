//! RC and NC logins (stage 4, skeleton).
//!
//! `login::parse` refuses RC (type 1, and 6 for the newer RCs) and NC
//! (type 3) as "not served yet", and the refusal goes out in that type's
//! framing (gen 5 for type 6) so the client shows it. Their tokens are in
//! Graal_analysis `protocol/login-and-walk.md`. Stage 4 accepts them for
//! staff accounts (a `STAFF` key in the account file) and serves the player
//! list, server options and file browser `seihai-rc` needs.
//!
//! Lessons from the real-RC capture against the gserver-v2 fork
//! (2026-11-04, lab notes `rc-capture-2026-11-04/rc-layouts.md`):
//!
//! - Folder rights (`RC_PLAYERRIGHTSGET` 62 / `RC_PLAYERRIGHTSSET` 84) come
//!   in as Graal CSV, one item per line, quoted when the line has a space
//!   (`"rw levels/*"`). Store the lines and send them back CSV-encoded the
//!   same way. The fork returned them plain, and the real RC split
//!   `rw levels/*` at the space into two lines, so saving again from the RC
//!   would store the wrong rights. Comments (63 / 86) and the ban reason
//!   (64 / 88) are CSV too.
//! - Setting a ban (`RC_PLAYERBANSET` 88) on a player who is online hung
//!   the fork at 100 % CPU. Here the ban must disconnect the player through
//!   the world task like any other leave, and a test must ban an online
//!   player and then read the ban back.
