# Vendored crates

- `smithay/`, `smithay-drm-extras/`: [Smithay](https://github.com/Smithay/smithay) at
  `ff5fa7df392cecfba049ffed55cdaa4e98a8e7ef` (MIT, `LICENSE.txt`). Only the library sources are
  kept (no anvil, examples, benches); the manifests lost their `[workspace]`, examples, benches
  and dev-dependencies. arbor's changes are marked `// arbor:` in the sources and listed here.

  To update: copy `src/` (and `smithay-drm-extras/src/`) from a newer Smithay over these,
  re-apply the changes below, and check `cargo build` and `cargo test`.

Changes:
