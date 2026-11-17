//! Port of packages/coding-agent/src/utils/html.ts

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedHtmlEntity {
    pub text: String,
    pub length: usize,
}

fn decode_code_point(code_point: i64) -> Option<String> {
    if !(0..=0x10_FFFF).contains(&code_point) {
        return None;
    }
    let cp = u32::try_from(code_point).ok()?;
    char::from_u32(cp).map(|c| c.to_string())
}

pub fn decode_html_entity(entity: &str) -> Option<String> {
    match entity {
        "amp" => return Some("&".to_owned()),
        "lt" => return Some("<".to_owned()),
        "gt" => return Some(">".to_owned()),
        "quot" => return Some("\"".to_owned()),
        "apos" => return Some("'".to_owned()),
        _ => {}
    }

    if let Some(hex) = entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
        let parsed = pi_js::num::parse_int(hex, Some(16));
        if parsed.is_finite() && parsed.fract() == 0.0 {
            return decode_code_point(parsed as i64);
        }
        return None;
    }

    if let Some(dec) = entity.strip_prefix('#') {
        let parsed = pi_js::num::parse_int(dec, Some(10));
        if parsed.is_finite() && parsed.fract() == 0.0 {
            return decode_code_point(parsed as i64);
        }
        return None;
    }

    None
}

pub fn decode_html_entity_at(html: &str, index: usize) -> Option<DecodedHtmlEntity> {
    let tail = html.get(index + 1..)?;
    let rel_semicolon = tail.find(';')?;
    let semicolon_index = index + 1 + rel_semicolon;
    if semicolon_index - index > 16 {
        return None;
    }

    let entity = &html[index + 1..semicolon_index];
    let decoded = decode_html_entity(entity)?;
    Some(DecodedHtmlEntity {
        text: decoded,
        length: semicolon_index - index + 1,
    })
}
