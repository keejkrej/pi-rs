//! Port of packages/ai/src/utils/streaming-json.ts

use serde_json::{Map, Value};

use crate::types::{JsonObject, JsonValue};
use crate::utils::json_parse::{ParsedToolCallArguments, parse_tool_call_arguments};

/// A path segment from the root value: member key or array index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JsonPathSegment {
    Key(String),
    Index(usize),
}

impl From<&str> for JsonPathSegment {
    fn from(value: &str) -> Self {
        Self::Key(value.to_owned())
    }
}

impl From<String> for JsonPathSegment {
    fn from(value: String) -> Self {
        Self::Key(value)
    }
}

impl From<usize> for JsonPathSegment {
    fn from(value: usize) -> Self {
        Self::Index(value)
    }
}

pub type JsonPath = Vec<JsonPathSegment>;

/// One change a piece of text made to the value.
#[derive(Clone, Debug, PartialEq)]
pub enum JsonChange {
    Set { path: JsonPath, value: JsonValue },
    Append { path: JsonPath, text: String },
}

#[derive(Clone, Debug)]
enum JsonContainer {
    Object(Map<String, Value>),
    Array(Vec<Value>),
}

impl JsonContainer {
    fn is_array(&self) -> bool {
        matches!(self, Self::Array(_))
    }

    fn as_value(&self) -> Value {
        match self {
            Self::Object(map) => Value::Object(map.clone()),
            Self::Array(arr) => Value::Array(arr.clone()),
        }
    }
}

#[derive(Clone, Debug)]
enum Slot {
    Root,
    Member { key: String },
    Element { index: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FramePhase {
    Key,
    Colon,
    Value,
    Comma,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StringTarget {
    Key,
    Value,
}

#[derive(Clone, Debug)]
struct Frame {
    container: JsonContainer,
    slot: Option<Slot>,
    path: Option<JsonPath>,
    reports_inside: bool,
    size: usize,
    key: Option<String>,
    phase: FramePhase,
}

#[derive(Clone, Debug)]
struct OpenString {
    target: StringTarget,
    slot: Option<Slot>,
    text: String,
    escape: String,
    pending_high_surrogate: Option<u16>,
    path: Option<JsonPath>,
    active_change_idx: Option<usize>,
}

const MAX_REPORTED_DEPTH: usize = 32;
const CREDIT_PER_APPEND: usize = 256;
const CREDIT_PER_CHARACTER: usize = 8;
const MAX_PENDING_SCALAR_LENGTH: usize = 64;

fn is_valid_json_number(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    if bytes.get(i) == Some(&b'-') {
        i += 1;
    }
    match bytes.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            i += 1;
            while matches!(bytes.get(i), Some(b'0'..=b'9')) {
                i += 1;
            }
        }
        _ => return false,
    }
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        let frac_start = i;
        while matches!(bytes.get(i), Some(b'0'..=b'9')) {
            i += 1;
        }
        if i == frac_start {
            return false;
        }
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let exp_start = i;
        while matches!(bytes.get(i), Some(b'0'..=b'9')) {
            i += 1;
        }
        if i == exp_start {
            return false;
        }
    }
    i == bytes.len()
}

fn strip_incomplete_exponent(s: &str) -> Option<&str> {
    let prefix = s
        .strip_suffix("e+")
        .or_else(|| s.strip_suffix("e-"))
        .or_else(|| s.strip_suffix("E+"))
        .or_else(|| s.strip_suffix("E-"))
        .or_else(|| s.strip_suffix('e'))
        .or_else(|| s.strip_suffix('E'))?;
    if prefix.contains(['e', 'E']) || !is_valid_json_number(prefix) {
        return None;
    }
    Some(prefix)
}

const LITERALS: [(&str, fn() -> Value); 3] = [
    ("true", || Value::Bool(true)),
    ("false", || Value::Bool(false)),
    ("null", || Value::Null),
];

/// Incremental, tolerant parser of JSON text arriving in pieces.
pub struct StreamingJsonParser {
    root: Option<Value>,
    done: bool,
    stack: Vec<Frame>,
    open_string: Option<OpenString>,
    scalar: String,
    scalar_slot: Option<Slot>,
    path_cost: usize,
    credit: usize,
    dirty: bool,
    snapshot: Option<Value>,
    changes: Vec<JsonChange>,
}

