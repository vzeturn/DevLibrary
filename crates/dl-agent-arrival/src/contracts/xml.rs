//! Well-formedness check for `Xml` parameters, standing in for .NET's `XmlReader` with
//! `DtdProcessing.Prohibit`: XML 1.0 with namespaces, one root element, the five
//! predefined entities and character references, no DOCTYPE. The procedure parses the
//! text again with SQL Server's own XML parser; this check only refuses what .NET refused.

/// `true` when `s` is a well-formed XML document.
pub fn is_well_formed(s: &str) -> bool {
    let mut p = Parser {
        s: s.as_bytes(),
        text: s,
        i: 0,
        scopes: Vec::new(),
    };
    p.document().is_some()
}

struct Parser<'a> {
    s: &'a [u8],
    text: &'a str,
    i: usize,
    /// Namespace prefixes declared per open element.
    scopes: Vec<Vec<String>>,
}

fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

fn is_name_start(c: char) -> bool {
    matches!(c, ':' | 'A'..='Z' | '_' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

fn is_name_char(c: char) -> bool {
    is_name_start(c)
        || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn starts(&self, lit: &str) -> bool {
        self.s[self.i..].starts_with(lit.as_bytes())
    }

    fn eat(&mut self, lit: &str) -> bool {
        let ok = self.starts(lit);
        if ok {
            self.i += lit.len();
        }
        ok
    }

    fn char_at(&self) -> Option<char> {
        self.text.get(self.i..)?.chars().next()
    }

    fn spaces(&mut self) -> bool {
        let start = self.i;
        while self.peek().is_some_and(is_space) {
            self.i += 1;
        }
        self.i > start
    }

    fn name(&mut self) -> Option<&'a str> {
        let start = self.i;
        let first = self.char_at().filter(|&c| is_name_start(c))?;
        self.i += first.len_utf8();
        while let Some(c) = self.char_at().filter(|&c| is_name_char(c)) {
            self.i += c.len_utf8();
        }
        self.text.get(start..self.i)
    }

    /// A QName: at most one colon, not at either end.
    fn qname(&mut self) -> Option<&'a str> {
        let n = self.name()?;
        let colons = n.matches(':').count();
        let ok = colons == 0 || (colons == 1 && !n.starts_with(':') && !n.ends_with(':'));
        ok.then_some(n)
    }

    fn document(&mut self) -> Option<()> {
        if self.starts("<?xml") && self.s.get(5).is_some_and(|&b| is_space(b)) {
            self.xml_decl()?;
        }
        self.misc()?;
        if self.starts("<!DOCTYPE") || self.peek() != Some(b'<') {
            return None;
        }
        self.element()?;
        self.misc()?;
        (self.i == self.s.len()).then_some(())
    }

    fn misc(&mut self) -> Option<()> {
        loop {
            self.spaces();
            if self.starts("<!--") {
                self.comment()?;
            } else if self.starts("<?") {
                self.pi()?;
            } else {
                return Some(());
            }
        }
    }

    fn quoted(&mut self) -> Option<&'a str> {
        let q = self.peek().filter(|&b| b == b'"' || b == b'\'')?;
        self.i += 1;
        let start = self.i;
        while self.peek()? != q {
            self.i += 1;
        }
        let v = self.text.get(start..self.i)?;
        self.i += 1;
        Some(v)
    }

    fn pseudo_attr(&mut self, name: &str) -> Option<&'a str> {
        let save = self.i;
        if !self.spaces() || !self.eat(name) {
            self.i = save;
            return None;
        }
        self.spaces();
        if !self.eat("=") {
            self.i = save;
            return None;
        }
        self.spaces();
        self.quoted()
    }

    fn xml_decl(&mut self) -> Option<()> {
        self.i += 5;
        let version = self.pseudo_attr("version")?;
        if version != "1.0" {
            return None;
        }
        if let Some(enc) = self.pseudo_attr("encoding") {
            let b = enc.as_bytes();
            let ok = b.first().is_some_and(u8::is_ascii_alphabetic)
                && b.iter()
                    .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'));
            if !ok {
                return None;
            }
        }
        if let Some(sa) = self.pseudo_attr("standalone")
            && sa != "yes"
            && sa != "no"
        {
            return None;
        }
        self.spaces();
        self.eat("?>").then_some(())
    }

    fn comment(&mut self) -> Option<()> {
        self.i += 4;
        let start = self.i;
        let end = start + self.text[start..].find("--")?;
        if !self.text[end..].starts_with("-->") {
            return None;
        }
        self.chars_ok(start, end)?;
        self.i = end + 3;
        Some(())
    }

    fn pi(&mut self) -> Option<()> {
        self.i += 2;
        let target = self.name()?;
        if target.eq_ignore_ascii_case("xml") || target.contains(':') {
            return None;
        }
        if self.eat("?>") {
            return Some(());
        }
        if !self.spaces() {
            return None;
        }
        let start = self.i;
        let end = start + self.text[start..].find("?>")?;
        self.chars_ok(start, end)?;
        self.i = end + 2;
        Some(())
    }

    fn chars_ok(&self, start: usize, end: usize) -> Option<()> {
        self.text
            .get(start..end)?
            .chars()
            .all(is_xml_char)
            .then_some(())
    }

    fn declared(&self, prefix: &str) -> bool {
        prefix == "xml" || self.scopes.iter().flatten().any(|p| p == prefix)
    }

    fn element(&mut self) -> Option<()> {
        self.i += 1;
        let name = self.qname()?;
        let mut attrs: Vec<&str> = Vec::new();
        let mut prefixes: Vec<String> = Vec::new();
        let empty = loop {
            let had_space = self.spaces();
            if self.eat("/>") {
                break true;
            }
            if self.eat(">") {
                break false;
            }
            if !had_space {
                return None;
            }
            let attr = self.qname()?;
            self.spaces();
            if !self.eat("=") {
                return None;
            }
            self.spaces();
            let value = self.quoted()?;
            self.attr_value(value)?;
            if attrs.contains(&attr) {
                return None;
            }
            if let Some(p) = attr.strip_prefix("xmlns:") {
                if value.is_empty() {
                    return None;
                }
                prefixes.push(p.to_string());
            }
            attrs.push(attr);
        };
        self.scopes.push(prefixes);
        let prefixed = |n: &str| n.split_once(':').map(|(p, _)| p.to_string());
        if let Some(p) = prefixed(name)
            && !self.declared(&p)
        {
            return None;
        }
        for a in &attrs {
            if let Some(p) = prefixed(a)
                && p != "xmlns"
                && !self.declared(&p)
            {
                return None;
            }
        }
        if !empty {
            self.content()?;
            if !self.eat("</") || self.qname()? != name {
                return None;
            }
            self.spaces();
            if !self.eat(">") {
                return None;
            }
        }
        self.scopes.pop();
        Some(())
    }

    fn attr_value(&self, v: &str) -> Option<()> {
        if v.contains('<') {
            return None;
        }
        let mut rest = v;
        while let Some(pos) = rest.find('&') {
            if !rest[..pos].chars().all(is_xml_char) {
                return None;
            }
            let end = rest[pos..].find(';')? + pos;
            reference(&rest[pos + 1..end])?;
            rest = &rest[end + 1..];
        }
        rest.chars().all(is_xml_char).then_some(())
    }

    fn content(&mut self) -> Option<()> {
        loop {
            match self.peek()? {
                b'<' => {
                    if self.starts("</") {
                        return Some(());
                    } else if self.starts("<!--") {
                        self.comment()?;
                    } else if self.starts("<![CDATA[") {
                        self.i += 9;
                        let start = self.i;
                        let end = start + self.text[start..].find("]]>")?;
                        self.chars_ok(start, end)?;
                        self.i = end + 3;
                    } else if self.starts("<?") {
                        self.pi()?;
                    } else if self.starts("<!") {
                        return None;
                    } else {
                        self.element()?;
                    }
                }
                b'&' => {
                    let end = self.i + self.text[self.i..].find(';')?;
                    reference(&self.text[self.i + 1..end])?;
                    self.i = end + 1;
                }
                _ => {
                    if self.starts("]]>") {
                        return None;
                    }
                    let c = self.char_at()?;
                    if !is_xml_char(c) {
                        return None;
                    }
                    self.i += c.len_utf8();
                }
            }
        }
    }
}

/// The text between `&` and `;`: a predefined entity or a character reference to a legal
/// character.
fn reference(r: &str) -> Option<()> {
    let code = if let Some(hex) = r.strip_prefix("#x") {
        (!hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| u32::from_str_radix(hex, 16).ok())??
    } else if let Some(dec) = r.strip_prefix('#') {
        (!dec.is_empty() && dec.bytes().all(|b| b.is_ascii_digit()))
            .then(|| dec.parse::<u32>().ok())??
    } else {
        return matches!(r, "amp" | "lt" | "gt" | "quot" | "apos").then_some(());
    };
    char::from_u32(code).filter(|&c| is_xml_char(c)).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_guest_details() {
        assert!(is_well_formed(
            "<?xml version='1.0'?>\n<Guest><Name>Nguyễn</Name></Guest>"
        ));
        assert!(!is_well_formed("<a>&nbsp;</a>"));
        assert!(!is_well_formed("<!DOCTYPE a [<!ENTITY x 'y'>]><a>&x;</a>"));
    }
}
