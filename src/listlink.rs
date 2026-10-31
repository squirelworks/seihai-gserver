//! The listserver link (skeleton).
//!
//! To do (see the handoff's `03-listserver.md` and `seihai-listserver`):
//! connect to `<listserver-host>` (a setting, empty by default, so a lab
//! server never registers anywhere unless told to), send the register line,
//! then zlib frames: new server (name, description, language, version, URL,
//! IP, port), player count and add/remove, account checks for logins
//! (`VERIACC2`), and a keep-alive. Until then `tools/lab-register.py` lists a
//! stage 1 server on the lab listserver by hand.