impl Default for StreamingJsonParser {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamingJsonParser {
    pub fn new() -> Self {
        Self {
            root: None,
            done: false,
            stack: Vec::new(),
            open_string: None,
            scalar: String::new(),
            scalar_slot: None,
            path_cost: 0,
            credit: 0,
            dirty: false,
            snapshot: None,
            changes: Vec::new(),
        }
    }

    /// Read the next piece of text; returns the changes it made, in order.
    pub fn append(&mut self, text: &str) -> Vec<JsonChange> {
        // Credit scales by UTF-16 code units approx / char count; len() is O(1) and sufficient for snapshot budget.
        self.credit = self
            .credit
            .saturating_add(CREDIT_PER_APPEND)
            .saturating_add(CREDIT_PER_CHARACTER.saturating_mul(text.chars().count()));
        self.changes.clear();
        if let Some(open) = &mut self.open_string {
            open.active_change_idx = None;
        }

        let mut index = 0usize;
        while index < text.len() && !self.done {
            if self.open_string.is_some() {
                index = self.read_string(text, index);
            } else if !self.scalar.is_empty() {
                index = self.read_scalar(text, index);
            } else if let Some(ch) = text[index..].chars().next() {
                index += ch.len_utf8();
                self.structural(ch);
            } else {
                break;
            }
        }
        std::mem::take(&mut self.changes)
    }

    /// The value of the text read so far (`None` before a value starts).
    pub fn snapshot(&mut self) -> Option<Value> {
        if !self.dirty {
            return self.snapshot.clone();
        }
        let pending = self.pending_scalar();
        let cost = self.path_cost + usize::from(pending.is_some());
        if cost > self.credit {
            return self.snapshot.clone();
        }
        self.credit -= cost;
        self.dirty = false;
        self.snapshot = self.copy_open_path(pending.as_ref());
        self.snapshot.clone()
    }

    fn read_string(&mut self, text: &str, start: usize) -> usize {
        let mut run_start = start;
        let mut iter = text[start..].char_indices();
        while let Some((rel_idx, ch)) = iter.next() {
            let idx = start + rel_idx;
            let has_escape = self.open_string.as_ref().is_some_and(|o| !o.escape.is_empty());
            if has_escape {
                if self.continue_escape(ch) {
                    run_start = idx + ch.len_utf8();
                    continue;
                }
                // Not part of the escape: read `ch` normally below.
                run_start = idx;
            }

            if ch == '"' {
                if idx > run_start {
                    self.append_to_open_string(&text[run_start..idx]);
                }
                self.end_string();
                return idx + 1;
            }
            if ch == '\\' {
                if idx > run_start {
                    self.append_to_open_string(&text[run_start..idx]);
                }
                if let Some(open) = &mut self.open_string {
                    open.escape = "\\".to_owned();
                }
                run_start = idx + 1;
            }
        }

        if text.len() > run_start {
            self.append_to_open_string(&text[run_start..]);
        }
        text.len()
    }

    fn read_scalar(&mut self, text: &str, start: usize) -> usize {
        for (rel_idx, ch) in text[start..].char_indices() {
            if is_delimiter(ch) {
                let end = start + rel_idx;
                if end > start {
                    self.scalar.push_str(&text[start..end]);
                    if self.scalar_slot.is_some() {
                        self.dirty = true;
                    }
                }
                self.end_scalar();
                return end;
            }
        }
        if text.len() > start {
            self.scalar.push_str(&text[start..]);
            if self.scalar_slot.is_some() {
                self.dirty = true;
            }
        }
        text.len()
    }

    fn structural(&mut self, ch: char) {
        match ch {
            ' ' | '\t' | '\n' | '\r' => {}
            '"' => self.start_string(),
            '{' => self.open_container(JsonContainer::Object(Map::new())),
            '[' => self.open_container(JsonContainer::Array(Vec::new())),
            '}' | ']' => self.close_container(),
            ':' => {
                if let Some(frame) = self.stack.last_mut()
                    && !frame.container.is_array()
                    && frame.phase == FramePhase::Colon
                {
                    frame.phase = FramePhase::Value;
                }
            }
            ',' => {
                if let Some(frame) = self.stack.last_mut() {
                    frame.phase = if frame.container.is_array() {
                        FramePhase::Value
                    } else {
                        FramePhase::Key
                    };
                }
            }
            _ => {
                self.scalar.clear();
                self.scalar.push(ch);
                self.scalar_slot = self.value_slot();
                if self.scalar_slot.is_some() {
                    self.dirty = true;
                }
            }
        }
    }

