//! seihai-gserver: A new Graal game server written in Rust on Seihai's core, for classic clients (1.33 to 2.31 first) and Seihai.
//!
//! Scaffold only. The plan is in `docs/PLAN.md`; the rules are in `AGENTS.md`.

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("seihai-gserver {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    eprintln!(
        "seihai-gserver {}: not implemented yet, see docs/PLAN.md",
        env!("CARGO_PKG_VERSION")
    );
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    #[test]
    fn links_seihai_core() {
        // Proves the shared crate builds on every CI target.
        let _ = std::any::type_name::<seihai_core::protocol::ClientVersion>();
    }
}
