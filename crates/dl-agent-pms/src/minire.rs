//! A small regular-expression engine for the patterns the agent evaluates: the
//! `excluded_proc_patterns` of the ops manifest and the migration scan rules. It covers
//! the RE2 syntax those use (Go's `regexp`): literals, `.`, classes `[...]` with ranges
//! and negation, `\w \W \d \D \s \S \b \B`, escaped metacharacters, groups `(...)` and
//! `(?:...)`, alternation, `* + ? {n} {n,} {n,m}` (optionally lazy), `^` and `$` (whole
//! text, no multi-line mode) and a leading `(?i)`.
//!
//! Matching backtracks. Repeats of a single character are scanned iteratively, so long
//! runs (whitespace, identifiers) cost no stack; the patterns used here have no nested
//! unbounded repeats, so matching stays linear in practice.

use std::fmt;

/// A pattern that does not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReError(pub String);

impl fmt::Display for ReError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReError {}

#[derive(Debug, Clone)]
enum ClassItem {
    Range(char, char),
    Word(bool),
    Digit(bool),
    Space(bool),
}

#[derive(Debug, Clone)]
enum Node {
    Char(char),
    Any,
    Class(Vec<ClassItem>, bool),
    Start,
    End,
    WordBoundary(bool),
    Group(Vec<Vec<Node>>),
    Repeat(Box<Node>, u32, Option<u32>, bool),
}

/// A compiled pattern.
#[derive(Debug, Clone)]
pub struct Regex {
    alts: Vec<Vec<Node>>,
    icase: bool,
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' ')
}

struct Parser<'a> {
    s: &'a [char],
    i: usize,
}

