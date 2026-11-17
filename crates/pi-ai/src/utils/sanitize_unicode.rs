//! Port of packages/ai/src/utils/sanitize-unicode.ts

/// Removes unpaired Unicode surrogate characters from text.
///
/// PORT: Valid Rust `&str` is UTF-8 and cannot represent unpaired UTF-16 surrogates, so
/// [`sanitize_surrogates`] is a zero-loss copy while [`sanitize_surrogates_utf16`] strips
/// unpaired surrogates from raw UTF-16 code units before decoding.
pub fn sanitize_surrogates(text: &str) -> String {
    text.to_string()
}

/// Strip unpaired UTF-16 surrogate code units from a slice and return valid UTF-8 text.
pub fn sanitize_surrogates_utf16(units: &[u16]) -> String {
    let mut filtered = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        if (0xD800..=0xDBFF).contains(&u) {
            if let Some(&next) = units.get(i + 1) {
                if (0xDC00..=0xDFFF).contains(&next) {
                    filtered.push(u);
                    filtered.push(next);
                    i += 2;
                    continue;
                }
            }
            i += 1;
            continue;
        }
        if (0xDC00..=0xDFFF).contains(&u) {
            i += 1;
            continue;
        }
        filtered.push(u);
        i += 1;
    }
    String::from_utf16(&filtered).unwrap_or_default()
}
