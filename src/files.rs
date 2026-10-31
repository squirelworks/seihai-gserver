//! File serving (stage 2, skeleton).
//!
//! Stage 1 answers `PLI_WANTFILE` (23) with `PLO_FILESENDFAILED` (30) and
//! `PLI_UPDATEFILE` (34) with `PLO_FILEUPTODATE` (45), each only where the
//! build reads that id (`server::Session::handle`).
//!
//! To do: serve files from `<world>/` folders. `PLO_FILE` (102) carries a
//! GINT5 mod time from 2.1 on (`profile.files.modtime`); files over 64 KB go
//! as `PLO_LARGEFILESTART`/`END` (68/69) where `files.large_files` is set and
//! `PLO_LARGEFILESIZE` (84) where `files.large_file_size` is set. Send at most
//! a few files per tick so one big download does not stall the world. Names
//! from clients: no path separators, no `..`, looked up case-insensitively.
