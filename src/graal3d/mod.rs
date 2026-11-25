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
//! - [`ground`] is the streamed terrain, water and ground cover
//!   (protocols 21 and 24); [`assets`] checks a world's files at start-up.
//! - [`scene`] reads `config/graal3dworld.txt`.
//! - [`service`] is the UDP task and its tie to the TCP logins.

pub mod assets;
pub mod bits;
pub mod conn;
pub mod events;
pub mod ground;
pub mod handshake;
pub mod huffman;
pub mod link;
pub mod objects;
pub mod scene;
pub mod service;
pub mod tge;

pub use service::{has_graal3d, Graal3d, TcpPlayer};

/// An image a 3D client asked for that the folders do not hold under that
/// name. The 5.x clients ask for Sky and terrain textures with no
/// extension and store what they get under the name they asked for; 3.153
/// asks for Sky textures as `.gif`, never `.png` (lab, 2026-11-25). So a
/// bare name, or a name with an image extension, also finds the same stem
/// with another image extension: `.png`, `.jpg`, `.jpeg`, `.gif`, `.bmp`,
/// `.tga`, `.dds`, in that order. The file keeps the name asked for.
pub fn extensionless_texture(
    files: &crate::files::FileStore,
    name: &str,
) -> Option<crate::files::Found> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    if base.is_empty() {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let stem = match base.rfind('.') {
        None => name,
        Some(_) => {
            let ext = assets::IMAGE_EXTENSIONS
                .iter()
                .find(|e| lower.ends_with(*e))?;
            &name[..name.len() - ext.len()]
        }
    };
    assets::IMAGE_EXTENSIONS
        .iter()
        .map(|ext| format!("{stem}{ext}"))
        .filter(|n| !n.eq_ignore_ascii_case(name))
        .find_map(|n| files.find(&n))
        .map(|mut f| {
            f.name = name.to_string();
            f
        })
}
