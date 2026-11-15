//! Embed the Windows icon + version resources (graphyne.rc) into graphyne.exe.

fn main() {
    println!("cargo:rerun-if-changed=graphyne.rc");
    println!("cargo:rerun-if-changed=graphyne.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // `cargo run --example make_icon` creates it; don't block that first build.
    if !std::path::Path::new("graphyne.ico").exists() {
        println!("cargo:warning=graphyne.ico missing: run `cargo run -p graphyne --example make_icon`");
        return;
    }
    let num = |key: &str| std::env::var(key).unwrap_or_else(|_| "0".into());
    let macros = [
        format!("VER_MAJOR={}", num("CARGO_PKG_VERSION_MAJOR")),
        format!("VER_MINOR={}", num("CARGO_PKG_VERSION_MINOR")),
        format!("VER_PATCH={}", num("CARGO_PKG_VERSION_PATCH")),
        format!("VER_STRING=\"{}\"", std::env::var("CARGO_PKG_VERSION").unwrap_or_default()),
    ];
    embed_resource::compile("graphyne.rc", &macros).manifest_optional().expect("compiling graphyne.rc");
}