    fn value_slot(&self) -> Option<Slot> {
        let Some(frame) = self.stack.last() else {
            return if self.root.is_none() { Some(Slot::Root) } else { None };
        };
        if let JsonContainer::Array(arr) = &frame.container {
            return Some(Slot::Element { index: arr.len() });
        }
        // A missing colon is tolerated: `{"a" 1}` reads as `{"a": 1}`.
        if matches!(frame.phase, FramePhase::Value | FramePhase::Colon) {
            if let Some(key) = &frame.key {
                return Some(Slot::Member { key: key.clone() });
            }
        }
        None
    }

    fn start_string(&mut self) {
        if let Some(frame) = self.stack.last()
            && !frame.container.is_array()
            && matches!(frame.phase, FramePhase::Key | FramePhase::Comma)
        {
            self.open_string = Some(OpenString {
                target: StringTarget::Key,
                slot: None,
                text: String::new(),
                escape: String::new(),
                pending_high_surrogate: None,
                path: None,
                active_change_idx: None,
            });
            return;
        }

        let slot = self.value_slot();
        let path = self.reported_path(slot.as_ref());
        self.place(slot.as_ref(), Value::String(String::new()));
        let active_change_idx = if let Some(p) = path.clone() {
            Some(self.report(JsonChange::Set {
                path: p,
                value: Value::String(String::new()),
            }))
        } else {
            None
        };
        self.open_string = Some(OpenString {
            target: StringTarget::Value,
            slot,
            text: String::new(),
            escape: String::new(),
            pending_high_surrogate: None,
            path,
            active_change_idx,
        });
    }

    fn reported_path(&self, slot: Option<&Slot>) -> Option<JsonPath> {
        let slot = slot?;
        match slot {
            Slot::Root => Some(Vec::new()),
            Slot::Member { key } => {
                let frame = self.stack.last()?;
                if !frame.reports_inside {
                    return None;
                }
                let mut p = frame.path.clone()?;
                p.push(JsonPathSegment::Key(key.clone()));
                Some(p)
            }
            Slot::Element { index } => {
                let frame = self.stack.last()?;
                if !frame.reports_inside {
                    return None;
                }
                let mut p = frame.path.clone()?;
                p.push(JsonPathSegment::Index(*index));
                Some(p)
            }
        }
    }

    fn report(&mut self, change: JsonChange) -> usize {
        self.changes.push(change);
        self.changes.len() - 1
    }

    fn flush_pending_surrogate(&mut self) {
        let high = self.open_string.as_mut().and_then(|o| o.pending_high_surrogate.take());
        if high.is_some() {
            self.push_decoded_text("\u{FFFD}");
        }
    }

    fn push_decoded_text(&mut self, segment: &str) {
        let Some(mut open) = self.open_string.take() else {
            return;
        };
        open.text.push_str(segment);
        if open.target == StringTarget::Value {
            self.place(open.slot.as_ref(), Value::String(open.text.clone()));
            if let Some(path) = &open.path {
                let extended = open.active_change_idx.is_some_and(|idx| {
                    idx + 1 == self.changes.len()
                        && match &mut self.changes[idx] {
                            JsonChange::Set {
                                value: Value::String(s),
                                ..
                            } => {
                                s.push_str(segment);
                                true
                            }
                            JsonChange::Append { text, .. } => {
                                text.push_str(segment);
                                true
                            }
                            _ => false,
                        }
                });
                if !extended {
                    let idx = self.report(JsonChange::Append {
                        path: path.clone(),
                        text: segment.to_owned(),
                    });
                    open.active_change_idx = Some(idx);
                }
            }
        }
        self.open_string = Some(open);
    }

    fn append_to_open_string(&mut self, segment: &str) {
        if segment.is_empty() {
            return;
        }
        self.flush_pending_surrogate();
        self.push_decoded_text(segment);
    }

