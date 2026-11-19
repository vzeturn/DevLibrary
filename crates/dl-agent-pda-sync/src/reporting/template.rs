//! Serilog message templates (`Serilog.Parsing.MessageTemplateParser` and
//! `PropertyBinder`): text with `{Name}`, `{@Name}`, `{$Name}`, `{Name,10}`, `{Name:format}`
//! and positional `{0}` holes; `{{` and `}}` escape braces in text. A hole that does not
//! parse stays text, exactly as Serilog keeps it.

use super::log::Value;

/// How a hole asks for its value to be captured (`Destructuring`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destructuring {
    /// `{Name}`.
    Default,
    /// `{$Name}`: stringify.
    Stringify,
    /// `{@Name}`: destructure.
    Destructure,
}

/// A property hole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyToken {
    /// The property name (without the `@`/`$` hint).
    pub name: String,
    /// The hole as written, braces included.
    pub raw: String,
    /// The format after `:`, if any.
    pub format: Option<String>,
    /// The alignment after `,` (negative = left aligned), if any.
    pub alignment: Option<i32>,
    /// The capture hint.
    pub destructuring: Destructuring,
}

impl PropertyToken {
    /// `{0}`-style: the name is a non-negative integer.
    pub fn position(&self) -> Option<usize> {
        if self.name.bytes().all(|b| b.is_ascii_digit()) {
            self.name.parse().ok()
        } else {
            None
        }
    }
}

/// One piece of a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// Literal text (escapes already resolved).
    Text(String),
    /// A property hole.
    Property(PropertyToken),
}

/// A parsed message template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageTemplate {
    text: String,
    tokens: Vec<Token>,
}

/// `char.IsPunctuation`: Unicode categories Pc, Pd, Ps, Pe, Pi, Pf, Po. Exact for ASCII
/// and Latin-1; outside them the General Punctuation and CJK punctuation blocks are
/// covered, which is every punctuation the agent's templates can contain.
fn is_punctuation(c: char) -> bool {
    matches!(
        c,
        '!' | '"' | '#' | '%' | '&' | '\'' | '(' | ')' | '*' | ',' | '-' | '.' | '/' | ':'
            | ';' | '?' | '@' | '[' | '\\' | ']' | '_' | '{' | '}'
            | '\u{a1}' | '\u{a7}' | '\u{ab}' | '\u{b6}' | '\u{b7}' | '\u{bb}' | '\u{bf}'
            | '\u{2010}'..='\u{2027}'
            | '\u{2030}'..='\u{2043}'
            | '\u{2045}'..='\u{2051}'
            | '\u{2053}'..='\u{205e}'
            | '\u{3001}'..='\u{3003}'
            | '\u{3008}'..='\u{3011}'
    )
}

/// `char.IsLetterOrDigit`.
fn is_letter_or_digit(c: char) -> bool {
    c.is_alphanumeric()
}

fn valid_in_name(c: char) -> bool {
    is_letter_or_digit(c) || c == '_'
}

fn valid_in_format(c: char) -> bool {
    c != '}' && (is_letter_or_digit(c) || is_punctuation(c) || c == ' ' || c == '+')
}

fn valid_in_tag(c: char) -> bool {
    c == '@' || c == '$' || valid_in_name(c) || valid_in_format(c) || c == ':'
}

/// `TrySplitTagContent`: name (with hint), format, alignment.
fn split_tag(content: &str) -> Option<(&str, Option<&str>, Option<&str>)> {
    let fmt = content.find(':');
    let align = content.find(',');
    match (fmt, align) {
        (None, None) => Some((content, None, None)),
        (Some(f), a) if a.is_none() || a.is_some_and(|a| a > f) => {
            let format = (f != content.len() - 1).then(|| &content[f + 1..]);
            Some((&content[..f], format, None))
        }
        (f, Some(a)) => {
            let name = &content[..a];
            match f {
                None => {
                    if a == content.len() - 1 {
                        return None;
                    }
                    Some((name, None, Some(&content[a + 1..])))
                }
                Some(f) => {
                    if a + 1 == f {
                        return None;
                    }
                    let format = (f != content.len() - 1).then(|| &content[f + 1..]);
                    Some((name, format, Some(&content[a + 1..f])))
                }
            }
        }
        // (Some, None) is covered by the second arm.
        (Some(_), None) => None,
    }
}

