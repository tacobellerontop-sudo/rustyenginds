//! Embeds a Windows application manifest.
//!
//! Besides per-monitor DPI awareness and modern common controls, the
//! manifest declares Windows 8+ compatibility. Without that declaration
//! Windows ignores WS_EX_LAYERED on child windows, and on the Windows 11
//! 24H2 "raised desktop" only layered children of Progman are drawn, so the
//! wallpaper would stay invisible.

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest::embed_manifest(embed_manifest::new_manifest("RustyWallpaper"))
            .expect("unable to embed the Windows manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