    fn continue_escape(&mut self, ch: char) -> bool {
        let esc_is_backslash = self.open_string.as_ref().is_some_and(|o| o.escape == "\\");
        if esc_is_backslash {
            if let Some(decoded) = simple_escape(ch) {
                if let Some(o) = &mut self.open_string {
                    o.escape.clear();
                }
                self.flush_pending_surrogate();
                let mut buf = [0u8; 4];
                self.push_decoded_text(decoded.encode_utf8(&mut buf));
                return true;
            }
            if ch == 'u' {
                if let Some(o) = &mut self.open_string {
                    o.escape = "\\u".to_owned();
                }
                return true;
            }
            // Invalid escape: keep the backslash as text and read `ch` normally.
            if let Some(o) = &mut self.open_string {
                o.escape.clear();
            }
            self.flush_pending_surrogate();
            self.push_decoded_text("\\");
            return false;
        }

        if ch.is_ascii_hexdigit() {
            let complete_hex = {
                let Some(o) = &mut self.open_string else {
                    return false;
                };
                o.escape.push(ch);
                if o.escape.len() < 6 {
                    return true;
                }
                let hex = o.escape[2..].to_owned();
                o.escape.clear();
                hex
            };
            if let Ok(code) = u16::from_str_radix(&complete_hex, 16) {
                if (0xD800..=0xDBFF).contains(&code) {
                    self.flush_pending_surrogate();
                    if let Some(o) = &mut self.open_string {
                        o.pending_high_surrogate = Some(code);
                    }
                    return true;
                }
                if (0xDC00..=0xDFFF).contains(&code) {
                    let prev = self.open_string.as_mut().and_then(|o| o.pending_high_surrogate.take());
                    if let Some(hi) = prev {
                        let scalar = 0x10000 + (((u32::from(hi) - 0xD800) << 10) | (u32::from(code) - 0xDC00));
                        let decoded = char::from_u32(scalar).unwrap_or('\u{FFFD}');
                        let mut buf = [0u8; 4];
                        self.push_decoded_text(decoded.encode_utf8(&mut buf));
                        return true;
                    }
                    self.push_decoded_text("\u{FFFD}");
                    return true;
                }
                self.flush_pending_surrogate();
                let decoded = char::from_u32(u32::from(code)).unwrap_or('\u{FFFD}');
                let mut buf = [0u8; 4];
                self.push_decoded_text(decoded.encode_utf8(&mut buf));
                return true;
            }
            return true;
        }

        // `\u` without four hex digits: keep it as text and read `ch` normally.
        self.flush_pending_surrogate();
        let literal = self
            .open_string
            .as_mut()
            .map(|o| std::mem::take(&mut o.escape))
            .unwrap_or_default();
        self.push_decoded_text(&literal);
        false
    }

    fn end_string(&mut self) {
        self.flush_pending_surrogate();
        let Some(open) = self.open_string.take() else {
            return;
        };
        if open.target == StringTarget::Key {
            if let Some(frame) = self.stack.last_mut() {
                frame.key = Some(open.text);
                frame.phase = FramePhase::Colon;
            }
            return;
        }
        self.value_ended();
    }

    fn end_scalar(&mut self) {
        let token = std::mem::take(&mut self.scalar);
        let slot = self.scalar_slot.take();
        if let Some(value) = scalar_value(&token) {
            self.place(slot.as_ref(), value.clone());
            if let Some(path) = self.reported_path(slot.as_ref()) {
                self.report(JsonChange::Set { path, value });
            }
        }
        self.value_ended();
    }

    fn pending_scalar(&self) -> Option<(Slot, Value)> {
        let slot = self.scalar_slot.as_ref()?;
        if self.scalar.is_empty() || self.scalar.len() > MAX_PENDING_SCALAR_LENGTH {
            return None;
        }
        let value = pending_scalar_value(&self.scalar)?;
        Some((slot.clone(), value))
    }

    fn open_container(&mut self, container: JsonContainer) {
        let empty_value = if container.is_array() {
            Value::Array(Vec::new())
        } else {
            Value::Object(Map::new())
        };
        let slot = self.value_slot();
        let path = self.reported_path(slot.as_ref());
        self.place(slot.as_ref(), empty_value.clone());
        let reports_inside = path.as_ref().is_some_and(|p| p.len() < MAX_REPORTED_DEPTH);
        self.stack.push(Frame {
            container,
            slot,
            path: path.clone(),
            reports_inside,
            size: 0,
            key: None,
            phase: FramePhase::Value,
        });
        if let Some(last) = self.stack.last_mut() {
            if !last.container.is_array() {
                last.phase = FramePhase::Key;
            }
        }
        self.path_cost += 1;
        self.dirty = true;
        if let Some(p) = path {
            self.report(JsonChange::Set {
                path: p,
                value: empty_value,
            });
        }
    }

