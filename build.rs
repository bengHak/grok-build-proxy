fn main() {
    println!("cargo:rerun-if-env-changed=GROK_BUILD_PROXY_VERSION");
    let version = std::env::var("GROK_BUILD_PROXY_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").expect("Cargo package version"));
    println!("cargo:rustc-env=GROK_BUILD_PROXY_BUILD_VERSION={version}");
}
