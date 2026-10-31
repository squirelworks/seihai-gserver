# Lab run: the encrypted listserver link, 2026-10-31

Loopback only (`127.0.0.1`) in a cloud container. Release builds:
seihai-listserver from its `secure-gserver-link` branch and this branch,
both on seihai-core rev `637ade3`; client `seihai-mae` headless under
`xvfb-run`, fresh settings folder. One listserver account `test1` / `labpw`
(`add-account`). No real Graal client or server was involved.

Listserver: gserver port 14900, client port 14922, `accounts = "accounts.txt"`,
`require_secure_gserver_link = true`. Gserver: an empty world with
`listserver = 127.0.0.1:14900`, `listserver_accounts = true`, no local
accounts, `create_accounts` off.

| Run | Result |
|---|---|
| Gserver start | `first visit to 127.0.0.1:14900; trusting its key SHA256:…` (written to `config/listserver.pin`), `registered (encrypted)`; listserver: `encrypted link (SEI00001 SVLINK01)`, `register seihai-gserver 0.0.1`, `newserver Link Lab port 14802` |
| `--list-only --version 2.22` on 14922 | `Link Lab 127.0.0.1:14802 2.31 players: 0` |
| Game login, classic 2.31 (as a real client logs in) | gserver `logged in as player 1`; listserver `veriacc2 test1 type 0: ok`, `player add 1`, then `player rem 1` on leave |
| Game login, `--secure-login --version 2.05` | `[SEI00001 inner GNW03081 2.05, Gen3] logged in as player 2`; listserver `veriacc2 … ok` |
| Wrong password | gserver refuses with `Account name or password is wrong.`; listserver `refused, Your password is incorrect.` |
| Listserver stopped, then a login | `The account server cannot be reached; try again later.`; link retries every 15 s |
| Account file made on the first checked login | `accounts/test1.txt` with an empty `PASSWORD` line: the state lives here, the password at the listserver |

What crossed the wire is checked in `tests/listserver_link.rs` (a fake
listserver on the same seihai-core code taps every raw byte): no password,
no account name and no server name appear; with a wrong `listserver_key`
only the C1 line crosses and no pin file is written.