impl Parser<'_> {
    fn err<T>(&self, m: &str) -> Result<T, ReError> {
        Err(ReError(format!("{m} at offset {}", self.i)))
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn alts(&mut self) -> Result<Vec<Vec<Node>>, ReError> {
        let mut alts = vec![self.seq()?];
        while self.peek() == Some('|') {
            self.i += 1;
            alts.push(self.seq()?);
        }
        Ok(alts)
    }

    fn seq(&mut self) -> Result<Vec<Node>, ReError> {
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            let atom = self.quantifier(atom)?;
            out.push(atom);
        }
        Ok(out)
    }

    fn number(&mut self) -> Option<u32> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if start == self.i {
            return None;
        }
        self.s[start..self.i]
            .iter()
            .collect::<String>()
            .parse()
            .ok()
    }

    fn quantifier(&mut self, atom: Node) -> Result<Node, ReError> {
        let (min, max) = match self.peek() {
            Some('*') => (0, None),
            Some('+') => (1, None),
            Some('?') => (0, Some(1)),
            Some('{') => {
                let save = self.i;
                self.i += 1;
                let Some(lo) = self.number() else {
                    // Not a repeat: a literal brace, as in RE2.
                    self.i = save;
                    return Ok(atom);
                };
                let hi = if self.peek() == Some(',') {
                    self.i += 1;
                    if self.peek() == Some('}') {
                        None
                    } else {
                        match self.number() {
                            Some(h) => Some(h),
                            None => return self.err("bad repeat"),
                        }
                    }
                } else {
                    Some(lo)
                };
                if self.peek() != Some('}') {
                    return self.err("bad repeat");
                }
                if hi.is_some_and(|h| h < lo) || lo > 1000 || hi.is_some_and(|h| h > 1000) {
                    return self.err("bad repeat range");
                }
                (lo, hi)
            }
            _ => return Ok(atom),
        };
        self.i += 1;
        if matches!(atom, Node::Start | Node::End | Node::WordBoundary(_)) {
            return self.err("repeat of an assertion");
        }
        let greedy = if self.peek() == Some('?') {
            self.i += 1;
            false
        } else {
            true
        };
        if matches!(self.peek(), Some('*' | '+' | '?')) {
            return self.err("repeated repeat");
        }
        Ok(Node::Repeat(Box::new(atom), min, max, greedy))
    }

    fn escape(&mut self) -> Result<Result<char, ClassItem>, ReError> {
        let Some(c) = self.peek() else {
            return self.err("trailing backslash");
        };
        self.i += 1;
        Ok(match c {
            'w' => Err(ClassItem::Word(false)),
            'W' => Err(ClassItem::Word(true)),
            'd' => Err(ClassItem::Digit(false)),
            'D' => Err(ClassItem::Digit(true)),
            's' => Err(ClassItem::Space(false)),
            'S' => Err(ClassItem::Space(true)),
            'n' => Ok('\n'),
            'r' => Ok('\r'),
            't' => Ok('\t'),
            'f' => Ok('\x0c'),
            c if c.is_ascii_punctuation() => Ok(c),
            _ => return self.err("unsupported escape"),
        })
    }

    fn atom(&mut self) -> Result<Node, ReError> {
        let Some(c) = self.peek() else {
            return self.err("unexpected end");
        };
        self.i += 1;
        Ok(match c {
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '(' => {
                if self.s[self.i..].starts_with(&['?', ':']) {
                    self.i += 2;
                } else if self.peek() == Some('?') {
                    return self.err("unsupported group flag");
                }
                let alts = self.alts()?;
                if self.peek() != Some(')') {
                    return self.err("missing )");
                }
                self.i += 1;
                Node::Group(alts)
            }
            ')' => return self.err("unexpected )"),
            '[' => self.class()?,
            '*' | '+' | '?' => return self.err("missing argument to repeat"),
            '\\' => match self.peek() {
                Some('b') => {
                    self.i += 1;
                    Node::WordBoundary(true)
                }
                Some('B') => {
                    self.i += 1;
                    Node::WordBoundary(false)
                }
                _ => match self.escape()? {
                    Ok(c) => Node::Char(c),
                    Err(item) => Node::Class(vec![item], false),
                },
            },
            c => Node::Char(c),
        })
    }

    fn class(&mut self) -> Result<Node, ReError> {
        let neg = if self.peek() == Some('^') {
            self.i += 1;
            true
        } else {
            false
        };
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.peek() else {
                return self.err("missing ]");
            };
            if c == ']' && !first {
                self.i += 1;
                break;
            }
            first = false;
            self.i += 1;
            let lo = if c == '\\' {
                match self.escape()? {
                    Ok(c) => c,
                    Err(item) => {
                        items.push(item);
                        continue;
                    }
                }
            } else {
                c
            };
            if self.peek() == Some('-') && self.s.get(self.i + 1).is_some_and(|c| *c != ']') {
                self.i += 1;
                let Some(mut hi) = self.peek() else {
                    return self.err("missing ]");
                };
                self.i += 1;
                if hi == '\\' {
                    match self.escape()? {
                        Ok(c) => hi = c,
                        Err(_) => return self.err("bad class range"),
                    }
                }
                if hi < lo {
                    return self.err("bad class range");
                }
                items.push(ClassItem::Range(lo, hi));
            } else {
                items.push(ClassItem::Range(lo, lo));
            }
        }
        Ok(Node::Class(items, neg))
    }
}

impl Regex {
    /// Compiles `pattern`.
    pub fn new(pattern: &str) -> Result<Regex, ReError> {
        let (icase, body) = match pattern.strip_prefix("(?i)") {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        let chars: Vec<char> = body.chars().collect();
        let mut p = Parser { s: &chars, i: 0 };
        let alts = p.alts()?;
        if p.i != chars.len() {
            return p.err("unexpected )");
        }
        Ok(Regex { alts, icase })
    }

    /// Whether the pattern matches anywhere in `text`.
    pub fn is_match(&self, text: &str) -> bool {
        self.find_start(text).is_some()
    }

    /// The byte offset where the leftmost match starts.
    pub fn find_start(&self, text: &str) -> Option<usize> {
        let m = Matcher { re: self, t: text };
        let anchored = self
            .alts
            .iter()
            .all(|seq| matches!(seq.first(), Some(Node::Start)));
        if anchored {
            return m.alts(&self.alts, 0, &mut |_| true).then_some(0);
        }
        text.char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .find(|&s| m.alts(&self.alts, s, &mut |_| true))
    }
}

struct Matcher<'a> {
    re: &'a Regex,
    t: &'a str,
}

