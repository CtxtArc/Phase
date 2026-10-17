//! Just enough XML for CMSIS-SVD files: elements, attributes, text,
//! comments, CDATA, the XML declaration and the five predefined entities
//! (plus numeric ones). No namespaces, DTDs or validation -- an SVD file
//! needs none of them, and the importer reads only what it knows.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    /// The element's own text, entities decoded (children's text not
    /// included).
    pub text: String,
}

impl Element {
    /// The first child named `name`.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Every child named `name`.
    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// The trimmed text of the first child named `name`.
    pub fn text_of(&self, name: &str) -> Option<&str> {
        self.child(name).map(|c| c.text.trim())
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Parse a document and return its root element.
pub fn parse(src: &str) -> Result<Element, String> {
    let mut p = Parser {
        s: src.as_bytes(),
        i: 0,
    };
    p.skip_misc()?;
    let root = p.element()?;
    p.skip_misc()?;
    if p.i < p.s.len() {
        return Err(p.error("content after the root element"));
    }
    Ok(root)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn error(&self, what: &str) -> String {
        let line = 1 + self.s[..self.i.min(self.s.len())]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        format!("XML line {line}: {what}")
    }

    fn starts(&self, pat: &str) -> bool {
        self.s[self.i..].starts_with(pat.as_bytes())
    }

    fn skip_ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn skip_past(&mut self, end: &str) -> Result<(), String> {
        match find(&self.s[self.i..], end.as_bytes()) {
            Some(n) => {
                self.i += n + end.len();
                Ok(())
            }
            None => Err(self.error(&format!("missing `{end}`"))),
        }
    }

    /// Whitespace, comments, `<?..?>` and `<!DOCTYPE ..>` between elements.
    fn skip_misc(&mut self) -> Result<(), String> {
        loop {
            self.skip_ws();
            if self.starts("<!--") {
                self.skip_past("-->")?;
            } else if self.starts("<?") {
                self.skip_past("?>")?;
            } else if self.starts("<!DOCTYPE") {
                self.skip_past(">")?;
            } else {
                return Ok(());
            }
        }
    }

    fn name(&mut self) -> Result<String, String> {
        let start = self.i;
        while self.i < self.s.len() {
            let b = self.s[self.i];
            if b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'-' | b'.') {
                self.i += 1;
            } else {
                break;
            }
        }
        if start == self.i {
            return Err(self.error("expected a name"));
        }
        Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    fn element(&mut self) -> Result<Element, String> {
        if !self.starts("<") {
            return Err(self.error("expected an element"));
        }
        self.i += 1;
        let mut el = Element {
            name: self.name()?,
            ..Element::default()
        };
        loop {
            self.skip_ws();
            if self.starts("/>") {
                self.i += 2;
                return Ok(el);
            }
            if self.starts(">") {
                self.i += 1;
                break;
            }
            let key = self.name()?;
            self.skip_ws();
            if !self.starts("=") {
                return Err(self.error(&format!("attribute '{key}' needs a value")));
            }
            self.i += 1;
            self.skip_ws();
            let quote = *self
                .s
                .get(self.i)
                .ok_or_else(|| self.error("end of file"))?;
            if quote != b'"' && quote != b'\'' {
                return Err(self.error("expected a quoted attribute value"));
            }
            self.i += 1;
            let start = self.i;
            while self.i < self.s.len() && self.s[self.i] != quote {
                self.i += 1;
            }
            let raw = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
            self.i += 1;
            el.attrs.push((key, decode(&raw)));
        }
        // Content: text, children, comments, CDATA, until `</name>`.
        loop {
            if self.i >= self.s.len() {
                return Err(self.error(&format!("<{}> is never closed", el.name)));
            }
            if self.starts("</") {
                self.i += 2;
                let end = self.name()?;
                if end != el.name {
                    return Err(self.error(&format!("</{end}> closes <{}>", el.name)));
                }
                self.skip_ws();
                if !self.starts(">") {
                    return Err(self.error("expected `>`"));
                }
                self.i += 1;
                return Ok(el);
            }
            if self.starts("<!--") {
                self.skip_past("-->")?;
            } else if self.starts("<![CDATA[") {
                self.i += 9;
                let n =
                    find(&self.s[self.i..], b"]]>").ok_or_else(|| self.error("missing `]]>`"))?;
                el.text
                    .push_str(&String::from_utf8_lossy(&self.s[self.i..self.i + n]));
                self.i += n + 3;
            } else if self.starts("<?") {
                self.skip_past("?>")?;
            } else if self.starts("<") {
                el.children.push(self.element()?);
            } else {
                let start = self.i;
                while self.i < self.s.len() && self.s[self.i] != b'<' {
                    self.i += 1;
                }
                el.text
                    .push_str(&decode(&String::from_utf8_lossy(&self.s[start..self.i])));
            }
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Decode `&lt;` `&gt;` `&amp;` `&quot;` `&apos;` and `&#N;` / `&#xN;`.
fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';') else {
            break;
        };
        let ent = &rest[1..end];
        let ch = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_elements_attributes_text_and_entities() {
        let doc = parse(
            "<?xml version=\"1.0\"?>\n<!-- c -->\n<device a='1' b=\"x&amp;y\">\n  <name>A &lt;B&gt;</name>\n  <!-- skip -->\n  <e/>\n  <d><![CDATA[<raw>]]></d>\n</device>\n",
        )
        .unwrap();
        assert_eq!(doc.name, "device");
        assert_eq!(doc.attr("b"), Some("x&y"));
        assert_eq!(doc.text_of("name"), Some("A <B>"));
        assert_eq!(doc.child("e").unwrap().children.len(), 0);
        assert_eq!(doc.text_of("d"), Some("<raw>"));
    }

    #[test]
    fn rejects_mismatched_tags() {
        assert!(parse("<a><b></a>").unwrap_err().contains("</a> closes <b>"));
        assert!(parse("<a>").unwrap_err().contains("never closed"));
    }
}
