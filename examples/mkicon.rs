//! Regenerate the icon files in `assets/` from `src/icon.rs`:
//!
//!     cargo run --example mkicon
//!
//! Writes icon.png (512px, for Linux/README), icon.ico (Windows, embedded in edit.exe)
//! and icon.icns (macOS app bundle). ICO and ICNS both store PNG-compressed images.

#[path = "../src/icon.rs"]
mod icon;

use std::path::Path;

fn png(size: u32) -> Vec<u8> {
    let mut out = vec![];
    {
        let mut enc = png::Encoder::new(&mut out, size, size);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Best);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&icon::rgba(size)).unwrap();
    }
    out
}

fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = sizes.iter().map(|&s| (s, png(s))).collect();
    let mut out = vec![];
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (s, data) in &images {
        let dim = if *s >= 256 { 0 } else { *s as u8 }; // 0 means 256
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in images {
        out.extend_from_slice(&data);
    }
    out
}

fn icns() -> Vec<u8> {
    let entries: &[(&[u8; 4], u32)] = &[
        (b"icp4", 16),
        (b"icp5", 32),
        (b"icp6", 64),
        (b"ic07", 128),
        (b"ic08", 256),
        (b"ic09", 512),
        (b"ic10", 1024),
    ];
    let mut body = vec![];
    for (kind, size) in entries {
        let data = png(*size);
        body.extend_from_slice(*kind);
        body.extend_from_slice(&(8 + data.len() as u32).to_be_bytes());
        body.extend_from_slice(&data);
    }
    let mut out = b"icns".to_vec();
    out.extend_from_slice(&(8 + body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("icon.png"), png(512)).unwrap();
    std::fs::write(dir.join("icon.ico"), ico(&[16, 20, 24, 32, 40, 48, 64, 128, 256])).unwrap();
    std::fs::write(dir.join("icon.icns"), icns()).unwrap();
    println!("wrote icon.png, icon.ico, icon.icns to {}", dir.display());
}