impl Matcher<'_> {
    fn char_at(&self, pos: usize) -> Option<char> {
        self.t[pos..].chars().next()
    }

    fn char_before(&self, pos: usize) -> Option<char> {
        self.t[..pos].chars().next_back()
    }

    fn eq(&self, a: char, b: char) -> bool {
        a == b || (self.re.icase && fold(a) == fold(b))
    }

    fn class_has(&self, items: &[ClassItem], c: char) -> bool {
        items.iter().any(|it| match *it {
            ClassItem::Range(lo, hi) => {
                (lo..=hi).contains(&c) || (self.re.icase && (fold_range(lo, hi, c)))
            }
            ClassItem::Word(neg) => is_word(c) != neg,
            ClassItem::Digit(neg) => c.is_ascii_digit() != neg,
            ClassItem::Space(neg) => is_space(c) != neg,
        })
    }

    /// Matches one character node at `pos`; the position after it.
    fn single(&self, n: &Node, pos: usize) -> Option<usize> {
        let c = self.char_at(pos)?;
        let ok = match n {
            Node::Char(x) => self.eq(*x, c),
            Node::Any => c != '\n',
            Node::Class(items, neg) => self.class_has(items, c) != *neg,
            _ => return None,
        };
        ok.then(|| pos + c.len_utf8())
    }

    fn alts(&self, alts: &[Vec<Node>], pos: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        alts.iter().any(|seq| self.seq(seq, pos, k))
    }

    fn seq(&self, seq: &[Node], pos: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        let Some((n, rest)) = seq.split_first() else {
            return k(pos);
        };
        match n {
            Node::Char(_) | Node::Any | Node::Class(..) => match self.single(n, pos) {
                Some(p) => self.seq(rest, p, k),
                None => false,
            },
            Node::Start => pos == 0 && self.seq(rest, pos, k),
            Node::End => pos == self.t.len() && self.seq(rest, pos, k),
            Node::WordBoundary(want) => {
                let a = self.char_before(pos).is_some_and(is_word);
                let b = self.char_at(pos).is_some_and(is_word);
                ((a != b) == *want) && self.seq(rest, pos, k)
            }
            Node::Group(alts) => self.alts(alts, pos, &mut |p| self.seq(rest, p, k)),
            Node::Repeat(inner, min, max, greedy) => {
                if matches!(**inner, Node::Char(_) | Node::Any | Node::Class(..)) {
                    // Collect the end positions of 0..=max repeats iteratively.
                    let mut ends = vec![pos];
                    let mut p = pos;
                    while max.is_none_or(|m| (ends.len() as u32) <= m) {
                        match self.single(inner, p) {
                            Some(q) => {
                                p = q;
                                ends.push(p);
                            }
                            None => break,
                        }
                    }
                    let lo = *min as usize;
                    if ends.len() <= lo {
                        return false;
                    }
                    let mut try_end = |e: usize| self.seq(rest, e, k);
                    if *greedy {
                        ends[lo..].iter().rev().any(|&e| try_end(e))
                    } else {
                        ends[lo..].iter().any(|&e| try_end(e))
                    }
                } else {
                    self.repeat(inner, *min, *max, *greedy, 0, pos, rest, k)
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn repeat(
        &self,
        inner: &Node,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        count: u32,
        pos: usize,
        rest: &[Node],
        k: &mut dyn FnMut(usize) -> bool,
    ) -> bool {
        let can_more = max.is_none_or(|m| count < m);
        let more = |this: &Self, k: &mut dyn FnMut(usize) -> bool| {
            can_more
                && this.seq(std::slice::from_ref(inner), pos, &mut |p| {
                    // An empty iteration would loop forever.
                    p != pos && this.repeat(inner, min, max, greedy, count + 1, p, rest, k)
                })
        };
        if count < min {
            return more(self, k);
        }
        // Greedy tries one more iteration first; lazy tries the rest of the pattern first.
        if greedy {
            if more(self, k) {
                return true;
            }
            return self.seq(rest, pos, k);
        }
        if self.seq(rest, pos, k) {
            return true;
        }
        more(self, k)
    }
}

fn fold(c: char) -> char {
    let mut l = c.to_lowercase();
    match (l.next(), l.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

fn fold_range(lo: char, hi: char, c: char) -> bool {
    let f = fold(c);
    (lo..=hi).contains(&f)
        || c.to_uppercase()
            .next()
            .is_some_and(|u| (lo..=hi).contains(&u))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, t: &str) -> bool {
        Regex::new(p).unwrap().is_match(t)
    }

    #[test]
    fn manifest_and_scan_patterns() {
        assert!(m(r"(?i)^xp_\w+$", concat!("XP_", "cmdshell")));
        assert!(!m(r"(?i)^xp_\w+$", "xp_"));
        assert!(!m(r"(?i)^xp_\w+$", "my_xp_cmd"));
        assert!(m(r"(?i)^SMLExecRsSql$", "smlexecrssql"));
        assert!(!m(r"(?i)^SMLExecRsSql$", "SMLExecRsSql2"));
        assert!(m(
            r"(?i)\b(BACKUP|RESTORE)\s+(DATABASE|LOG)\b",
            "x;\n  backup \t log [x]"
        ));
        assert!(!m(
            r"(?i)\b(BACKUP|RESTORE)\s+(DATABASE|LOG)\b",
            "MyBackup DATABASEX"
        ));
        assert!(m(
            r"(?i)(^|;|\n)\s*SHUTDOWN\b",
            "select 1;\n   shutdown with nowait"
        ));
        assert!(!m(r"(?i)(^|;|\n)\s*SHUTDOWN\b", "exec shutdown_x"));
        assert!(m(r"(?i)\bsp_configure\b|\bRECONFIGURE\b", "RECONFIGURE;"));
        assert!(m(
            r"(?i)\bOPEN(ROWSET|DATASOURCE)\b",
            "select * from openrowset("
        ));
        assert!(m(
            r"(?i)\b(CREATE|ALTER|DROP)\s+FULLTEXT\b",
            "CREATE  FULLTEXT INDEX"
        ));
        let re = Regex::new(r"(?i)\bxp_\w+").unwrap();
        assert_eq!(re.find_start(concat!("exec  xp_", "cmdshell 'x'")), Some(6));
    }

    #[test]
    fn syntax() {
        assert!(m("^a{2,3}$", "aaa"));
        assert!(!m("^a{2,3}$", "aaaa"));
        assert!(m("^[^0-9]+$", "abc"));
        assert!(m(r"^[A-Za-z_][A-Za-z0-9_]*$", "MM_Op_1"));
        assert!(m("^(?:ab)+$", "ababab"));
        assert!(m("^a.c$", "abc"));
        assert!(m(r"^\.$", "."));
        assert!(m("a{x", "a{x"));
        assert!(m("^(a|ab)c$", "abc"));
        assert!(m("^a*?b$", "aaab"));
        assert!(m(r"(?i)^[a-z]+$", "ABC"));
        for bad in ["(", "a)", "[a", "*a", r"\q", "a{3,1}", "(?P<x>a)"] {
            assert!(Regex::new(bad).is_err(), "{bad}");
        }
        let long = format!("{}SHUTDOWN", " ".repeat(200_000));
        assert!(m(r"(?i)(^|;|\n)\s*SHUTDOWN\b", &long));
    }
}
