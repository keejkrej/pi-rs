//! Port of `diff/libesm/convert/xml.js` (diff@8.0.4).

use crate::vendor::jsdiff::types::Change;

/// converts a list of change objects to a serialized XML format
pub fn convert_changes_to_xml(changes: &[Change]) -> String {
    let mut ret = String::new();
    for change in changes {
        if change.added {
            ret.push_str("<ins>");
        } else if change.removed {
            ret.push_str("<del>");
        }
        ret.push_str(&escape_html(&change.value));
        if change.added {
            ret.push_str("</ins>");
        } else if change.removed {
            ret.push_str("</del>");
        }
    }
    ret
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
