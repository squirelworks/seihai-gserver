//! The 3D mode: the Torque UDP server the Graal 3.153 to 5.x clients open
//! on the listed port + 1 when the server's name starts with `3 `.
//!
//! Off unless `graal3d = true` in `config/server.txt`. The guide is
//! `docs/GRAAL3D.md`; the wire facts come from Graal_analysis
//! `torque/udp-session.md` and `torque/protocol19-server.md`.
//!
//! - [`bits`], [`huffman`], [`link`] and [`events`] are the Torque wire
//!   layer. They are the same code as the Seihai client's (Ato's
//!   `torque::net`, same licence), carried here until both move into
//!   `seihai-core`.
//! - [`handshake`] is the out-of-band handshake, server side, and the
//!   profile of each protocol (19, 21, 24).
//! - [`objects`] are the GameObject-era ghosts (GameObject, Sun, Sky,
//!   Camera); [`tge`] is the protocol 19 layer (datablocks, the stock
//!   Player and Camera, moves).
//! - [`conn`] is one link: numbering, events, ghosting, mission start.
//! - [`scene`] reads `config/graal3dworld.txt`.
//! - [`service`] is the UDP task and its tie to the TCP logins.

pub mod bits;
pub mod conn;
pub mod events;
pub mod handshake;
pub mod huffman;
pub mod link;
pub mod objects;
pub mod scene;
pub mod service;
pub mod tge;

pub use service::{has_graal3d, Graal3d, TcpPlayer};

/// A file a 3D client asked for with no extension: the 5.x client asks
/// for a Sky's textures that way and stores what it gets under the name
/// it asked for. The first image with that name and a known extension.
pub fn extensionless_texture(
    files: &crate::files::FileStore,
    name: &str,
) -> Option<crate::files::Found> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    if base.is_empty() || base.contains('.') {
        return None;
    }
    [".png", ".jpg", ".jpeg", ".bmp", ".gif", ".dds"]
        .iter()
        .find_map(|ext| files.find(&format!("{name}{ext}")))
        .map(|mut f| {
            f.name = name.to_string();
            f
        })
}
