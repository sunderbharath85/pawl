//! A small JSON / JSONC reader that remembers where every value sits, so a writer can change one
//! member of an object and leave every other byte of the file as it was (comments, key order,
//! indentation, line endings).
//!
//! JSONC is what VS Code reads: JSON plus `//` and `/* */` comments and trailing commas. Strict mode
//! refuses both. Errors name a line and column, never the file's text (it may hold secrets).

use serde_json::{Map, Value};

/// Deeper documents are refused (a config file never nests this deep).
const MAX_DEPTH: usize = 64;

/// A value and its byte range `start..end` in the text.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub(crate) enum Kind {
    Object(Vec<Member>),
    Array(Vec<Node>),
    String,
    /// Numbers, true / false / null: `serde_json` reads the slice.
    Scalar,
}

#[derive(Debug, Clone)]
pub(crate) struct Member {
    pub key: String,
    /// Where the key's opening quote is.
    pub key_start: usize,
    pub value: Node,
    /// The comma after the value, if any.
    pub comma: Option<usize>,
}

impl Node {
    pub fn members(&self) -> Option<&[Member]> {
        match &self.kind {
            Kind::Object(m) => Some(m),
            _ => None,
        }
    }

    /// The member called `key` (the last one if a key repeats, as JSON readers do).
    pub fn get(&self, key: &str) -> Option<&Member> {
        self.members()?.iter().rev().find(|m| m.key == key)
    }

    /// How many members are called `key`.
    pub fn count(&self, key: &str) -> usize {
        self.members().map(|ms| ms.iter().filter(|m| m.key == key).count()).unwrap_or(0)
    }

    /// The value as `serde_json` (comments and trailing commas dropped; a repeated key: the last).
    pub fn to_value(&self, text: &str) -> Value {
        match &self.kind {
            Kind::Object(ms) => {
                let mut m = Map::new();
                for x in ms {
                    m.insert(x.key.clone(), x.value.to_value(text));
                }
                Value::Object(m)
            }
            Kind::Array(items) => Value::Array(items.iter().map(|n| n.to_value(text)).collect()),
            Kind::String => Value::String(unescape(&text[self.start + 1..self.end - 1])),
            Kind::Scalar => serde_json::from_str(&text[self.start..self.end]).unwrap_or(Value::Null),
        }
    }
}

/// A JSON string body (between the quotes, escapes checked by the parser) as text. A lone
/// surrogate escape (which JSON allows) becomes U+FFFD; the file's bytes are never rewritten.
fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let hex4 = |chars: &mut std::iter::Peekable<std::str::Chars<'_>>| -> u32 {
        (0..4).filter_map(|_| chars.next()).fold(0, |n, c| n * 16 + c.to_digit(16).unwrap_or(0))
    };
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let hi = hex4(&mut chars);
                if (0xD800..0xDC00).contains(&hi) {
                    let mut look = chars.clone();
                    if look.next() == Some('\\') && look.next() == Some('u') {
                        let lo = hex4(&mut look);
                        if (0xDC00..0xE000).contains(&lo) {
                            chars = look;
                            out.push(char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)).unwrap_or('\u{fffd}'));
                            continue;
                        }
                    }
                    out.push('\u{fffd}');
                } else {
                    out.push(char::from_u32(hi).unwrap_or('\u{fffd}'));
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

struct Parser<'a> {
    b: &'a [u8],
    pos: usize,
    jsonc: bool,
}

