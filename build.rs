// Embed the app icon (assets/icon.ico, made by `cargo run --example mkicon`) in edit.exe,
// so Explorer, the taskbar and shortcuts show it.
fn main() {
    println!("cargo:rerun-if-changed=assets/edit.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/edit.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
