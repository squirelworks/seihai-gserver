# seihai-gserver

A new Graal game server written in Rust on Seihai's core, for classic clients (1.33 to 2.31 first) and Seihai.

Part of the Seihai server stack:
[seihai-gserver](https://github.com/squirelworks/seihai-gserver) ·
[seihai-npcserver](https://github.com/squirelworks/seihai-npcserver) ·
[seihai-listserver](https://github.com/squirelworks/seihai-listserver) ·
[seihai-rc](https://github.com/squirelworks/seihai-rc) ·
[seihai-gserver-v2](https://github.com/squirelworks/seihai-gserver-v2) (GPL fork of GServer-v2, ships first) ·
client: [seihai](https://github.com/squirelworks/seihai).

**Status: stage 1.** Clients log in and stay connected; there is no world yet.
What works, by stage, is in [docs/PLAN.md](docs/PLAN.md); the rules every
contributor and coding agent follows are in [AGENTS.md](AGENTS.md).

Stage 1 covers:

* login for every protocol generation: gen 2 (1.21 to 1.39), gen 3 (1.41 to
  2.17) and gen 5 (2.22 to 6.x), with `seihai-core`'s codecs;
* the version allow-list (`config/allowedversions.txt`);
* flat-file accounts (`accounts/<name>.txt`, PBKDF2 password hashes);
* the login burst (props, warp, level name, a generated board, mod time and
  the rest, in the order each generation needs);
* the idle session: file requests answered, the world time as a heartbeat;
* the NPC-server link (`PROTOCOL.md`): `seihai-npcserver` logs in on its own
  loopback port and is told the level list and every player's props. With
  `npcserver = auto` in `config/server.txt` the gserver starts
  `seihai-npcserver` from next to its own binary with a fresh token, so there is
  nothing else to set up. The NPC-server runs no scripts on players yet.

* the listserver link (`listserver = host:port`): registers the server, keeps
  its player count, and with `listserver_accounts = true` checks passwords
  with the listserver (`VERIACC2`). The link is encrypted by default
  (`SVLINK01` in Seihai's `documentation/secure-login.md`), so the password
  of every player, real Graal clients included, crosses it sealed. The
  listserver's key is pinned in `config/listserver.pin` on first contact, or
  given as `listserver_key`.

Levels, other players, NPCs and RC are later stages.

## Build and run

```
cargo build --release
cargo test

# the sample world in world/ listens on 127.0.0.1:14802
./target/release/seihai-gserver --world world
# make an account (the password comes from an environment variable)
GRAAL_PW=labpw ./target/release/seihai-gserver account test1 --password-env GRAAL_PW --world world
```

`--create-accounts` (or `create_accounts = true` in `config/server.txt`)
makes an account on its first login instead; use it in a lab only.
`--verbose` logs every message id the server does not handle yet.

Then log in with Seihai (see its README) or a real client on loopback:

```
seihai-mae --online 127.0.0.1:14802 --account test1 --password-env GRAAL_PW --version 2.05
```

### Secure login (SEI00001)

Seihai clients started with `--secure-login` send the version token
`SEI00001`: the account and password go encrypted (X25519 key exchange,
Ed25519-signed server hello pinned by the client, ChaCha20-Poly1305), and
the session then runs as the `--version` build. Spec:
[Seihai `documentation/secure-login.md`](https://github.com/squirelworks/seihai/blob/main/documentation/secure-login.md).

On first start the server makes its key in `config/seihai-login.key` and
prints the fingerprint: publish it so players can check it, keep the file
private and back it up. `secure_login_key =` (empty) turns secure logins
off; `require_secure_login = true` refuses classic logins (real clients then
cannot log in). Classic logins are unchanged.

```
seihai-mae --online 127.0.0.1:14802 --account test1 --password-env GRAAL_PW --version 2.31 --secure-login
```

The settings are listed with their defaults in
[`world/config/server.txt`](world/config/server.txt).

Releases for Linux, Windows and macOS are built by GitHub Actions on each `v*` tag.

## Licence

`MIT OR Apache-2.0`, at your option (see `LICENSE-MIT` and `LICENSE-APACHE`).
This repo contains no code from GServer-v2, gs2lib or gs2-parser (GPL) and must not.
