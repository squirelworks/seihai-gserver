# seihai-gserver

A new Graal game server written in Rust on Seihai's core, for classic clients (1.33 to 2.31 first) and Seihai.

Part of the Seihai project: the client [seihai](https://github.com/squirelworks/seihai)
and its library [seihai-core](https://github.com/squirelworks/seihai-core); the Rust
server stack [seihai-gserver](https://github.com/squirelworks/seihai-gserver),
[seihai-listserver](https://github.com/squirelworks/seihai-listserver),
[seihai-npcserver](https://github.com/squirelworks/seihai-npcserver),
[seihai-rc](https://github.com/squirelworks/seihai-rc) and
[seihai-servertools](https://github.com/squirelworks/seihai-servertools); the GPL forks
[seihai-gserver-v2](https://github.com/squirelworks/seihai-gserver-v2) and
[seihai-gs2lib](https://github.com/squirelworks/seihai-gs2lib); and the research notes in
[Graal_analysis](https://github.com/squirelworks/Graal_analysis).

## Status

Stages 1 to 3 of the [plan](docs/PLAN.md) are built: clients log in, play one
shared world of levels and gmaps, and see each other's NPCs, baddies, bombs and
hits. What is still open in each stage (hits checked against real clients, RC
support, server-side NPC scripts) is listed in the plan. Contributors should
read [AGENTS.md](AGENTS.md) for the project rules.

What works today:

* login for every protocol generation: gen 2 (1.21 to 1.39), gen 3 (1.41 to
  2.17) and gen 5 (2.22 to 6.x), with `seihai-core`'s codecs;
* the version allow-list (`config/allowedversions.txt`);
* flat-file accounts (`accounts/<name>.txt`, PBKDF2 password hashes);
* the login burst (props, warp, the start level from `levels/` with its
  links, signs and chests, mod time and the rest, in the order each
  generation needs; a generated board when the level file is missing);
* other players in the level, and links between levels (`PLI_LEVELWARP`,
  `PLI_LEVELWARPMOD`; a current cached copy gets the level name alone);
* gmaps: `.gmap` files in `levels/`, played as one world by the builds that
  play gmaps (neighbour requests, walking across seams), as plain levels by
  the rest;
* files: `WANTFILE`/`UPDATEFILE` served from `<world>/files/` (any depth)
  and the `.gmap` files in `levels/`, in each build's file layout, large
  files in chunks where the build takes them;
* the account keeps what the player does: on logout their level, position,
  hearts, rupees, arrows, bombs and glove go back into the account file,
  with the chests they opened (sent opened from then on; a chest's rupees,
  bombs, darts or hearts are given) and their player flags (`PLI_FLAGSET`,
  `PLI_FLAGDEL`; sent back as `PLO_FLAGSET` at login);
* items on the floor: rolled or laid items (`PLI_ITEMADD`) show for the
  others in the level and for players who arrive while they lie there;
  `PLI_ITEMTAKE` gives the item to the first taker only, and `PLI_ITEMDEL`
  clears it for everyone;
* anti-cheat on the server, off by default: speed, teleports, warps with no
  link under the player, item raises, hits from across the level and
  message floods can be logged, put back, kicked or banned, with staff
  exempt per check if the admin wants (`anticheat.*` in
  `config/server.txt`; it only logs until the admin says more);
* bans in `config/bans.txt` by account, address (`ip:ADDR`) or machine
  (`machine:ID`, the ids a 5.00+ client sends at login), with an end time,
  the admin and a reason; `seihai-gserver ban` / `unban`. An anti-cheat
  ban takes the player's machine ids with the account. Address and machine
  bans do not keep out staff accounts;
* packages: `.gpak` and `.gpake` for the builds that load them, the key
  (`PLO_FILEDECRYPTKEY`, 104) first from `config/gpakkeys.txt`, which is never
  served; `gpaks =` pushes packages at login;
* client-side NPCs (stage 3): the level file's NPCs and baddies go to
  every player who enters; changes a client's script makes reach the
  others; one player per level leads the baddies (`PLO_ISLEADER`); bombs,
  shots, fire and blasts reach the level; hits reach their victim and kill
  claims cost the killer alignment;
* the idle session: the world time as a heartbeat;
* the NPC-server link (`PROTOCOL.md`): `seihai-npcserver` logs in on its own
  loopback port and is told the level list and every player's props. With
  `npcserver = auto` in `config/server.txt` the gserver starts
  `seihai-npcserver` from next to its own binary with a fresh token, so there is
  nothing else to set up. The NPC-server runs no scripts on players yet;
* the listserver link (`listserver = host:port`): registers the server, keeps
  its player count, and with `listserver_accounts = true` checks passwords
  with the listserver (`VERIACC2`). The link is encrypted by default
  (`SVLINK01` in Seihai's `documentation/secure-login.md`), so the password
  of every player, real Graal clients included, crosses it sealed. The
  listserver's key is pinned in `config/listserver.pin` on first contact, or
  given as `listserver_key`.

RC support and server-side NPC scripts are later stages.

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
`--verbose` logs every message id the server does not handle yet. Every
setting is listed with its default in
[`world/config/server.txt`](world/config/server.txt).

Then log in with Seihai (see its README) or a real client on loopback:

```
seihai-mae --online 127.0.0.1:14802 --account test1 --password-env GRAAL_PW --version 2.05
```

### The 1998 relay

For the older, account-free room/peer protocol, use the separate relay:

```sh
seihai-gserver relay --bind 127.0.0.1 --port 14801
```

It forwards peer state without interpreting or rewriting it. Clients supply
their own compatible levels; it does not serve a world or require a listserver.
See
[early networking and explicit endpoints](docs/ANCIENT-NETWORKING.md) for the
1998 relay versus 1999 GSERV distinction, room compatibility and test limits.

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

## Releases

Prebuilt Linux, Windows and macOS archives are on the
[Releases page](https://github.com/squirelworks/seihai-gserver/releases), built by
GitHub Actions for each `v*` tag. [VERSIONING.md](VERSIONING.md) explains when a
release is made, what the version numbers mean and what 1.0.0 needs.

## Licence

`MIT OR Apache-2.0`, at your option (see `LICENSE-MIT` and `LICENSE-APACHE`).
This repo contains no code from GServer-v2, gs2lib or gs2-parser (GPL) and must not.
