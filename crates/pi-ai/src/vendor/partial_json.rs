//! Port of npm `partial-json` 0.1.7 (node_modules/partial-json); vendored per PORTING.md §2.6.

use pi_js::{Error, Result};
use serde_json::{Map, Value};

/// Bitmask flags controlling which constructs may be partially parsed.
pub mod allow {
    pub const STR: u32 = 0b000_000_001;
    pub const NUM: u32 = 0b000_000_010;
    pub const ARR: u32 = 0b000_000_100;
    pub const OBJ: u32 = 0b000_001_000;
    pub const NULL: u32 = 0b000_010_000;
    pub const BOOL: u32 = 0b000_100_000;
    pub const NAN: u32 = 0b001_000_000;
    pub const INFINITY: u32 = 0b010_000_000;
    pub const _INFINITY: u32 = 0b100_000_000;
    pub const INF: u32 = INFINITY | _INFINITY;
    pub const SPECIAL: u32 = NULL | BOOL | INF | NAN;
    pub const ATOM: u32 = STR | NUM | SPECIAL;
    pub const COLLECTION: u32 = ARR | OBJ;
    pub const ALL: u32 = ATOM | COLLECTION;
}

/// Alias matching `import { parse } from "partial-json"`.
pub fn parse(json_string: &str, allow_partial: Option<u32>) -> Result<Value> {
    parse_json(json_string, allow_partial)
}

/// Parse potentially incomplete JSON according to `allow_partial` flags (defaults to [`allow::ALL`]).
pub fn parse_json(json_string: &str, allow_partial: Option<u32>) -> Result<Value> {
    let trimmed = json_string.trim();
    if trimmed.is_empty() {
        return Err(Error::msg(format!("{json_string} is empty")));
    }
    let allow_flags = allow_partial.unwrap_or(allow::ALL);
    let mut parser = Parser {
        src: trimmed,
        bytes: trimmed.as_bytes(),
        index: 0,
        allow: allow_flags,
    };
    parser.parse_any()
}

#[derive(Debug)]
enum ParseKind {
    Partial(String),
    Malformed(String),
}

impl From<ParseKind> for Error {
    fn from(value: ParseKind) -> Self {
        match value {
            ParseKind::Partial(msg) => Error::js("PartialJSON", msg),
            ParseKind::Malformed(msg) => Error::js("MalformedJSON", msg),
        }
    }
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    index: usize,
    allow: u32,
}

impl<'a> Parser<'a> {
    fn mark_partial(&self, msg: &str) -> Error {
        ParseKind::Partial(format!("{msg} at position {}", self.index)).into()
    }

    fn throw_malformed(&self, msg: &str) -> Error {
        ParseKind::Malformed(format!("{msg} at position {}", self.index)).into()
    }

