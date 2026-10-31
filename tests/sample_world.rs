//! The sample world in `world/` loads with no warnings and the defaults.

#[test]
fn sample_world_loads_clean() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("world");
    let (server, warnings) = seihai_gserver::load_world(&dir).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let d = seihai_gserver::config::Config::defaults(&dir);
    assert_eq!(server.cfg.port, d.port);
    assert!(server.cfg.bind.is_loopback());
    assert!(!server.cfg.create_accounts);
    assert_eq!(server.cfg.board_tile, d.board_tile);
    assert_eq!(
        server.allow.len(),
        seihai_gserver::allow::AllowList::all_known().len()
    );
}
