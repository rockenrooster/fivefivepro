use std::collections::BTreeMap;
use std::str;

use crate::error::{Pro55Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(i64),
    String(String),
    Array(Vec<JsonValue>),
    Object(BTreeMap<String, JsonValue>),
}

impl JsonValue {
    pub fn as_object(&self) -> Option<&BTreeMap<String, JsonValue>> {
        match self {
            JsonValue::Object(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[JsonValue]> {
        match self {
            JsonValue::Array(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsonValue::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            JsonValue::Number(value) => Some(*value),
            _ => None,
        }
    }
}

pub fn parse_json(input: &[u8]) -> Result<JsonValue> {
    let mut parser = Parser { input, pos: 0 };
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.pos != parser.input.len() {
        return Err(Pro55Error::new("trailing data after JSON document"));
    }
    Ok(value)
}

pub fn escape_json_string(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 2);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch < ' ' => {
                out.push_str("\\u");
                out.push(hex((ch as u32 >> 12) & 0xF));
                out.push(hex((ch as u32 >> 8) & 0xF));
                out.push(hex((ch as u32 >> 4) & 0xF));
                out.push(hex(ch as u32 & 0xF));
            }
            _ => out.push(ch),
        }
    }
    out
}

fn hex(n: u32) -> char {
    match n {
        0..=9 => char::from_u32(u32::from(b'0') + n).unwrap(),
        10..=15 => char::from_u32(u32::from(b'a') + n - 10).unwrap(),
        _ => '?',
    }
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn err(&self, message: impl Into<String>) -> Pro55Error {
        Pro55Error::new(format!(
            "invalid JSON at byte {}: {}",
            self.pos,
            message.into()
        ))
    }

    fn skip_ws(&mut self) {
        while self.pos < self.input.len()
            && matches!(self.input[self.pos], b' ' | b'\n' | b'\r' | b'\t')
        {
            self.pos += 1;
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue> {
        self.skip_ws();
        if self.pos >= self.input.len() {
            return Err(self.err("unexpected end of input"));
        }
        match self.input[self.pos] {
            b'n' => self.parse_keyword(b"null", JsonValue::Null),
            b't' => self.parse_keyword(b"true", JsonValue::Bool(true)),
            b'f' => self.parse_keyword(b"false", JsonValue::Bool(false)),
            b'"' => self.parse_string().map(JsonValue::String),
            b'[' => self.parse_array(),
            b'{' => self.parse_object(),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => Err(self.err("unexpected character")),
        }
    }

    fn parse_keyword(&mut self, keyword: &[u8], value: JsonValue) -> Result<JsonValue> {
        if self.input.get(self.pos..self.pos + keyword.len()) == Some(keyword) {
            self.pos += keyword.len();
            Ok(value)
        } else {
            Err(self.err("invalid keyword"))
        }
    }

    fn parse_string(&mut self) -> Result<String> {
        if self.input.get(self.pos) != Some(&b'"') {
            return Err(self.err("expected string"));
        }
        self.pos += 1;
        let mut out = String::new();
        let mut segment_start = self.pos;
        while self.pos < self.input.len() {
            match self.input[self.pos] {
                b'"' => {
                    let segment = str::from_utf8(&self.input[segment_start..self.pos])
                        .map_err(|_| self.err("string is not valid UTF-8"))?;
                    out.push_str(segment);
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    let segment = str::from_utf8(&self.input[segment_start..self.pos])
                        .map_err(|_| self.err("string is not valid UTF-8"))?;
                    out.push_str(segment);
                    self.pos += 1;
                    if self.pos >= self.input.len() {
                        return Err(self.err("truncated escape"));
                    }
                    match self.input[self.pos] {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            self.pos += 1;
                            let first = self.parse_hex4()?;
                            if (0xD800..=0xDBFF).contains(&first) {
                                if self.input.get(self.pos) != Some(&b'\\')
                                    || self.input.get(self.pos + 1) != Some(&b'u')
                                {
                                    return Err(self.err("missing low surrogate"));
                                }
                                self.pos += 2;
                                let second = self.parse_hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&second) {
                                    return Err(self.err("invalid low surrogate"));
                                }
                                let scalar =
                                    0x1_0000 + ((first - 0xD800) << 10) + (second - 0xDC00);
                                let ch = char::from_u32(scalar)
                                    .ok_or_else(|| self.err("invalid Unicode scalar"))?;
                                out.push(ch);
                                segment_start = self.pos;
                                continue;
                            }
                            if (0xDC00..=0xDFFF).contains(&first) {
                                return Err(self.err("unpaired low surrogate"));
                            }
                            let ch = char::from_u32(first)
                                .ok_or_else(|| self.err("invalid Unicode scalar"))?;
                            out.push(ch);
                            segment_start = self.pos;
                            continue;
                        }
                        _ => return Err(self.err("invalid escape")),
                    }
                    self.pos += 1;
                    segment_start = self.pos;
                }
                b if b < 0x20 => return Err(self.err("control character in string")),
                _ => self.pos += 1,
            }
        }
        Err(self.err("unterminated string"))
    }

    fn parse_hex4(&mut self) -> Result<u32> {
        if self.pos + 4 > self.input.len() {
            return Err(self.err("truncated Unicode escape"));
        }
        let mut value = 0u32;
        for _ in 0..4 {
            let b = self.input[self.pos];
            self.pos += 1;
            value = (value << 4)
                | match b {
                    b'0'..=b'9' => u32::from(b - b'0'),
                    b'a'..=b'f' => u32::from(b - b'a' + 10),
                    b'A'..=b'F' => u32::from(b - b'A' + 10),
                    _ => return Err(self.err("invalid Unicode escape")),
                };
        }
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<JsonValue> {
        let start = self.pos;
        if self.input[self.pos] == b'-' {
            self.pos += 1;
        }
        if self.pos >= self.input.len() {
            return Err(self.err("truncated number"));
        }
        match self.input[self.pos] {
            b'0' => self.pos += 1,
            b'1'..=b'9' => {
                self.pos += 1;
                while self.pos < self.input.len() && self.input[self.pos].is_ascii_digit() {
                    self.pos += 1;
                }
            }
            _ => return Err(self.err("invalid number")),
        }
        if self.pos < self.input.len() && matches!(self.input[self.pos], b'.' | b'e' | b'E') {
            return Err(self.err("floating-point numbers are not supported in this manifest"));
        }
        let text =
            str::from_utf8(&self.input[start..self.pos]).map_err(|_| self.err("invalid number"))?;
        let value = text
            .parse::<i64>()
            .map_err(|_| self.err("number out of range"))?;
        Ok(JsonValue::Number(value))
    }

    fn parse_array(&mut self) -> Result<JsonValue> {
        self.pos += 1;
        let mut values = Vec::new();
        loop {
            self.skip_ws();
            if self.pos >= self.input.len() {
                return Err(self.err("unterminated array"));
            }
            if self.input[self.pos] == b']' {
                self.pos += 1;
                return Ok(JsonValue::Array(values));
            }
            values.push(self.parse_value()?);
            self.skip_ws();
            if self.pos >= self.input.len() {
                return Err(self.err("unterminated array"));
            }
            match self.input[self.pos] {
                b',' => self.pos += 1,
                b']' => {
                    self.pos += 1;
                    return Ok(JsonValue::Array(values));
                }
                _ => return Err(self.err("expected comma or closing bracket")),
            }
        }
    }

    fn parse_object(&mut self) -> Result<JsonValue> {
        self.pos += 1;
        let mut map = BTreeMap::new();
        loop {
            self.skip_ws();
            if self.pos >= self.input.len() {
                return Err(self.err("unterminated object"));
            }
            if self.input[self.pos] == b'}' {
                self.pos += 1;
                return Ok(JsonValue::Object(map));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.input.get(self.pos) != Some(&b':') {
                return Err(self.err("expected colon"));
            }
            self.pos += 1;
            let value = self.parse_value()?;
            map.insert(key, value);
            self.skip_ws();
            if self.pos >= self.input.len() {
                return Err(self.err("unterminated object"));
            }
            match self.input[self.pos] {
                b',' => self.pos += 1,
                b'}' => {
                    self.pos += 1;
                    return Ok(JsonValue::Object(map));
                }
                _ => return Err(self.err("expected comma or closing brace")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_manifest_shape() {
        let input = br#"{"entries":[{"path":"a.txt","size":12}],"format":"5.5pro-path-archive","version":1}"#;
        let doc = parse_json(input).unwrap();
        let obj = doc.as_object().unwrap();
        assert_eq!(obj.get("version").unwrap().as_i64(), Some(1));
        assert_eq!(obj.get("entries").unwrap().as_array().unwrap().len(), 1);
    }

    #[test]
    fn parses_unicode_escape() {
        let doc = parse_json(br#"{"x":"hi \u263a"}"#).unwrap();
        assert_eq!(
            doc.as_object().unwrap().get("x").unwrap().as_str(),
            Some("hi ☺")
        );
    }

    #[test]
    fn escapes_json_string() {
        assert_eq!(escape_json_string("a\"b\\c\n"), "a\\\"b\\\\c\\n");
    }
}
