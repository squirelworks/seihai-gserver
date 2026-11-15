# Early networking and explicit endpoints

The relay command and the normal world-server command serve different
protocols. Choosing a version label does not convert one into the other.

| Family | Original transport and state | This server |
|---|---|---|
| Zelda Online 1 | Single player; no networking | No server connection is applicable |
| Zelda Online 2, Java Graal, Delphi betas through 1.1.9 | Uncompressed TCP lines; named rooms and paired peers; game state in peer payloads; no account token. Clients obtain levels separately | Optional `relay` listener, no world or level transfer |
| 1.21–1.32, including 1.28 beta 2 | Direct central GSERV connection, account/password and version token, length-prefixed zlib frames; server supplies levels. 1.28 beta 2 also exchanges gameplay directly between peers over UDP | Normal world server supports the central protocol; original early-player rendering and peer UDP parity remain incomplete |

These original contracts come from the canonical research:
[relay protocol](https://github.com/squirelworks/Graal_analysis/blob/ab9a7d74b606ac4751579b8c2e349a13761931ca/protocol/relay-1998.md),
[GSERV protocol](https://github.com/squirelworks/Graal_analysis/blob/ab9a7d74b606ac4751579b8c2e349a13761931ca/protocol/gserv-1999.md),
[room interoperability](https://github.com/squirelworks/Graal_analysis/blob/ab9a7d74b606ac4751579b8c2e349a13761931ca/client-internals/relay-room-bridge.md),
[qualified early native runs](https://github.com/squirelworks/Graal_analysis/blob/ab9a7d74b606ac4751579b8c2e349a13761931ca/client-internals/live-trace-early.md),
and the [full-party and availability contract](https://github.com/squirelworks/Graal_analysis/blob/ab9a7d74b606ac4751579b8c2e349a13761931ca/client-internals/precursors-and-fan-builds.md#the-gserv-lobby).
The research distinguishes static findings, captured behavior and inference.

## Run a small relay

```sh
seihai-gserver relay --bind 127.0.0.1 --port 14801 --max-clients 128
```

No world folder, account creation, key file or listserver registration is
required. The bind defaults to loopback; select another interface explicitly
only for a network you intend to serve. The limit counts TCP connections,
not players: Delphi relay clients normally use a level connection and a
separate game-wide connection. Java clients normally use one.

Point a client whose original connection UI supports it at that explicit
endpoint. Keep the original executable unchanged and use an owned copy of
its normal settings. Some builds have fixed or disabled address controls;
this command does not unlock them or establish an unpatched route for every
build. A Java applet may obtain its relay host from its codebase and use a
fixed port; its codebase setup is distinct from relay service.

The relay announces local-level mode (`NR`, then `NL`) after a room join,
matching the recorded local-mode 1.1.9 server. Clients need compatible local
assets; HTTP level hosting is not supplied. Test Java Graal's fixed relay
port with an explicitly selected listener port where appropriate, for example
`--port 14802`; neither command is a modern listserver.

## Keep room identity and peer state

The listener implements room join (`N`), identity (`I`), availability (`+` and
`-`), pairing (`P`), party leave (`C`) and data forwarding (`D`). It keeps
peer payload bytes unchanged, including non-UTF-8 bytes. It does not create
positions, replay a board, elect a game leader or run NPCs. Those are peer
client responsibilities for this protocol.

Room names must match exactly. Historical captures show Java Graal and
**4th Graal alpha** meeting in `G2<level>` without translation. The captured
Java/5th Graal meeting used an explicit `G2` to `G3` room-renaming proxy.
This listener supplies no implicit rename. Matching a room is a necessary
pairing condition, not proof of complete cross-version gameplay support.

Repeated pairing requests are idempotent. The original Java relay could
duplicate party edges and deliver data twice when both peers requested a
pair simultaneously. This implementation deliberately delivers once per
partner. Input lines, connections and outgoing queues are bounded; malformed
or oversized input and slow readers may be disconnected. These defensive
limits are implementation policy, not newly observed original behavior.
Observer/super-user commands are outside this first listener.

## Central 1999 sessions remain separate

For a normal account/world session, use the existing command, for example:

```sh
seihai-gserver --world world --bind 127.0.0.1 --port 14804
```

Use the normal account command or the explicitly enabled lab account-creation
setting. A direct GSERV client does not need a listserver merely to contact
this endpoint. `rules = 1999` selects documented game rules within the central
server; it does not turn that server into the earlier relay architecture.

Historical original 1.21 and 1.28 beta 2 pair captures exist. Their Wine input
fixes, the 1.28 UDP receive fix and imported levels must remain attached to
those results. A synthetic login matrix is not an original-client test.
Issue [#132](https://github.com/squirelworks/seihai-gserver/issues/132) still
tracks invisible early players. The retained-source introduction-order and
level-property differences are hypotheses to test separately; relay support
does not resolve that issue or establish original peer UDP compatibility.

## Validation boundary

`tests/relay.rs` uses generated loopback peers to check actual stream
forwarding, exact `G2` versus `G3` room separation, pairing, fragmented lines,
disconnect cleanup and input bounds. It is transport regression coverage,
not an original Java/Delphi run against this listener. Native acceptance must
pin the unchanged clients, their assets/settings, endpoints and observed
results independently. Preserved archive membership is useful provenance;
it does not by itself authenticate a pristine vendor release.
