//! Port of packages/durable/src/tools/image.ts

const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

pub fn detect_supported_image_mime_type(buffer: &[u8]) -> Option<&'static str> {
    if buffer.starts_with(&[0xff, 0xd8, 0xff]) {
        return if buffer.get(3).copied() == Some(0xf7) {
            None
        } else {
            Some("image/jpeg")
        };
    }
    if buffer.starts_with(&PNG_SIGNATURE) {
        return if is_png(buffer) && !is_animated_png(buffer) {
            Some("image/png")
        } else {
            None
        };
    }
    if buffer.starts_with(b"GIF87a") || buffer.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if buffer.len() >= 12 && &buffer[0..4] == b"RIFF" && &buffer[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if buffer.starts_with(b"BM") && is_bmp(buffer) {
        return Some("image/bmp");
    }
    None
}

fn is_png(buffer: &[u8]) -> bool {
    buffer.len() >= 16 && read_u32_be(buffer, PNG_SIGNATURE.len()) == 13 && starts_with_ascii(buffer, 12, b"IHDR")
}

fn is_animated_png(buffer: &[u8]) -> bool {
    let mut offset = PNG_SIGNATURE.len();
    while offset + 8 <= buffer.len() {
        let chunk_length = read_u32_be(buffer, offset) as usize;
        let chunk_type_offset = offset + 4;
        if starts_with_ascii(buffer, chunk_type_offset, b"acTL") {
            return true;
        }
        if starts_with_ascii(buffer, chunk_type_offset, b"IDAT") {
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
    if u64::from(pixel_data_offset) < 14 + u64::from(dib_header_size) {
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
    u16::from(buffer[offset]) + (u16::from(buffer[offset + 1]) << 8)
}

fn read_u32_be(buffer: &[u8], offset: usize) -> u32 {
    (u32::from(buffer[offset]) << 24)
        + (u32::from(buffer[offset + 1]) << 16)
        + (u32::from(buffer[offset + 2]) << 8)
        + u32::from(buffer[offset + 3])
}

fn read_u32_le(buffer: &[u8], offset: usize) -> u32 {
    u32::from(buffer[offset])
        + (u32::from(buffer[offset + 1]) << 8)
        + (u32::from(buffer[offset + 2]) << 16)
        + (u32::from(buffer[offset + 3]) << 24)
}

fn starts_with_ascii(buffer: &[u8], offset: usize, bytes: &[u8]) -> bool {
    buffer
        .get(offset..offset + bytes.len())
        .is_some_and(|slice| slice == bytes)
}
