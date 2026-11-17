//! Port of packages/coding-agent/src/utils/zip.ts

use std::io::Write;

use chrono::{Datelike, Timelike};
use flate2::Compression;
use flate2::write::DeflateEncoder;
use pi_js::Result;

#[derive(Clone, Debug)]
pub struct ZipEntry {
    pub name: String,
    pub data: Vec<u8>,
}

impl ZipEntry {
    pub fn from_str(name: impl Into<String>, data: &str) -> Self {
        Self {
            name: name.into(),
            data: data.as_bytes().to_vec(),
        }
    }

    pub fn from_bytes(name: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            data,
        }
    }
}

fn dos_date_time(ms: i64) -> (u16, u16) {
    let dt = chrono::DateTime::from_timestamp_millis(ms)
        .map(|utc| utc.with_timezone(&chrono::Local))
        .unwrap_or_else(chrono::Local::now);
    let time = ((dt.hour() as u16) << 11) | ((dt.minute() as u16) << 5) | ((dt.second() as u16) >> 1);
    let year = (dt.year().max(1980) - 1980) as u16;
    let day = (year << 9) | ((dt.month() as u16) << 5) | (dt.day() as u16);
    (time, day)
}

/// Create the small, classic ZIP archives used by bug reports.
pub fn create_zip_archive(entries: &[ZipEntry]) -> Result<Vec<u8>> {
    let mut files: Vec<u8> = Vec::new();
    let mut directory: Vec<u8> = Vec::new();
    let (time, day) = dos_date_time(pi_js::time::now_ms());
    let mut offset: u32 = 0;

    for entry in entries {
        let name = entry.name.as_bytes();
        let data = &entry.data;

        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(data)
            .map_err(|err| pi_js::Error::msg(format!("ZIP deflate failed: {err}")))?;
        let compressed = encoder
            .finish()
            .map_err(|err| pi_js::Error::msg(format!("ZIP deflate finish failed: {err}")))?;

        let mut hasher = crc32fast::Hasher::new();
        hasher.update(data);
        let checksum = hasher.finalize();

        let name_len = u16::try_from(name.len())
            .map_err(|_| pi_js::Error::msg(format!("ZIP entry name too long: {}", entry.name)))?;
        let comp_len =
            u32::try_from(compressed.len()).map_err(|_| pi_js::Error::msg("ZIP compressed entry exceeds 4 GiB"))?;
        let data_len =
            u32::try_from(data.len()).map_err(|_| pi_js::Error::msg("ZIP uncompressed entry exceeds 4 GiB"))?;

        let mut local = [0u8; 30];
        local[0..4].copy_from_slice(&0x0403_4b50u32.to_le_bytes());
        local[4..6].copy_from_slice(&20u16.to_le_bytes());
        local[6..8].copy_from_slice(&0x0800u16.to_le_bytes());
        local[8..10].copy_from_slice(&8u16.to_le_bytes());
        local[10..12].copy_from_slice(&time.to_le_bytes());
        local[12..14].copy_from_slice(&day.to_le_bytes());
        local[14..18].copy_from_slice(&checksum.to_le_bytes());
        local[18..22].copy_from_slice(&comp_len.to_le_bytes());
        local[22..26].copy_from_slice(&data_len.to_le_bytes());
        local[26..28].copy_from_slice(&name_len.to_le_bytes());

        let mut central = [0u8; 46];
        central[0..4].copy_from_slice(&0x0201_4b50u32.to_le_bytes());
        central[4..6].copy_from_slice(&20u16.to_le_bytes());
        central[6..8].copy_from_slice(&20u16.to_le_bytes());
        central[8..10].copy_from_slice(&0x0800u16.to_le_bytes());
        central[10..12].copy_from_slice(&8u16.to_le_bytes());
        central[12..14].copy_from_slice(&time.to_le_bytes());
        central[14..16].copy_from_slice(&day.to_le_bytes());
        central[16..20].copy_from_slice(&checksum.to_le_bytes());
        central[20..24].copy_from_slice(&comp_len.to_le_bytes());
        central[24..28].copy_from_slice(&data_len.to_le_bytes());
        central[28..30].copy_from_slice(&name_len.to_le_bytes());
        central[42..46].copy_from_slice(&offset.to_le_bytes());

        files.extend_from_slice(&local);
        files.extend_from_slice(name);
        files.extend_from_slice(&compressed);

        directory.extend_from_slice(&central);
        directory.extend_from_slice(name);

        offset = offset
            .checked_add(30 + u32::from(name_len) + comp_len)
            .ok_or_else(|| pi_js::Error::msg("ZIP archive exceeds 4 GiB"))?;
    }

    let entry_count = u16::try_from(entries.len()).map_err(|_| pi_js::Error::msg("Too many ZIP entries"))?;
    let cd_len =
        u32::try_from(directory.len()).map_err(|_| pi_js::Error::msg("ZIP central directory exceeds 4 GiB"))?;

    let mut end = [0u8; 22];
    end[0..4].copy_from_slice(&0x0605_4b50u32.to_le_bytes());
    end[8..10].copy_from_slice(&entry_count.to_le_bytes());
    end[10..12].copy_from_slice(&entry_count.to_le_bytes());
    end[12..16].copy_from_slice(&cd_len.to_le_bytes());
    end[16..20].copy_from_slice(&offset.to_le_bytes());

    let mut out = Vec::with_capacity(files.len() + directory.len() + end.len());
    out.extend_from_slice(&files);
    out.extend_from_slice(&directory);
    out.extend_from_slice(&end);
    Ok(out)
}

pub async fn write_zip_archive(file_path: &str, entries: &[ZipEntry]) -> Result<()> {
    let bytes = create_zip_archive(entries)?;
    pi_js::fs::promises::write_file(file_path, bytes, None).await
}
