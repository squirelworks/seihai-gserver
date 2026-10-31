# Lab run: the Seihai secure login (SEI00001), 2026-10-31

Loopback only (`127.0.0.1`) in a cloud container. Server: this branch,
release build, the sample `world/` copied to a temp folder, run with
`--create-accounts`. Client: `seihai-mae` from seihai's
`secure-login-sei00001` branch, a fresh settings folder (`HOME` in the temp
folder), headless under `xvfb-run`.

| Run | Before (main) | Now |
|---|---|---|
| `--version 2.05 --secure-login` | refused: `SEI00001` is not a known token | `trusting its key SHA256:…` (first visit), `logged in`, `level from the server's board`; server: `test1 [SEI00001 inner GNW03081 2.05, Gen3] logged in as player 1` |
| `--version 2.31 --secure-login` | refused | pin matched, `logged in`, level loaded; server: `[SEI00001 inner GNW28015 2.31, Gen5]` |
| wrong password, secure | — | `disconnected: Account name or password is wrong.` |
| classic logins (`tests/login_matrix.rs`) | pass | pass, unchanged |

A byte-logging relay on the earlier scaffold run showed the classic 2.31
frame carrying `test1` and the password in plain zlib, and the secure one
carrying only base64 (C1, S1, C2); see seihai `documentation/secure-login.md`
"Lab run". No real Graal client was used: they never send this token.
