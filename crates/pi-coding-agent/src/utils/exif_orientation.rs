//! Port of packages/coding-agent/src/utils/exif-orientation.ts

use crate::utils::photon::{PhotonBackend, PhotonImage, fliph, flipv};

fn read_orientation_from_tiff(bytes: &[u8], tiff_start: usize) -> u16 {
    if tiff_start + 8 > bytes.len() {
        return 1;
    }

    let byte_order = (u16::from(bytes[tiff_start]) << 8) | u16::from(bytes[tiff_start + 1]);
    let le = byte_order == 0x4949;

    let read16 = |pos: usize| -> u16 {
        if pos + 2 > bytes.len() {
            return 0;
        }
        if le {
            u16::from_le_bytes([bytes[pos], bytes[pos + 1]])
        } else {
            u16::from_be_bytes([bytes[pos], bytes[pos + 1]])
        }
    };

    let read32 = |pos: usize| -> u32 {
        if pos + 4 > bytes.len() {
            return 0;
        }
        if le {
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
        } else {
            u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
        }
    };

    let ifd_offset = read32(tiff_start + 4) as usize;
    let Some(ifd_start) = tiff_start.checked_add(ifd_offset) else {
        return 1;
    };
    if ifd_start + 2 > bytes.len() {
        return 1;
    }

    let entry_count = usize::from(read16(ifd_start));
    for i in 0..entry_count {
        let entry_pos = ifd_start + 2 + i * 12;
        if entry_pos + 12 > bytes.len() {
            return 1;
        }
        if read16(entry_pos) == 0x0112 {
            let value = read16(entry_pos + 8);
            return if (1..=8).contains(&value) { value } else { 1 };
        }
    }

    1
}

fn has_exif_header(bytes: &[u8], offset: usize) -> bool {
    bytes.get(offset..offset + 6) == Some(&[0x45, 0x78, 0x69, 0x66, 0x00, 0x00])
}

fn find_jpeg_tiff_offset(bytes: &[u8]) -> Option<usize> {
    let mut offset = 2usize;
    while offset < bytes.len().saturating_sub(1) {
        if bytes[offset] != 0xff {
            return None;
        }
        let marker = bytes[offset + 1];
        if marker == 0xff {
            offset += 1;
            continue;
        }

        if marker == 0xe1 {
            if offset + 4 >= bytes.len() {
                return None;
            }
            let segment_start = offset + 4;
            if segment_start + 6 > bytes.len() {
                return None;
            }
            if has_exif_header(bytes, segment_start) {
                return Some(segment_start + 6);
            }
        }

        if offset + 4 > bytes.len() {
            return None;
        }
        let length = ((usize::from(bytes[offset + 2])) << 8) | usize::from(bytes[offset + 3]);
        offset = offset.checked_add(2 + length)?;
    }
    None
}

fn find_webp_tiff_offset(bytes: &[u8]) -> Option<usize> {
    let mut offset = 12usize;
    while offset + 8 <= bytes.len() {
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_size = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        let data_start = offset + 8;

        if chunk_id == b"EXIF" {
            if data_start.checked_add(chunk_size)? > bytes.len() {
                return None;
            }
            let tiff_start = if chunk_size >= 6 && has_exif_header(bytes, data_start) {
                data_start + 6
            } else {
                data_start
            };
            return Some(tiff_start);
        }

        offset = data_start.checked_add(chunk_size)?.checked_add(chunk_size % 2)?;
    }
    None
}

fn get_exif_orientation(bytes: &[u8]) -> u16 {
    let tiff_offset = if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xd8 {
        find_jpeg_tiff_offset(bytes)
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        find_webp_tiff_offset(bytes)
    } else {
        None
    };

    match tiff_offset {
        Some(off) => read_orientation_from_tiff(bytes, off),
        None => 1,
    }
}

fn rotate90<F>(image: &PhotonImage, dst_index: F) -> Option<PhotonImage>
where
    F: Fn(u32, u32, u32, u32) -> usize,
{
    let w = image.get_width();
    let h = image.get_height();
    let src = image.get_raw_pixels();
    let mut dst = vec![0u8; src.len()];

    for y in 0..h {
        for x in 0..w {
            let src_idx = ((y * w + x) * 4) as usize;
            let dst_idx = dst_index(x, y, w, h) * 4;
            if dst_idx + 4 <= dst.len() && src_idx + 4 <= src.len() {
                dst[dst_idx..dst_idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
            }
        }
    }

    PhotonImage::new(dst, h, w).ok()
}

pub fn apply_exif_orientation(_photon: &PhotonBackend, mut image: PhotonImage, original_bytes: &[u8]) -> PhotonImage {
    let orientation = get_exif_orientation(original_bytes);
    if orientation == 1 {
        return image;
    }

    match orientation {
        2 => {
            fliph(&mut image);
            image
        }
        3 => {
            fliph(&mut image);
            flipv(&mut image);
            image
        }
        4 => {
            flipv(&mut image);
            image
        }
        5 => {
            if let Some(mut rotated) = rotate90(&image, |x, y, _w, h| (x * h + (h - 1 - y)) as usize) {
                fliph(&mut rotated);
                rotated
            } else {
                image
            }
        }
        6 => rotate90(&image, |x, y, _w, h| (x * h + (h - 1 - y)) as usize).unwrap_or(image),
        7 => {
            if let Some(mut rotated) = rotate90(&image, |x, y, w, h| ((w - 1 - x) * h + y) as usize) {
                fliph(&mut rotated);
                rotated
            } else {
                image
            }
        }
        8 => rotate90(&image, |x, y, w, h| ((w - 1 - x) * h + y) as usize).unwrap_or(image),
        _ => image,
    }
}
