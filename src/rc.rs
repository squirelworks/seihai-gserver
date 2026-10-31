//! RC and NC logins (stage 4, skeleton).
//!
//! `login::parse` refuses login types other than 0 and 5 with "not served
//! yet". RC is type 1 (and 6 for RC2), NC type 3; their tokens are in
//! Graal_analysis `protocol/login-and-walk.md`. Stage 4 accepts them for
//! staff accounts (a `STAFF` key in the account file) and serves the player
//! list, server options and file browser `seihai-rc` needs.
