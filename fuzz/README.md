# traceless-fuzz

Per-handler cargo-fuzz targets. Every target feeds raw bytes to one
handler's `read_metadata` and `clean_metadata`, asserting only that
neither call panics. Any crash reproduces the offending input under
`fuzz/artifacts/<target>/`.

## Running

Requires a nightly toolchain plus `cargo-fuzz`:

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
```

From the repository root:

```bash
cargo +nightly fuzz run handler_jpeg
cargo +nightly fuzz run handler_pdf
cargo +nightly fuzz run handler_zip
# ...
```

Available targets: `handler_jpeg`, `handler_png`, `handler_webp`,
`handler_gif`, `handler_pdf`, `handler_html`, `handler_svg`,
`handler_css`, `handler_torrent`, `handler_zip`, `handler_tar`,
`handler_targz`.

## Why this crate is outside the workspace

`libfuzzer-sys` uses linkage tricks that the stable compiler refuses,
so the workspace `[workspace] exclude = ["fuzz"]` keeps the stable
CI gate (`cargo check --workspace`, `cargo clippy --workspace
--all-targets`) from ever touching this directory. Run the targets
with the explicit `+nightly` channel.

## Adding a new target

1. Add a `fuzz_targets/handler_<name>.rs` binary that calls
   `traceless_fuzz::fuzz_handler("<mime>", "<ext>", data)`.
2. Add a matching `[[bin]]` entry to `Cargo.toml`.
3. Run once locally with `cargo +nightly fuzz run handler_<name>
   -- -runs=1000000` to smoke-test for obvious crashes before
   merging.
