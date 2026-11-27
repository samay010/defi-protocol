//! Minimal JSON writing (and a strict validator used by tests), std only.

/// A JSON string literal, quotes included. Besides the mandatory escapes, `<`, `>`, `&` and the
/// Unicode line separators are escaped too, so the text is inert even if it ever reached HTML.
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 || matches!(c, '<' | '>' | '&' | '\u{2028}' | '\u{2029}') => {
                o.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// A float as JSON; `null` for NaN and infinity (which JSON cannot hold).
pub fn float(v: Option<f64>) -> String {
    match v {
        Some(x) if x.is_finite() => format!("{x}"),
        _ => "null".to_string(),
    }
}

pub fn arr(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

/// An object under construction.
#[derive(Default)]
pub struct Obj {
    buf: String,
}

impl Obj {
    pub fn new() -> Obj {
        Obj::default()
    }

    fn key(&mut self, k: &str) {
        if !self.buf.is_empty() {
            self.buf.push(',');
        }
        self.buf.push_str(&esc(k));
        self.buf.push(':');
    }

    pub fn str(mut self, k: &str, v: &str) -> Obj {
        self.key(k);
        self.buf.push_str(&esc(v));
        self
    }

    pub fn num(mut self, k: &str, v: u64) -> Obj {
        self.key(k);
        self.buf.push_str(&v.to_string());
        self
    }

    pub fn bool(mut self, k: &str, v: bool) -> Obj {
        self.key(k);
        self.buf.push_str(if v { "true" } else { "false" });
        self
    }

    /// Already-valid JSON (an array, object, number or `null`).
    pub fn raw(mut self, k: &str, json: &str) -> Obj {
        self.key(k);
        self.buf.push_str(json);
        self
    }

    pub fn finish(self) -> String {
        format!("{{{}}}", self.buf)
    }
}

/// Strict JSON syntax check (RFC 8259 grammar, depth-limited). For tests and sanity checks.
pub fn is_valid(s: &str) -> bool {
    let b = s.as_bytes();
    let mut p = Parser { b, i: 0 };
    p.ws();
    p.value(0) && {
        p.ws();
        p.i == b.len()
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn lit(&mut self, w: &str) -> bool {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: u32) -> bool {
        if depth > 64 {
            return false;
        }
        match self.b.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                self.ws();
                if self.eat(b'}') {
                    return true;
                }
                loop {
                    self.ws();
                    if !self.string() {
                        return false;
                    }
                    self.ws();
                    if !self.eat(b':') {
                        return false;
                    }
                    self.ws();
                    if !self.value(depth + 1) {
                        return false;
                    }
                    self.ws();
                    if self.eat(b',') {
                        continue;
                    }
                    return self.eat(b'}');
                }
            }
            Some(b'[') => {
                self.i += 1;
                self.ws();
                if self.eat(b']') {
                    return true;
                }
                loop {
                    self.ws();
                    if !self.value(depth + 1) {
                        return false;
                    }
                    self.ws();
                    if self.eat(b',') {
                        continue;
                    }
                    return self.eat(b']');
                }
            }
            Some(b'"') => self.string(),
            Some(b't') => self.lit("true"),
            Some(b'f') => self.lit("false"),
            Some(b'n') => self.lit("null"),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => false,
        }
    }

    fn string(&mut self) -> bool {
        if !self.eat(b'"') {
            return false;
        }
        loop {
            match self.b.get(self.i) {
                None => return false,
                Some(b'"') => {
                    self.i += 1;
                    return true;
                }
                Some(b'\\') => {
                    self.i += 1;
                    match self.b.get(self.i) {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => self.i += 1,
                        Some(b'u') => {
                            self.i += 1;
                            for _ in 0..4 {
                                if !matches!(self.b.get(self.i), Some(c) if c.is_ascii_hexdigit()) {
                                    return false;
                                }
                                self.i += 1;
                            }
                        }
                        _ => return false,
                    }
                }
                Some(c) if *c < 0x20 => return false,
                Some(_) => self.i += 1,
            }
        }
    }

    fn number(&mut self) -> bool {
        self.eat(b'-');
        match self.b.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                    self.i += 1;
                }
            }
            _ => return false,
        }
        if self.eat(b'.') {
            let start = self.i;
            while matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                self.i += 1;
            }
            if self.i == start {
                return false;
            }
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            let start = self.i;
            while matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                self.i += 1;
            }
            if self.i == start {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_escaped_and_inert() {
        assert_eq!(esc("a\"b\\c\nd\te"), "\"a\\\"b\\\\c\\nd\\te\"");
        assert_eq!(esc("<script>&"), "\"\\u003cscript\\u003e\\u0026\"");
        assert_eq!(esc("\u{1}\u{1f}"), "\"\\u0001\\u001f\"");
        assert_eq!(esc("\u{2028}"), "\"\\u2028\"");
        assert_eq!(esc("héllo ✓"), "\"héllo ✓\"");
        for nasty in [
            "",
            "\"",
            "\\",
            "\n\r\t",
            "\u{0}",
            "</script>",
            "{\"a\":1}",
            "日本",
        ] {
            assert!(is_valid(&esc(nasty)), "{nasty:?}");
        }
    }

    #[test]
    fn objects_arrays_and_floats() {
        let o = Obj::new()
            .str("name", "x\"y")
            .num("n", 7)
            .bool("ok", true)
            .raw("list", &arr(&["1".into(), esc("two")]))
            .raw("hf", &float(Some(1.5)))
            .raw("none", &float(None))
            .raw("nan", &float(Some(f64::NAN)))
            .finish();
        assert!(is_valid(&o), "{o}");
        assert!(o.contains("\"n\":7") && o.contains("\"hf\":1.5") && o.contains("\"nan\":null"));
        assert_eq!(Obj::new().finish(), "{}");
        assert_eq!(arr(&[]), "[]");
    }

    #[test]
    fn the_validator_rejects_bad_json() {
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\"}",
            "{'a':1}",
            "01",
            "1.",
            "\"\\x\"",
            "[1 2]",
            "nul",
            "{\"a\":1,}",
            "\"a\nb\"",
            "[\"\\u12G4\"]",
            "{\"a\":1} x",
            "-",
            "1e",
        ] {
            assert!(!is_valid(bad), "{bad:?} must be invalid");
        }
        for good in [
            "{}",
            "[]",
            "0",
            "-1.5e+3",
            "\"\"",
            " { \"a\" : [ 1 , true , null ] } ",
            "[[[[]]]]",
        ] {
            assert!(is_valid(good), "{good:?} must be valid");
        }
        let deep = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert!(!is_valid(&deep), "depth limit");
    }
}
