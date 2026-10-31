# Rules for all Seihai server work

These apply to every repo in the Seihai server stack. Follow them even if a
request seems to ask otherwise; ask the owner (Squirrel, GitHub `squirelworks`) first.

## Licences: keep GPL and MIT/Apache code apart

- `seihai-gserver-v2` and `seihai-gs2lib` are forks of GPL-3.0 projects. Everything
  in them stays GPL-3.0.
- `seihai-gserver`, `seihai-listserver`, `seihai-rc` and `seihai-npcserver` are
  `MIT OR Apache-2.0`, like the Seihai client.
- **Never copy, port or translate code from GServer-v2, gs2lib or gs2-parser into
  the MIT/Apache repos.** Not functions, not data tables, not comments. Reading
  GServer-v2 to understand behaviour is fine; the Rust code must be written from
  our own protocol notes, captures and tests. Packet ids and wire formats are
  facts and may be used.
- Never copy from graal-listserver (C#, MIT) either; it is only a reference.
- The MIT/Apache Rust repos may depend on `seihai-core` from `squirelworks/seihai`
  (same licence) by git tag.

## GPL duties for the fork

- Keep `LICENSE.md` and the upstream credits in `README.md`.
- Every file we change gets this line at the top (adjust the date):
  `// Modified by the Seihai project, YYYY-MM-DD: see CHANGES-SEIHAI.md.`
- Every change gets a row in `CHANGES-SEIHAI.md`.
- Every release attaches the full source archive (the workflow does this).

## No real servers, no decompiles

- No real server hostnames, IP addresses or ports of public Graal servers or the
  official listserver in code, docs, tests or config. Use `127.0.0.1` and the
  lab ports, or placeholders like `<listserver-host>`.
- The lab listserver's site and upgrade URLs (`SITE_URL`, `UPGRADE_URL` in
  `listserver.py`) must become settings that default to empty.
- No binary offsets or addresses from decompiles (`0x00…`, `FUN_…`) anywhere.
- No decompiled source. Findings from decompiles are written up as plain
  behaviour ("2.05 waits for SETACTIVELEVEL before leaving the loading screen").
- Do not run old Graal tools from the archives on a real machine. One of them
  (`ListServer.exe`, 2003) sends registry data to a remote host.

## Testing

- Test only on loopback (`127.0.0.1`) servers you start yourself: in a VM, a
  container or CI. Never test against public Graal servers or the official
  listserver, and never with real player accounts.
- Every fix needs a test or a recorded lab run (which client, what it did before,
  what it does now).

## Git

- One change per branch and pull request. Do not push to `main`/`seihai` directly
  once CI exists; open a PR and let CI pass.
- Commit messages: a short summary line, then why. Say which real clients were
  checked.