    fn skip_blank(&mut self) {
        while self.index < self.bytes.len() && matches!(self.bytes[self.index], b' ' | b'\n' | b'\r' | b'\t') {
            self.index += 1;
        }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.index..]
    }

    fn matches_partial_keyword(&self, kw: &str, flag: u32) -> bool {
        let rem = self.rest();
        rem.starts_with(kw) || ((self.allow & flag) != 0 && rem.len() < kw.len() && kw.starts_with(rem))
    }

    fn parse_any(&mut self) -> Result<Value> {
        self.skip_blank();
        if self.index >= self.bytes.len() {
            return Err(self.mark_partial("Unexpected end of input"));
        }
        match self.bytes[self.index] {
            b'"' => return self.parse_str(),
            b'{' => return self.parse_obj(),
            b'[' => return self.parse_arr(),
            _ => {}
        }

        if self.matches_partial_keyword("null", allow::NULL) {
            self.index = (self.index + 4).min(self.bytes.len());
            return Ok(Value::Null);
        }
        if self.matches_partial_keyword("true", allow::BOOL) {
            self.index = (self.index + 4).min(self.bytes.len());
            return Ok(Value::Bool(true));
        }
        if self.matches_partial_keyword("false", allow::BOOL) {
            self.index = (self.index + 5).min(self.bytes.len());
            return Ok(Value::Bool(false));
        }
        // `serde_json::Value` cannot represent non-finite floats (Infinity / NaN); fail as malformed
        // rather than silently substituting `null`, so `parse_streaming_json` handles them uniformly.
        if self.matches_partial_keyword("Infinity", allow::INFINITY) {
            self.index = (self.index + 8).min(self.bytes.len());
            return Err(self.throw_malformed("Infinity is not representable in JSON Value"));
        }
        let rem = self.rest();
        if rem.starts_with("-Infinity")
            || ((self.allow & allow::_INFINITY) != 0 && rem.len() > 1 && rem.len() < 9 && "-Infinity".starts_with(rem))
        {
            self.index = (self.index + 9).min(self.bytes.len());
            return Err(self.throw_malformed("-Infinity is not representable in JSON Value"));
        }
        if self.matches_partial_keyword("NaN", allow::NAN) {
            self.index = (self.index + 3).min(self.bytes.len());
            return Err(self.throw_malformed("NaN is not representable in JSON Value"));
        }

        self.parse_num()
    }

    fn parse_str(&mut self) -> Result<Value> {
        let start = self.index;
        let mut escape = false;
        self.index += 1; // skip initial quote
        while self.index < self.bytes.len()
            && (self.bytes[self.index] != b'"' || (escape && self.bytes[self.index - 1] == b'\\'))
        {
            escape = if self.bytes[self.index] == b'\\' {
                !escape
            } else {
                false
            };
            self.index += 1;
        }

        if self.bytes.get(self.index) == Some(&b'"') {
            self.index += 1;
            let end = self.index - usize::from(escape);
            let slice = &self.src[start..end];
            return pi_js::json::parse(slice).map_err(|e| self.throw_malformed(&e.to_string()));
        } else if (self.allow & allow::STR) != 0 {
            let end = self.index - usize::from(escape);
            let candidate = format!("{}\"", &self.src[start..end]);
            if let Ok(v) = pi_js::json::parse(&candidate) {
                return Ok(v);
            }
            if let Some(rel) = self.src[start..self.index].rfind('\\') {
                let truncated = format!("{}\"", &self.src[start..start + rel]);
                return pi_js::json::parse(&truncated).map_err(|e| self.throw_malformed(&e.to_string()));
            }
            return Err(self.throw_malformed("Invalid partial string literal"));
        }

        Err(self.mark_partial("Unterminated string literal"))
    }

    fn parse_obj(&mut self) -> Result<Value> {
        self.index += 1; // skip initial brace
        self.skip_blank();
        let mut obj = Map::new();

        loop {
            if self.bytes.get(self.index) == Some(&b'}') {
                self.index += 1;
                return Ok(Value::Object(obj));
            }
            self.skip_blank();
            if self.index >= self.bytes.len() {
                if (self.allow & allow::OBJ) != 0 {
                    return Ok(Value::Object(obj));
                }
                return Err(self.mark_partial("Expected '}' at end of object"));
            }
            if self.bytes.get(self.index) == Some(&b'}') {
                self.index += 1;
                return Ok(Value::Object(obj));
            }

            let key = match self.parse_str() {
                Ok(Value::String(s)) => s,
                Ok(_) => unreachable!("parse_str only returns Value::String"),
                Err(e) => {
                    if (self.allow & allow::OBJ) != 0 {
                        return Ok(Value::Object(obj));
                    }
                    return Err(e);
                }
            };

            self.skip_blank();
            if self.index >= self.bytes.len() {
                if (self.allow & allow::OBJ) != 0 {
                    return Ok(Value::Object(obj));
                }
                return Err(self.mark_partial("Expected '}' at end of object"));
            }
            if self.bytes[self.index] != b':' {
                if (self.allow & allow::OBJ) != 0 {
                    return Ok(Value::Object(obj));
                }
                return Err(self.mark_partial("Expected ':' after object key"));
            }
            self.index += 1; // skip colon

            match self.parse_any() {
                Ok(val) => {
                    obj.insert(key, val);
                }
                Err(e) => {
                    if (self.allow & allow::OBJ) != 0 {
                        return Ok(Value::Object(obj));
                    }
                    return Err(e);
                }
            }

            self.skip_blank();
            if self.bytes.get(self.index) == Some(&b',') {
                self.index += 1; // skip comma
            }
        }
    }

    fn parse_arr(&mut self) -> Result<Value> {
        self.index += 1; // skip initial bracket
        let mut arr = Vec::new();

        loop {
            if self.bytes.get(self.index) == Some(&b']') {
                self.index += 1;
                return Ok(Value::Array(arr));
            }
            if self.index >= self.bytes.len() {
                if (self.allow & allow::ARR) != 0 {
                    return Ok(Value::Array(arr));
                }
                return Err(self.mark_partial("Expected ']' at end of array"));
            }

            match self.parse_any() {
                Ok(val) => arr.push(val),
                Err(e) => {
                    if (self.allow & allow::ARR) != 0 {
                        return Ok(Value::Array(arr));
                    }
                    return Err(e);
                }
            }

            self.skip_blank();
            if self.bytes.get(self.index) == Some(&b',') {
                self.index += 1; // skip comma
            }
        }
    }

    fn parse_num(&mut self) -> Result<Value> {
        if self.index == 0 {
            if self.src == "-" {
                return Err(self.throw_malformed("Not sure what '-' is"));
            }
            match pi_js::json::parse(self.src) {
                Ok(v) => return Ok(v),
                Err(e) => {
                    if (self.allow & allow::NUM) != 0 {
                        if let Some(pos) = self.src.rfind('e') {
                            if let Ok(v) = pi_js::json::parse(&self.src[..pos]) {
                                self.index = self.bytes.len();
                                return Ok(v);
                            }
                        }
                    }
                    return Err(self.throw_malformed(&e.to_string()));
                }
            }
        }

        let start = self.index;
        if self.bytes.get(self.index) == Some(&b'-') {
            self.index += 1;
        }
        while self.index < self.bytes.len() && !matches!(self.bytes[self.index], b',' | b']' | b'}') {
            self.index += 1;
        }
        if self.index == self.bytes.len() && (self.allow & allow::NUM) == 0 {
            return Err(self.mark_partial("Unterminated number literal"));
        }

        let num_str = &self.src[start..self.index];
        match pi_js::json::parse(num_str) {
            Ok(v) => Ok(v),
            Err(e) => {
                if num_str == "-" {
                    return Err(self.mark_partial("Not sure what '-' is"));
                }
                if let Some(pos) = num_str.rfind('e') {
                    if let Ok(v) = pi_js::json::parse(&num_str[..pos]) {
                        return Ok(v);
                    }
                }
                Err(self.throw_malformed(&e.to_string()))
            }
        }
    }
}
