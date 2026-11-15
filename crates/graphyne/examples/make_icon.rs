//! Regenerate the application icon: `cargo run -p graphyne --example make_icon`.
//!
//! Renders `assets/app-icon/graphyne.svg` (large sizes) and a bolder variant (small sizes)
//! into a multi-resolution `crates/graphyne/graphyne.ico` (PNG-compressed entries), which
//! `build.rs` embeds as icon resource 1 — the exe icon and, via gpui, the window icon.

use std::path::Path;

/// Hexagonal "graph" mark on a blue rounded square. `bold` thickens strokes / nodes so
/// the 16–32 px renderings stay legible.
fn svg(bold: bool) -> String {
    let (stroke, node, hub) = if bold { (22, 22, 24) } else { (14, 15, 18) };
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#4F8DE8"/>
      <stop offset="1" stop-color="#2447A8"/>
    </linearGradient>
  </defs>
  <rect x="8" y="8" width="240" height="240" rx="56" fill="url(#bg)"/>
  <g stroke="#FFFFFF" stroke-width="{stroke}" stroke-linejoin="round" stroke-linecap="round" fill="none">
    <path d="M128 52 L194 90 L194 166 L128 204 L62 166 L62 90 Z"/>
    <path d="M128 128 L128 204 M128 128 L62 90 M128 128 L194 90" stroke-opacity="0.6"/>
  </g>
  <g fill="#FFFFFF">
    <circle cx="128" cy="52" r="{node}"/>
    <circle cx="194" cy="90" r="{node}"/>
    <circle cx="194" cy="166" r="{node}"/>
    <circle cx="128" cy="204" r="{node}"/>
    <circle cx="62" cy="166" r="{node}"/>
    <circle cx="62" cy="90" r="{node}"/>
  </g>
  <circle cx="128" cy="128" r="{hub}" fill="#BFD6FF"/>
</svg>
"##
    )
}

fn render_png(svg: &str, size: u32) -> Vec<u8> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).expect("valid svg");
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).expect("pixmap");
    let scale = size as f32 / tree.size().width();
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.encode_png().expect("png")
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let regular = svg(false);
    let bold = svg(true);
    std::fs::write(root.join("assets/app-icon/graphyne.svg"), &regular).expect("write svg");

    let sizes = [16u32, 20, 24, 32, 40, 48, 64, 128, 256];
    let images: Vec<(u32, Vec<u8>)> = sizes.iter().map(|&s| (s, render_png(if s <= 32 { &bold } else { &regular }, s))).collect();
    std::fs::write(root.join("assets/app-icon/graphyne-256.png"), &images.last().unwrap().1).expect("write png");

    // ICONDIR + ICONDIRENTRY[] + PNG payloads.
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, png) in &images {
        let dim = if *size >= 256 { 0u8 } else { *size as u8 };
        ico.extend_from_slice(&[dim, dim, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes()); // planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bpp
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in &images {
        ico.extend_from_slice(png);
    }
    std::fs::write(root.join("crates/graphyne/graphyne.ico"), &ico).expect("write ico");
    println!("wrote graphyne.ico ({} sizes, {} bytes)", images.len(), ico.len());
}
