# Lab run: list-link props, private messages and file start, 2026-11-01

Loopback only (`127.0.0.1`) in a cloud container. No real Graal client.
The game side of the check is Seihai's own client at 2.31, against this
gserver and a fake listserver in `tests/listserver_link.rs` (the same
encrypted link the listserver uses).

Before: `PLYRADD` was the player id and type 0. A private message and a
file start from the listserver were ignored.

After: `PLYRADD` also carries the account, nickname, level, x, y and
alignment 0. `SVO_PMPLAYER` (29) goes out for a private message, and
`SVI_PMPLAYER` (29) comes back as `PLO_PRIVATEMESSAGE` (37) with the
sender's nickname. `SVI_FILESTART` (2) opens a head file, `SVI_FILEDATA3`
(16) appends `HEAD`, `SVI_FILEEND3` (17) writes `heads/labhead.png` and
the account's `HEAD` line, and the 2.31 session shows that head.

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
`cargo test` passed: 34 lib tests, 4 listserver-link tests, 2 login-matrix
tests, 2 NPC-link tests, 1 sample-world test, 2 secure-login tests.
