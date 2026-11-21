use super::fixture::Fixture;
use crate::wl_proxy::interfaces;

/// The supervisor's proxy has to decode every protocol a client can bind.
#[test]
fn every_global_is_known_to_the_proxy() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();
    f.roundtrip(id);
    let globals = &f.client(id).state.globals;
    assert!(!globals.is_empty());
    let missing: Vec<_> = globals
        .iter()
        .filter(|g| interfaces::by_name(&g.interface).is_none())
        .map(|g| g.interface.clone())
        .collect();
    assert!(missing.is_empty(), "the proxy can't decode {missing:?}");
}
