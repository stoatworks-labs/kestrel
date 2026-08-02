//! Compiles the macOS App Nap opt-out. Nothing to do on other platforms.

fn main() {
    println!("cargo::rerun-if-changed=native/activity.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    cc::Build::new()
        .file("native/activity.m")
        // No ARC: the activity token is retained once and deliberately never
        // released, which is clearer written out than fought with.
        .flag("-fno-objc-arc")
        .compile("kestrel_activity");
    println!("cargo::rustc-link-lib=framework=Foundation");
}
