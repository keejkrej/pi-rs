//! Port of packages/coding-agent/src/utils/mime.ts

use std::io::Read;

use pi_js::Result;

const IMAGE_TYPE_SNIFF_BYTES: usize = 4100;
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

pub fn detect_supported_image_mime_type(buffer: &[u8]) -> Option<&'static str> {
    if starts_with(buffer, &[0xff, 0xd8, 0xff]) {
        return if buffer.get(3).copied() == Some(0xf7) {
            None
        } else {
            Some("image/jpeg")
        };
    }
    if starts_with(buffer, &PNG_SIGNATURE) {
        return if is_png(buffer) && !is_animated_png(buffer) {
            Some("image/png")
        } else {
            None
        };
    }
    if starts_with_ascii(buffer, 0, "GIF87a") || starts_with_ascii(buffer, 0, "GIF89a") {
        return Some("image/gif");
    }
    if starts_with_ascii(buffer, 0, "RIFF") && starts_with_ascii(buffer, 8, "WEBP") {
        return Some("image/webp");
    }
    if starts_with_ascii(buffer, 0, "BM") && is_bmp(buffer) {
        return Some("image/bmp");
    }
    None
}

pub async fn detect_supported_image_mime_type_from_file(file_path: &str) -> Result<Option<&'static str>> {
    let path = file_path.to_owned();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&path).map_err(|err| pi_js::Error::from_io(err, "open", Some(&path), None))?;
        let mut reader = std::io::BufReader::new(file);
        let mut buffer = [0u8; IMAGE_TYPE_SNIFF_BYTES];
        let mut total_read = 0;
        while total_read < IMAGE_TYPE_SNIFF_BYTES {
            let n = reader
                .read(&mut buffer[total_read..])
                .map_err(|err| pi_js::Error::from_io(err, "read", Some(&path), None))?;
            if n == 0 {
                break;
            }
            total_read += n;
        }
        Ok(detect_supported_image_mime_type(&buffer[..total_read]))
    })
    .await
    .map_err(|err| pi_js::Error::msg(err.to_string()))?
}

fn is_png(buffer: &[u8]) -> bool {
    buffer.len() >= 16 && read_u32_be(buffer, PNG_SIGNATURE.len()) == 13 && starts_with_ascii(buffer, 12, "IHDR")
}

fn is_animated_png(buffer: &[u8]) -> bool {
    let mut offset = PNG_SIGNATURE.len();
    while offset + 8 <= buffer.len() {
        let chunk_length = read_u32_be(buffer, offset) as usize;
        let chunk_type_offset = offset + 4;
        if starts_with_ascii(buffer, chunk_type_offset, "acTL") {
            return true;
        }
        if starts_with_ascii(buffer, chunk_type_offset, "IDAT") {
            return false;
        }

        let Some(next_offset) = offset
            .checked_add(8)
            .and_then(|v| v.checked_add(chunk_length))
            .and_then(|v| v.checked_add(4))
        else {
            return false;
        };
        if next_offset <= offset || next_offset > buffer.len() {
            return false;
        }
        offset = next_offset;
    }
    false
}

fn is_bmp(buffer: &[u8]) -> bool {
    if buffer.len() < 26 {
        return false;
    }

    let declared_file_size = read_u32_le(buffer, 2);
    let pixel_data_offset = read_u32_le(buffer, 10);
    let dib_header_size = read_u32_le(buffer, 14);
    if declared_file_size != 0 && declared_file_size < 26 {
        return false;
    }
    if pixel_data_offset < 14 + dib_header_size {
        return false;
    }
    if declared_file_size != 0 && pixel_data_offset >= declared_file_size {
        return false;
    }

    let (color_planes, bits_per_pixel) = if dib_header_size == 12 {
        (read_u16_le(buffer, 22), read_u16_le(buffer, 24))
    } else if (40..=124).contains(&dib_header_size) {
        if buffer.len() < 30 {
            return false;
        }
        (read_u16_le(buffer, 26), read_u16_le(buffer, 28))
    } else {
        return false;
    };

    color_planes == 1 && matches!(bits_per_pixel, 1 | 4 | 8 | 16 | 24 | 32)
}

fn read_u16_le(buffer: &[u8], offset: usize) -> u16 {
    let b0 = u16::from(buffer.get(offset).copied().unwrap_or(0));
    let b1 = u16::from(buffer.get(offset + 1).copied().unwrap_or(0));
    b0 | (b1 << 8)
}

fn read_u32_be(buffer: &[u8], offset: usize) -> u32 {
    let b0 = u32::from(buffer.get(offset).copied().unwrap_or(0));
    let b1 = u32::from(buffer.get(offset + 1).copied().unwrap_or(0));
    let b2 = u32::from(buffer.get(offset + 2).copied().unwrap_or(0));
    let b3 = u32::from(buffer.get(offset + 3).copied().unwrap_or(0));
    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
}

fn read_u32_le(buffer: &[u8], offset: usize) -> u32 {
    let b0 = u32::from(buffer.get(offset).copied().unwrap_or(0));
    let b1 = u32::from(buffer.get(offset + 1).copied().unwrap_or(0));
    let b2 = u32::from(buffer.get(offset + 2).copied().unwrap_or(0));
    let b3 = u32::from(buffer.get(offset + 3).copied().unwrap_or(0));
    b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
}

fn starts_with(buffer: &[u8], bytes: &[u8]) -> bool {
    buffer.len() >= bytes.len() && &buffer[..bytes.len()] == bytes
}

fn starts_with_ascii(buffer: &[u8], offset: usize, text: &str) -> bool {
    let target = text.as_bytes();
    buffer.len() >= offset.saturating_add(target.len()) && &buffer[offset..offset + target.len()] == target
}
