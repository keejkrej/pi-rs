//! Port of packages/coding-agent/test/clipboard-image-bmp-conversion.test.ts

use std::fs;
use std::os::unix::fs::PermissionsExt;

use indexmap::IndexMap;
use pi_coding_agent::utils::clipboard_image::{ClipboardImage, ReadClipboardImageOptions, read_clipboard_image};
use pi_js::env::testing::{EnvGuard, PlatformGuard};
use pi_tui::native_platform::testing::override_get_image;
use serial_test::serial;
use tempfile::TempDir;

fn create_tiny_bmp_1x1_red_24bpp() -> Vec<u8> {
    // Minimal 1x1 24bpp BMP (BGR + row padding to 4 bytes)
    // File size = 14 (BMP header) + 40 (DIB header) + 4 (pixel row) = 58
    let mut buf = vec![0u8; 58];

    // BITMAPFILEHEADER
    buf[0] = b'B';
    buf[1] = b'M';
    let buf_len = buf.len() as u32;
    buf[2..6].copy_from_slice(&buf_len.to_le_bytes());
    buf[6..8].copy_from_slice(&0u16.to_le_bytes());
    buf[8..10].copy_from_slice(&0u16.to_le_bytes());
    buf[10..14].copy_from_slice(&54u32.to_le_bytes());

    // BITMAPINFOHEADER
    buf[14..18].copy_from_slice(&40u32.to_le_bytes());
    buf[18..22].copy_from_slice(&1i32.to_le_bytes());
    buf[22..26].copy_from_slice(&1i32.to_le_bytes());
    buf[26..28].copy_from_slice(&1u16.to_le_bytes());
    buf[28..30].copy_from_slice(&24u16.to_le_bytes());
    buf[30..34].copy_from_slice(&0u32.to_le_bytes());
    buf[34..38].copy_from_slice(&4u32.to_le_bytes());
    buf[38..42].copy_from_slice(&0i32.to_le_bytes());
    buf[42..46].copy_from_slice(&0i32.to_le_bytes());
    buf[46..50].copy_from_slice(&0u32.to_le_bytes());
    buf[50..54].copy_from_slice(&0u32.to_le_bytes());

    // Pixel data (B, G, R) + 1 byte padding
    buf[54] = 0x00;
    buf[55] = 0x00;
    buf[56] = 0xff;
    buf[57] = 0x00;
    buf
}

fn write_exec(dir: &std::path::Path, name: &str, script: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write shim script");
    let mut perms = fs::metadata(&path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).expect("chmod");
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn linux_converts_command_bmp_to_png() {
    let bmp = create_tiny_bmp_1x1_red_24bpp();
    let dir = TempDir::new().expect("tempdir");
    let bmp_path = dir.path().join("sample.bmp");
    fs::write(&bmp_path, &bmp).expect("write sample bmp");

    write_exec(
        dir.path(),
        "wl-paste",
        &format!(
            "case \"$*\" in\n  *--list-types*) printf 'image/bmp\\n' ;;\n  *image/bmp*) cat '{}' ;;\n  *) exit 1 ;;\nesac",
            bmp_path.display()
        ),
    );

    let orig_path = pi_js::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{orig_path}", dir.path().display());
    let _path_guard = EnvGuard::set("PATH", Some(&new_path));

    let mut env = IndexMap::new();
    env.insert("WAYLAND_DISPLAY".to_owned(), "wayland-0".to_owned());

    let image = read_clipboard_image(Some(ReadClipboardImageOptions {
        env: Some(env),
        platform: Some("linux".to_owned()),
    }))
    .await;

    let ClipboardImage { bytes, mime_type } = image.expect("expected converted PNG image");
    assert_eq!(mime_type, "image/png");
    assert_eq!(&bytes[..4], &[0x89, 0x50, 0x4e, 0x47]);
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn win32_converts_native_bmp_to_png() {
    let bmp = create_tiny_bmp_1x1_red_24bpp();
    let _plat = PlatformGuard::set("win32");
    let _img = override_get_image(move |_| Ok(Some(Some(bmp.clone()))));

    let image = read_clipboard_image(Some(ReadClipboardImageOptions {
        env: Some(IndexMap::new()),
        platform: Some("win32".to_owned()),
    }))
    .await;

    let ClipboardImage { bytes, mime_type } = image.expect("expected converted PNG image");
    assert_eq!(mime_type, "image/png");
    assert_eq!(&bytes[..4], &[0x89, 0x50, 0x4e, 0x47]);
}