fn property_token(raw: &str) -> Option<PropertyToken> {
    let content = &raw[1..raw.len() - 1];
    if content.is_empty() {
        return None;
    }
    let (name_hint, format, alignment) = split_tag(content)?;
    let (destructuring, name) = match name_hint.chars().next() {
        Some('@') => (Destructuring::Destructure, &name_hint[1..]),
        Some('$') => (Destructuring::Stringify, &name_hint[1..]),
        _ => (Destructuring::Default, name_hint),
    };
    if name.is_empty() || !name.chars().all(valid_in_name) {
        return None;
    }
    if let Some(f) = format
        && !f.chars().all(valid_in_format)
    {
        return None;
    }
    let alignment = match alignment {
        None => None,
        Some(a) => {
            if !a.chars().all(|c| c.is_ascii_digit() || c == '-') {
                return None;
            }
            let last_dash = a.rfind('-');
            if last_dash.is_some_and(|d| d > 0) {
                return None;
            }
            let digits = if last_dash.is_some() { &a[1..] } else { a };
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let width: i32 = digits.parse().ok()?;
            if width == 0 {
                return None;
            }
            Some(if last_dash.is_some() { -width } else { width })
        }
    };
    Some(PropertyToken {
        name: name.to_owned(),
        raw: raw.to_owned(),
        format: format.map(str::to_owned),
        alignment,
        destructuring,
    })
}

impl MessageTemplate {
    /// Parses `text` like Serilog's `MessageTemplateParser`.
    pub fn parse(text: &str) -> MessageTemplate {
        let mut tokens: Vec<Token> = Vec::new();
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let n = chars.len();
        let byte_at = |i: usize| if i < n { chars[i].0 } else { text.len() };
        let mut i = 0;
        while i < n {
            let starts_hole = chars[i].1 == '{' && !(i + 1 < n && chars[i + 1].1 == '{');
            if starts_hole {
                // ParsePropertyToken.
                let first = i;
                let mut j = i + 1;
                while j < n && valid_in_tag(chars[j].1) {
                    j += 1;
                }
                if j == n || chars[j].1 != '}' {
                    tokens.push(Token::Text(text[byte_at(first)..byte_at(j)].to_owned()));
                    i = j;
                    continue;
                }
                let raw = &text[byte_at(first)..byte_at(j + 1)];
                i = j + 1;
                match property_token(raw) {
                    Some(p) => tokens.push(Token::Property(p)),
                    None => tokens.push(Token::Text(raw.to_owned())),
                }
            } else {
                // ParseTextToken.
                let mut acc = String::new();
                while i < n {
                    let c = chars[i].1;
                    if c == '{' {
                        if i + 1 < n && chars[i + 1].1 == '{' {
                            acc.push('{');
                            i += 1;
                        } else {
                            break;
                        }
                    } else {
                        acc.push(c);
                        if c == '}' && i + 1 < n && chars[i + 1].1 == '}' {
                            i += 1;
                        }
                    }
                    i += 1;
                }
                tokens.push(Token::Text(acc));
            }
        }
        MessageTemplate {
            text: text.to_owned(),
            tokens,
        }
    }

    /// The template as written.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The tokens in order.
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    /// The property holes in order.
    pub fn properties(&self) -> impl Iterator<Item = &PropertyToken> {
        self.tokens.iter().filter_map(|t| match t {
            Token::Property(p) => Some(p),
            Token::Text(_) => None,
        })
    }