    fn close_container(&mut self) {
        let Some(frame) = self.stack.pop() else {
            return;
        };
        self.path_cost -= frame.size + 1;
        self.dirty = true;
        if let Some(path) = &frame.path
            && !frame.reports_inside
        {
            self.report(JsonChange::Set {
                path: path.clone(),
                value: frame.container.as_value(),
            });
        }
        // Propagate the finalized closed container into its parent (or root).
        let closed = frame.container.as_value();
        if let Some(slot) = frame.slot.as_ref() {
            match slot {
                Slot::Root => self.root = Some(closed),
                _ => self.place_closed(slot, closed),
            }
        }
        self.value_ended();
    }

    fn value_ended(&mut self) {
        if let Some(frame) = self.stack.last_mut() {
            frame.phase = FramePhase::Comma;
        } else if self.root.is_some() {
            self.done = true;
        }
    }

    fn place_closed(&mut self, slot: &Slot, value: Value) {
        if let Some(frame) = self.stack.last_mut() {
            set_container_slot(&mut frame.container, slot, value);
        }
    }

    fn place(&mut self, slot: Option<&Slot>, value: Value) {
        let Some(slot) = slot else {
            return;
        };
        self.dirty = true;
        match slot {
            Slot::Root => {
                self.root = Some(value);
            }
            Slot::Element { index } => {
                if let Some(frame) = self.stack.last_mut()
                    && let JsonContainer::Array(arr) = &mut frame.container
                {
                    if *index == arr.len() {
                        frame.size += 1;
                        self.path_cost += 1;
                        arr.push(value);
                    } else if *index < arr.len() {
                        arr[*index] = value;
                    }
                }
            }
            Slot::Member { key } => {
                if let Some(frame) = self.stack.last_mut()
                    && let JsonContainer::Object(map) = &mut frame.container
                {
                    if !map.contains_key(key) {
                        frame.size += 1;
                        self.path_cost += 1;
                    }
                    map.insert(key.clone(), value);
                }
            }
        }
    }

