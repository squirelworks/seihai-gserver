Notes for the next seihai-gserver release.

## Added

- A client's trigger action (`PLI_TRIGGERACTION`, 38) goes to the other players who read 48, with the sender's id first, and to the NPC-server as `TRIGGER_ACTION` (the action name, the level, and the half-tile point). A new non-empty chat line goes to the NPC-server as `PM_TO_NPC`.
- Stand-ins until `seihai-core` has profiles for them: 1.25 (`GSERV009`) and 1.27 (`GSERV011`) are served as 1.21, 1.40 rev 1 (`GNW23090`) as 1.41 rev 1, and 2.19, 2.2 and 2.21 (`GNW15053`, `GNW28063`, `GNW01113`, type 4) as 2.17.1.
- **2.19 to 2.21 on an INVENTED gen-4 link.** No 2.19 to 2.21 binary survives and no type-4 login has been captured. graal-client-repro's inferred 2.19, 2.2 and 2.21 clients use the type-4 framing this server serves (bzip2 bodies, the gen-5 XOR at its bz2 limit, no compression byte) as an invented link, made up to work rather than observed. A real capture (3.0beta or 3.01, also type 4) would replace it.