    /// `PropertyBinder`: when every hole is positional, argument `k` binds to the holes
    /// named `k`; otherwise argument `i` binds to the `i`-th hole and arguments beyond the
    /// holes become `__i`. Returns (name, value) in binding order; a repeated name appears
    /// more than once (the event keeps the last value at the first position).
    pub fn bind(&self, args: &[Value]) -> Vec<(String, Value)> {
        let holes: Vec<&PropertyToken> = self.properties().collect();
        if holes.is_empty() {
            return Vec::new();
        }
        if holes.iter().all(|p| p.position().is_some()) {
            // ConstructPositionalProperties: one slot per hole, indexed by position.
            let mut slots: Vec<Option<(String, Value)>> = vec![None; holes.len()];
            for p in &holes {
                if let Some(pos) = p.position()
                    && let Some(v) = args.get(pos)
                    && let Some(slot) = slots.get_mut(pos)
                {
                    *slot = Some((p.name.clone(), v.clone()));
                }
            }
            return slots.into_iter().flatten().collect();
        }
        let matched = holes.len().min(args.len());
        let mut out = Vec::with_capacity(args.len());
        for (p, v) in holes.iter().zip(args.iter()).take(matched) {
            out.push((p.name.clone(), v.clone()));
        }
        for (i, v) in args.iter().enumerate().skip(matched) {
            out.push((format!("__{i}"), v.clone()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(t: &str) -> Vec<String> {
        MessageTemplate::parse(t)
            .tokens()
            .iter()
            .map(|t| match t {
                Token::Text(s) => format!("T({s})"),
                Token::Property(p) => format!(
                    "P({}{}{})",
                    p.name,
                    p.format
                        .as_deref()
                        .map(|f| format!(":{f}"))
                        .unwrap_or_default(),
                    p.alignment.map(|a| format!(",{a}")).unwrap_or_default()
                ),
            })
            .collect()
    }

    #[test]
    fn parses_like_serilog() {
        assert_eq!(kinds("a {B} c"), ["T(a )", "P(B)", "T( c)"]);
        assert_eq!(kinds("{{lit}} x}}y"), ["T({lit} x}y)"]);
        assert_eq!(kinds("{bad name} {G"), ["T({bad name})", "T( )", "T({G)"]);
        assert_eq!(
            kinds("{F:0.00}{Id,8}|{Id,-8}|{X,-8:P1}|"),
            [
                "P(F:0.00)",
                "P(Id,8)",
                "T(|)",
                "P(Id,-8)",
                "T(|)",
                "P(X:P1,-8)",
                "T(|)"
            ]
        );
        assert_eq!(
            kinds("{@Obj} {$S} {}"),
            ["P(Obj)", "T( )", "P(S)", "T( )", "T({})"]
        );
        assert_eq!(
            kinds("{A,} {A,:x} {A,0} {A,1-}"),
            [
                "T({A,})",
                "T( )",
                "T({A,:x})",
                "T( )",
                "T({A,0})",
                "T( )",
                "T({A,1-})"
            ]
        );
        assert_eq!(kinds("{A:}"), ["P(A)"]);
        assert_eq!(kinds("x{a{b}y"), ["T(x)", "T({a{b})", "T(y)"]);
        assert_eq!(
            kinds("Đồng bộ {StreamName} lỗi"),
            ["T(Đồng bộ )", "P(StreamName)", "T( lỗi)"]
        );
    }

    #[test]
    fn binds_like_serilog() {
        let t = MessageTemplate::parse("{A} {B}");
        let b = t.bind(&[1.into(), 2.into(), 3.into()]);
        let names: Vec<_> = b.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["A", "B", "__2"]);
        let t = MessageTemplate::parse("{1} {0} {1}");
        let b = t.bind(&["x".into(), "y".into()]);
        assert_eq!(
            b,
            [
                ("0".to_owned(), Value::from("x")),
                ("1".to_owned(), Value::from("y"))
            ]
        );
        assert!(MessageTemplate::parse("plain").bind(&[1.into()]).is_empty());
    }
}