    fn copy_open_path(&self, pending: Option<&(Slot, Value)>) -> Option<Value> {
        if self.stack.is_empty() {
            if let Some((Slot::Root, val)) = pending {
                return Some(val.clone());
            }
            return self.root.clone();
        }
        let mut child: Option<Value> = None;
        let mut child_slot: Option<&Slot> = None;
        for depth in (0..self.stack.len()).rev() {
            let frame = &self.stack[depth];
            let mut copy = frame.container.clone();
            if depth == self.stack.len() - 1
                && let Some((pslot, pval)) = pending
            {
                set_container_slot(&mut copy, pslot, pval.clone());
            }
            if let (Some(ch), Some(cslot)) = (child, child_slot) {
                set_container_slot(&mut copy, cslot, ch);
            }
            child = Some(copy.as_value());
            child_slot = frame.slot.as_ref();
        }
        if matches!(child_slot, Some(Slot::Root)) {
            child
        } else {
            self.root.clone()
        }
    }
}

/// The arguments of one streaming tool call: raw text for full validation on completion,
/// plus a [`StreamingJsonParser`] for incremental partial arguments.
pub struct ToolCallArgumentsStream {
    text: String,
    parser: StreamingJsonParser,
    last_snapshot: Option<Value>,
    arguments: JsonObject,
}

impl Default for ToolCallArgumentsStream {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolCallArgumentsStream {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            parser: StreamingJsonParser::new(),
            last_snapshot: None,
            arguments: JsonObject::new(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn append(&mut self, delta: &str) -> JsonObject {
        self.text.push_str(delta);
        let _ = self.parser.append(delta);
        self.partial()
    }

    /// Take the provider's complete argument text. When it continues `self.text`, returns `delta: Some(missing)`;
    /// otherwise resets and returns `delta: None`.
    pub fn replace(&mut self, text: &str) -> (Option<String>, JsonObject) {
        if let Some(delta) = text.strip_prefix(&self.text) {
            let delta = delta.to_owned();
            let args = if delta.is_empty() {
                self.partial()
            } else {
                self.append(&delta)
            };
            return (Some(delta), args);
        }
        self.text.clear();
        self.parser = StreamingJsonParser::new();
        self.last_snapshot = None;
        self.arguments = JsonObject::new();
        (None, self.append(text))
    }

    pub fn finish(&self) -> ParsedToolCallArguments {
        parse_tool_call_arguments(Some(&self.text))
    }

    fn partial(&mut self) -> JsonObject {
        let snapshot = self.parser.snapshot();
        if snapshot == self.last_snapshot {
            return self.arguments.clone();
        }
        self.last_snapshot = snapshot.clone();
        self.arguments = match snapshot {
            Some(Value::Object(map)) => map.into_iter().collect(),
            _ => JsonObject::new(),
        };
        self.arguments.clone()
    }
}

fn simple_escape(ch: char) -> Option<char> {
    match ch {
        '"' => Some('"'),
        '\\' => Some('\\'),
        '/' => Some('/'),
        'b' => Some('\x08'),
        'f' => Some('\x0c'),
        'n' => Some('\n'),
        'r' => Some('\r'),
        't' => Some('\t'),
        _ => None,
    }
}

fn is_delimiter(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r' | ',' | ':' | '{' | '}' | '[' | ']' | '"')
}

fn parse_js_json_number(token: &str) -> Option<Value> {
    if !is_valid_json_number(token) {
        return None;
    }
    if let Ok(val) = serde_json::from_str::<Value>(token)
        && val.is_number()
    {
        return Some(val);
    }
    let parsed: f64 = token.parse().ok()?;
    if parsed.is_finite() {
        Some(pi_js::json::number_value(parsed))
    } else {
        None
    }
}

fn scalar_value(token: &str) -> Option<Value> {
    for (literal, ctor) in LITERALS {
        if token == literal {
            return Some(ctor());
        }
    }
    parse_js_json_number(token)
}

fn pending_scalar_value(token: &str) -> Option<Value> {
    for (literal, ctor) in LITERALS {
        if literal.starts_with(token) {
            return Some(ctor());
        }
    }
    if let Some(n) = parse_js_json_number(token) {
        return Some(n);
    }
    let prefix = strip_incomplete_exponent(token)?;
    parse_js_json_number(prefix)
}

fn set_container_slot(container: &mut JsonContainer, slot: &Slot, value: Value) {
    match (container, slot) {
        (JsonContainer::Array(arr), Slot::Element { index }) => {
            if *index == arr.len() {
                arr.push(value);
            } else if *index < arr.len() {
                arr[*index] = value;
            }
        }
        (JsonContainer::Object(map), Slot::Member { key }) => {
            map.insert(key.clone(), value);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn streams_objects_strings_escapes_and_changes() {
        let mut parser = StreamingJsonParser::new();
        let c1 = parser.append(r#"{"msg":"hel"#);
        assert_eq!(parser.snapshot(), Some(json!({ "msg": "hel" })));
        assert_eq!(
            c1,
            vec![
                JsonChange::Set {
                    path: vec![],
                    value: json!({}),
                },
                JsonChange::Set {
                    path: vec![JsonPathSegment::from("msg")],
                    value: json!("hel"),
                }
            ]
        );

        let c2 = parser.append(r#"lo\n😀\uD83D"#);
        assert_eq!(parser.snapshot(), Some(json!({ "msg": "hello\n😀" })));
        assert_eq!(
            c2,
            vec![JsonChange::Append {
                path: vec![JsonPathSegment::from("msg")],
                text: "lo\n😀".to_owned(),
            }]
        );

        let _ = parser.append(r#"\uDE00","count":12e+"#);
        assert_eq!(parser.snapshot(), Some(json!({ "msg": "hello\n😀😀", "count": 12 })));

        let _ = parser.append(r#"0,"flag":tr"#);
        assert_eq!(
            parser.snapshot(),
            Some(json!({ "msg": "hello\n😀😀", "count": 12.0, "flag": true }))
        );

        let _ = parser.append(r#"ue}"#);
        assert_eq!(
            parser.snapshot(),
            Some(json!({ "msg": "hello\n😀😀", "count": 12.0, "flag": true }))
        );
    }

    #[test]
    fn tool_call_arguments_stream_tracks_partial_and_finish() {
        let mut s = ToolCallArgumentsStream::new();
        let a1 = s.append("{\"path\":\"src/");
        assert_eq!(a1.get("path"), Some(&json!("src/")));
        let (delta, a2) = s.replace("{\"path\":\"src/main.rs\",\"line\":5}");
        assert_eq!(delta.as_deref(), Some("main.rs\",\"line\":5}"));
        assert_eq!(a2.get("line"), Some(&json!(5)));
        let finished = s.finish();
        assert_eq!(finished.arguments_error, None);
        assert_eq!(finished.arguments.get("path"), Some(&json!("src/main.rs")));

        let mut bad = ToolCallArgumentsStream::new();
        bad.append("{\"broken\":");
        let fin = bad.finish();
        assert!(fin.arguments.is_empty());
        assert!(fin.arguments_error.is_some());
    }
}