/// Parse a whole document; the top level must be an object. `jsonc`: comments and trailing
/// commas are allowed. A leading BOM is skipped.
pub(crate) fn parse(text: &str, jsonc: bool) -> Result<Node, String> {
    let mut p = Parser { b: text.as_bytes(), pos: 0, jsonc };
    if text.starts_with('\u{feff}') {
        p.pos = '\u{feff}'.len_utf8();
    }
    p.ws()?;
    if p.peek() != Some(b'{') {
        return Err("the file does not hold a JSON object".into());
    }
    let root = p.value(0)?;
    p.ws()?;
    if p.pos != p.b.len() {
        return Err(p.err("unexpected text after the object"));
    }
    Ok(root)
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    /// `what` at the current line and column (bytes); never the text itself.
    fn err(&self, what: &str) -> String {
        let upto = &self.b[..self.pos.min(self.b.len())];
        let line = upto.iter().filter(|c| **c == b'\n').count() + 1;
        let col = upto.iter().rev().take_while(|c| **c != b'\n').count() + 1;
        format!("{what} at line {line}, column {col}")
    }

    /// Skip whitespace and, in JSONC, comments.
    fn ws(&mut self) -> Result<(), String> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.pos += 1,
                Some(b'/') if self.jsonc => match self.b.get(self.pos + 1) {
                    Some(b'/') => {
                        while self.peek().is_some_and(|c| c != b'\n') {
                            self.pos += 1;
                        }
                    }
                    Some(b'*') => {
                        let from = self.pos;
                        self.pos += 2;
                        loop {
                            match self.peek() {
                                None => {
                                    self.pos = from;
                                    return Err(self.err("a comment is not closed"));
                                }
                                Some(b'*') if self.b.get(self.pos + 1) == Some(&b'/') => {
                                    self.pos += 2;
                                    break;
                                }
                                Some(_) => self.pos += 1,
                            }
                        }
                    }
                    _ => return Err(self.err("unexpected '/'")),
                },
                _ => return Ok(()),
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<Node, String> {
        if depth > MAX_DEPTH {
            return Err(self.err("nested too deeply"));
        }
        let start = self.pos;
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => {
                self.string()?;
                Ok(Node { start, end: self.pos, kind: Kind::String })
            }
            Some(c) if c == b'-' || c.is_ascii_digit() || c.is_ascii_alphabetic() => {
                while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'+' | b'.')) {
                    self.pos += 1;
                }
                let lit = std::str::from_utf8(&self.b[start..self.pos]).unwrap_or("");
                if serde_json::from_str::<Value>(lit).is_err() {
                    self.pos = start;
                    return Err(self.err("a value is not valid JSON"));
                }
                Ok(Node { start, end: self.pos, kind: Kind::Scalar })
            }
            Some(_) => Err(self.err("expected a value")),
            None => Err(self.err("the file ends too early")),
        }
    }

    /// A string starting at `pos`; returns it decoded. Escapes are checked by their form only.
    fn string(&mut self) -> Result<String, String> {
        let start = self.pos;
        self.pos += 1;
        loop {
            match self.peek() {
                None | Some(b'\n') => {
                    self.pos = start;
                    return Err(self.err("a string is not closed"));
                }
                Some(c) if c < 0x20 => return Err(self.err("a string holds a control character")),
                Some(b'\\') => {
                    let ok = match self.b.get(self.pos + 1) {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                            self.pos += 2;
                            true
                        }
                        Some(b'u') => {
                            let hex = self.b.get(self.pos + 2..self.pos + 6).is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit));
                            self.pos += 6;
                            hex
                        }
                        _ => false,
                    };
                    if !ok {
                        return Err(self.err("a string has a bad escape"));
                    }
                }
                Some(b'"') => {
                    self.pos += 1;
                    break;
                }
                Some(_) => self.pos += 1,
            }
        }
        let body = std::str::from_utf8(&self.b[start + 1..self.pos - 1]).map_err(|_| self.err("a string is not UTF-8"))?;
        Ok(unescape(body))
    }

    fn object(&mut self, depth: usize) -> Result<Node, String> {
        let start = self.pos;
        self.pos += 1;
        let mut members: Vec<Member> = Vec::new();
        loop {
            self.ws()?;
            match self.peek() {
                Some(b'}') => {
                    if !self.jsonc && members.last().is_some_and(|m| m.comma.is_some()) {
                        return Err(self.err("a trailing comma"));
                    }
                    self.pos += 1;
                    return Ok(Node { start, end: self.pos, kind: Kind::Object(members) });
                }
                Some(b'"') => {
                    if members.last().is_some_and(|m| m.comma.is_none()) {
                        return Err(self.err("expected ',' between members"));
                    }
                    let key_start = self.pos;
                    let key = self.string()?;
                    self.ws()?;
                    if self.peek() != Some(b':') {
                        return Err(self.err("expected ':' after a key"));
                    }
                    self.pos += 1;
                    self.ws()?;
                    let value = self.value(depth + 1)?;
                    self.ws()?;
                    let comma = (self.peek() == Some(b',')).then_some(self.pos);
                    if comma.is_some() {
                        self.pos += 1;
                    }
                    members.push(Member { key, key_start, value, comma });
                }
                None => return Err(self.err("an object is not closed")),
                _ => return Err(self.err("expected a key or '}'")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Node, String> {
        let start = self.pos;
        self.pos += 1;
        let mut items = Vec::new();
        let mut comma = true;
        let mut trailing = false;
        loop {
            self.ws()?;
            match self.peek() {
                Some(b']') => {
                    if trailing && !self.jsonc {
                        return Err(self.err("a trailing comma"));
                    }
                    self.pos += 1;
                    return Ok(Node { start, end: self.pos, kind: Kind::Array(items) });
                }
                None => return Err(self.err("an array is not closed")),
                Some(b',') if !comma => {
                    self.pos += 1;
                    comma = true;
                    trailing = true;
                }
                Some(_) if comma => {
                    items.push(self.value(depth + 1)?);
                    comma = false;
                    trailing = false;
                }
                Some(_) => return Err(self.err("expected ',' or ']'")),
            }
        }
    }
}

// ---------------------------------------------------------------- editing

/// Line ending and indentation of a document.
#[derive(Debug, Clone)]
pub(crate) struct Style {
    pub nl: &'static str,
    /// One indentation step ("  ", "    " or "\t").
    pub unit: String,
    /// The whole document is on one line: write compact JSON.
    pub compact: bool,
}

impl Style {
    pub fn of(text: &str, root: &Node) -> Style {
        let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let compact = !text.trim_end().contains('\n');
        let unit = root
            .members()
            .and_then(|m| m.first())
            .map(|m| line_indent(text, m.key_start))
            .filter(|i| !i.is_empty())
            .unwrap_or_else(|| "  ".to_string());
        Style { nl, unit, compact }
    }
}

fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// The whitespace the line holding byte `at` starts with.
pub(crate) fn line_indent(text: &str, at: usize) -> String {
    text[line_start(text, at)..].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}

/// If only spaces, tabs and comments follow `pos` on its line: the position after that line's
/// newline. None when other text follows (or the file ends without a newline).
fn after_line(text: &str, pos: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = pos;
    loop {
        match b.get(i) {
            Some(b' ' | b'\t' | b'\r') => i += 1,
            Some(b'\n') => return Some(i + 1),
            Some(b'/') if b.get(i + 1) == Some(&b'/') => {
                return text[i..].find('\n').map(|n| i + n + 1);
            }
            Some(b'/') if b.get(i + 1) == Some(&b'*') => {
                let close = text[i + 2..].find("*/")? + i + 2;
                if text[i..close].contains('\n') {
                    return None;
                }
                i = close + 2;
            }
            _ => return None,
        }
    }
}

/// `value` as JSON text in this style, its later lines starting at `indent`.
pub(crate) fn render(value: &Value, style: &Style, indent: &str) -> String {
    if style.compact {
        return serde_json::to_string(value).expect("serializable");
    }
    let mut out = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(style.unit.as_bytes());
    let mut ser = serde_json::Serializer::with_formatter(&mut out, fmt);
    serde::Serialize::serialize(value, &mut ser).expect("serializable");
    let s = String::from_utf8(out).expect("utf-8");
    s.replace('\n', &format!("{}{indent}", style.nl))
}

/// Insertions at byte positions of the original text (applied from the end).
fn insert_all(text: &str, mut at: Vec<(usize, String)>) -> String {
    at.sort_by_key(|(p, _)| std::cmp::Reverse(*p));
    let mut s = text.to_string();
    for (p, t) in at {
        s.insert_str(p, &t);
    }
    s
}

/// Byte ranges of the original text removed (they must not overlap).
fn remove_all(text: &str, mut cuts: Vec<(usize, usize)>) -> String {
    cuts.sort_by_key(|(a, _)| std::cmp::Reverse(*a));
    let mut s = text.to_string();
    for (a, b) in cuts {
        s.replace_range(a..b, "");
    }
    s
}

/// Set member `key` of the object `obj` to `value`: its (last) value is replaced in place, or a
/// new member goes after the last one, in the object's own style (a trailing comma when the last
/// member has one; a comment at the end of the last member's line stays on that line). Every
/// other byte stays.
pub(crate) fn set_member(text: &str, obj: &Node, key: &str, value: &Value, style: &Style) -> String {
    let members = obj.members().expect("an object");
    if let Some(m) = members.iter().rev().find(|m| m.key == key) {
        let indent = line_indent(text, m.key_start);
        let one_line = style.compact || !text[obj.start..obj.end].contains('\n');
        let st = Style { compact: one_line, ..style.clone() };
        return insert_all(&remove_all(text, vec![(m.value.start, m.value.end)]), vec![(m.value.start, render(value, &st, &indent))]);
    }
    let key_json = serde_json::to_string(key).expect("serializable");
    let sep = if style.compact { ":" } else { ": " };
    let nl = style.nl;
    match members.last() {
        Some(last) => {
            let one_line = style.compact || !text[obj.start..obj.end].contains('\n');
            if one_line {
                let st = Style { compact: true, ..style.clone() };
                let lead = if style.compact { "" } else { " " };
                let member = format!(",{lead}{key_json}{sep}{}", render(value, &st, ""));
                return insert_all(text, vec![(last.value.end, member)]);
            }
            let indent = line_indent(text, members[0].key_start);
            let rendered = render(value, style, &indent);
            match last.comma {
                // Trailing-comma style: the new member gets one too.
                Some(c) => match after_line(text, c + 1) {
                    Some(p) => insert_all(text, vec![(p, format!("{indent}{key_json}{sep}{rendered},{nl}"))]),
                    None => insert_all(text, vec![(c + 1, format!("{nl}{indent}{key_json}{sep}{rendered},"))]),
                },
                None => match after_line(text, last.value.end) {
                    Some(p) => insert_all(text, vec![(last.value.end, ",".to_string()), (p, format!("{indent}{key_json}{sep}{rendered}{nl}"))]),
                    None => insert_all(text, vec![(last.value.end, format!(",{nl}{indent}{key_json}{sep}{rendered}"))]),
                },
            }
        }
        None => {
            if style.compact {
                let member = format!("{key_json}{sep}{}", render(value, style, ""));
                return insert_all(text, vec![(obj.start + 1, member)]);
            }
            let outer = line_indent(text, obj.start);
            let indent = format!("{outer}{}", style.unit);
            let rendered = render(value, style, &indent);
            let inner = &text[obj.start + 1..obj.end - 1];
            if inner.trim().is_empty() {
                let member = format!("{nl}{indent}{key_json}{sep}{rendered}{nl}{outer}");
                return insert_all(&remove_all(text, vec![(obj.start + 1, obj.end - 1)]), vec![(obj.start + 1, member)]);
            }
            // Only comments inside: the member goes after them, on its own line.
            let close = obj.end - 1;
            let ls = line_start(text, close);
            if text[ls..close].trim().is_empty() {
                insert_all(text, vec![(ls, format!("{indent}{key_json}{sep}{rendered}{nl}"))])
            } else {
                insert_all(text, vec![(close, format!("{nl}{indent}{key_json}{sep}{rendered}{nl}{outer}"))])
            }
        }
    }
}

/// Remove the member at `index` of `obj`. Only its own bytes go: from the start of its line (when
/// only whitespace comes before the key) through its comma and the rest of that line, or, for the
/// last member, through its value (and the rest of its line) plus the previous member's comma byte.
/// Comments above it and at the end of the previous member's line stay. An object left with only
/// whitespace inside becomes `{}`.
pub(crate) fn remove_member_at(text: &str, obj: &Node, index: usize) -> String {
    let members = obj.members().expect("an object");
    let m = &members[index];
    let ls = line_start(text, m.key_start);
    let own_line = text[ls..m.key_start].trim().is_empty();
    let start = if own_line { ls } else { m.key_start };
    let mut cuts = Vec::new();
    match m.comma {
        Some(c) => {
            let end = match after_line(text, c + 1) {
                Some(p) if own_line => p,
                _ => c + 1 + text[c + 1..].bytes().take_while(|b| *b == b' ').count(),
            };
            cuts.push((start, end));
        }
        None => {
            let end = match after_line(text, m.value.end) {
                Some(p) if own_line => p,
                _ => m.value.end,
            };
            cuts.push((start, end));
            if index > 0 {
                if let Some(pc) = members[index - 1].comma {
                    cuts.push((pc, pc + 1));
                }
            }
        }
    }
    let removed: usize = cuts.iter().map(|(a, b)| b - a).sum();
    let out = remove_all(text, cuts);
    let close = obj.end - removed;
    let inner = &out[obj.start + 1..close - 1];
    if members.len() == 1 && inner.trim().is_empty() && !inner.is_empty() {
        return remove_all(&out, vec![(obj.start + 1, close - 1)]);
    }
    out
}

