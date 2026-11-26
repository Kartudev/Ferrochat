//! Embeds the app icon (`packaging/ferrochat.rc`) into `ferrochat.exe` when
//! building for Windows; nothing to do elsewhere.

fn main() {
    println!("cargo:rerun-if-changed=packaging/ferrochat.rc");
    println!("cargo:rerun-if-changed=packaging/ferrochat.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if let Err(e) = embed_resource::compile_for(
        "packaging/ferrochat.rc",
        ["ferrochat"],
        embed_resource::NONE,
    )
    .manifest_optional()
    {
        panic!("could not embed the Windows icon: {e}");
    }
}
