//! Compile-time UI embedding check.
//!
//! When `apps/desktop/dist/index.html` exists at compile time, `cfg(has_ui_dist)`
//! is set and the server embeds the prebuilt SPA (see `web_ui.rs`). Without it,
//! `cargo build` and `cargo test` stay independent of Node and the frontend
//! build; the server then serves `ANY_CONVERTER_WEB_DIR` or answers 503.

fn main() {
    // `dist` itself tracks content changes; `apps/desktop` always exists
    // and covers the first build (cargo does not re-run the script when a
    // missing `rerun-if-changed` path appears); `index.html` covers
    // in-place rewrites.
    println!("cargo:rerun-if-changed=../../apps/desktop/dist");
    println!("cargo:rerun-if-changed=../../apps/desktop/dist/index.html");
    println!("cargo:rerun-if-changed=../../apps/desktop");
    println!("cargo:rustc-check-cfg=cfg(has_ui_dist)");
    if std::path::Path::new("../../apps/desktop/dist/index.html").is_file() {
        println!("cargo:rustc-cfg=has_ui_dist");
    }
}
