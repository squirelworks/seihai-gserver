//! RC and NC logins (stage 4, skeleton).
//!
//! `login::parse` refuses RC (type 1, and 6 for the newer RCs) and NC
//! (type 3) as "not served yet", and the refusal goes out in that type's
//! framing (gen 5 for type 6) so the client shows it. Their tokens are in
//! Graal_analysis `protocol/login-and-walk.md`. Stage 4 accepts them for
//! staff accounts (a `STAFF` key in the account file) and serves the player
//! list, server options and file browser `seihai-rc` needs.
