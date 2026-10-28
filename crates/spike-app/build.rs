//! Build script for the Spike app.
//!
//! The Slint UI is only compiled when the `gui` feature is enabled. Cargo
//! exposes enabled features to build scripts as `CARGO_FEATURE_<NAME>`
//! environment variables, which is the reliable way to branch here (build
//! scripts do not see `cfg!(feature = ...)` at compile time).

fn main() {
    println!("cargo:rerun-if-changed=ui/main.slint");

    if std::env::var_os("CARGO_FEATURE_GUI").is_some() {
        slint_build::compile("ui/main.slint").expect("failed to compile Slint UI");
    }
}
