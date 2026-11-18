//! Signed SQL bundles (PROTOCOL.md §9.7): the procedures, functions, views and idempotent
//! scripts a product needs in a site database, signed with a release key so neither the hub
//! nor the network can push SQL of its own; plus the T-SQL scanning the installer relies on
//! (header rewrite for engines without `CREATE OR ALTER`, syntax newer than the site).

use dl_ed25519::{SIGNATURE_LEN, SigningKey};
use dl_encoding::base64::STANDARD as B64;
use dl_json::{Value, json};

use super::probe::{Name, Requirement};
use crate::Error;
use crate::manifest::TrustedKeys;
use crate::version::Version;

/// Most objects one bundle may carry.
pub const MAX_OBJECTS: usize = 500;

/// What an object entry creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `CREATE PROCEDURE`.
    Procedure,
    /// `CREATE FUNCTION`.
    Function,
    /// `CREATE VIEW`.
    View,
    /// An idempotent batch (`IF COL_LENGTH(…) IS NULL ALTER TABLE …`), run as written.
    Script,
}

impl Kind {
    fn parse(s: &str) -> Result<Kind, Error> {
        match s {
            "procedure" => Ok(Kind::Procedure),
            "function" => Ok(Kind::Function),
            "view" => Ok(Kind::View),
            "script" => Ok(Kind::Script),
            other => Err(Error::Manifest(format!(
                "bundle: unknown object kind {other:?}"
            ))),
        }
    }

    /// The JSON name.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Procedure => "procedure",
            Kind::Function => "function",
            Kind::View => "view",
            Kind::Script => "script",
        }
    }

    /// `level1type` of `sp_addextendedproperty`; `None` for scripts.
    pub fn level1(self) -> Option<&'static str> {
        match self {
            Kind::Procedure => Some("PROCEDURE"),
            Kind::Function => Some("FUNCTION"),
            Kind::View => Some("VIEW"),
            Kind::Script => None,
        }
    }
}

/// One entry of a bundle.
#[derive(Debug, Clone, PartialEq)]
pub struct Object {
    /// What it is.
    pub kind: Kind,
    /// Object name (`schema.name`); for a script, a label.
    pub name: Name,
    /// The batch, `CREATE [OR ALTER] <kind> <name> …` for objects.
    pub sql: String,
}

impl Object {
    /// SHA-256 of the SQL text, hex.
    pub fn sha256(&self) -> String {
        use dl_hash::{Digest, Sha256};
        dl_encoding::hex::encode(Sha256::digest(self.sql.as_bytes()))
    }
}

/// A signed set of SQL objects for one product version.
#[derive(Debug, Clone, PartialEq)]
pub struct Bundle {
    /// Product (`einvoice`, `pms`, …).
    pub product: String,
    /// Product version the bundle belongs to.
    pub version: Version,
    /// Publication time (Unix seconds).
    pub published_at: u64,
    /// Objects, installed in this order.
    pub objects: Vec<Object>,
    /// What must exist once installed (checked with the schema probe), raw JSON.
    pub requires: Value,
    /// Release key id.
    pub key_id: String,
    /// Ed25519 signature over [`Bundle::signing_bytes`].
    pub signature: [u8; SIGNATURE_LEN],
}

impl Bundle {
    fn body(&self) -> Value {
        let objects: Vec<Value> = self
            .objects
            .iter()
            .map(|o| json!({"kind": o.kind.as_str(), "name": o.name.to_string(), "sql": o.sql}))
            .collect();
        json!({
            "product": self.product,
            "version": self.version.to_string(),
            "published_at": self.published_at,
            "objects": objects,
            "requires": self.requires.clone(),
            "key_id": self.key_id,
        })
    }

    /// Compact JSON of every field but `signature`.
    pub fn signing_bytes(&self) -> Vec<u8> {
        dl_json::to_string(&self.body()).into_bytes()
    }

    /// Signs with a release key.
    pub fn sign(&mut self, key_id: &str, key: &SigningKey) {
        self.key_id = key_id.to_string();
        self.signature = key.sign(&self.signing_bytes());
    }

    /// Checks the signature against the agent's release keys.
    ///
    /// # Errors
    /// [`Error::Manifest`] for an unknown key or a bad signature.
    pub fn verify(&self, trusted: &TrustedKeys) -> Result<(), Error> {
        let key = trusted
            .get(&self.key_id)
            .ok_or_else(|| Error::Manifest(format!("bundle: unknown key {:?}", self.key_id)))?;
        key.verify(&self.signing_bytes(), &self.signature)
            .map_err(|_| Error::Manifest("bundle: bad signature".into()))
    }

    /// The bundle as JSON, signature included.
    pub fn to_json(&self) -> Value {
        let mut v = self.body();
        if let Some(o) = v.as_object_mut() {
            o.insert("signature", B64.encode(self.signature));
        }
        v
    }

    /// The parsed `requires`, if any.
    ///
    /// # Errors
    /// [`Error::Config`] when it is not a valid requirement.
    pub fn requirement(&self) -> Result<Option<Requirement>, Error> {
        match &self.requires {
            Value::Null => Ok(None),
            v => Requirement::from_json(v).map(Some),
        }
    }

    /// `product@version`, the value stamped on installed objects.
    pub fn stamp(&self) -> String {
        format!("{}@{}", self.product, self.version)
    }

    /// Parses the JSON form; an absent signature parses as all zero (fails [`Bundle::verify`]).
    ///
    /// # Errors
    /// [`Error::Manifest`] for a wrong shape, a bad object header or too many objects.
    pub fn from_json(v: &Value) -> Result<Bundle, Error> {
        let bad = |what: &str| Error::Manifest(format!("bundle: {what}"));
        let s = |k: &str| v.get(k).and_then(Value::as_str).ok_or_else(|| bad(k));
        let product = s("product")?.to_string();
        if product.is_empty() || product.len() > 64 {
            return Err(bad("product"));
        }
        let version: Version = s("version")?.parse()?;
        let list = v
            .get("objects")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("objects"))?;
        if list.is_empty() || list.len() > MAX_OBJECTS {
            return Err(bad(&format!("1 to {MAX_OBJECTS} objects")));
        }
        let mut objects = Vec::new();
        for o in list {
            let f = |k: &str| o.get(k).and_then(Value::as_str).ok_or_else(|| bad(k));
            let kind = Kind::parse(f("kind")?)?;
            let name = Name::parse(f("name")?).map_err(|e| bad(&e.to_string()))?;
            let obj = Object {
                kind,
                name,
                sql: f("sql")?.to_string(),
            };
            if kind != Kind::Script {
                header(&obj)?; // refuse an entry whose SQL creates something else
            }
            objects.push(obj);
        }
        let signature = match v.get("signature").and_then(Value::as_str) {
            None => [0; SIGNATURE_LEN],
            Some(b) => B64
                .decode(b)
                .ok()
                .and_then(|raw| raw.try_into().ok())
                .ok_or_else(|| bad("signature"))?,
        };
        let bundle = Bundle {
            product,
            version,
            published_at: v.get("published_at").and_then(Value::as_u64).unwrap_or(0),
            objects,
            requires: v.get("requires").cloned().unwrap_or(Value::Null),
            key_id: v
                .get("key_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            signature,
        };
        bundle
            .requirement()
            .map_err(|e| bad(&format!("requires: {e}")))?;
        Ok(bundle)
    }
}

/// A T-SQL token outside comments and string literals.
#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// Bare word, upper-cased.
    Word(String),
    /// `[bracketed]` or `"quoted"` identifier, as written.
    Ident(String),
    /// One punctuation character.
    Punct(char),
}

/// Tokens with their byte span.
fn tokens(sql: &str) -> Vec<(Tok, usize, usize)> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if b[i..].starts_with(b"--") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            let mut depth = 0;
            while i < b.len() {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if c == b'\'' || ((c == b'N' || c == b'n') && b.get(i + 1) == Some(&b'\'')) {
            i += if c == b'\'' { 1 } else { 2 };
            while i < b.len() {
                if b[i] == b'\'' {
                    if b.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
        } else if c == b'[' || c == b'"' {
            let close = if c == b'[' { b']' } else { b'"' };
            let start = i;
            let mut s = Vec::new();
            i += 1;
            while i < b.len() {
                if b[i] == close {
                    if b.get(i + 1) == Some(&close) {
                        s.push(close);
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                s.push(b[i]);
                i += 1;
            }
            out.push((
                Tok::Ident(String::from_utf8_lossy(&s).into_owned()),
                start,
                i,
            ));
        } else if c.is_ascii_alphanumeric() || c == b'_' || c == b'@' || c == b'#' || c >= 0x80 {
            let start = i;
            while i < b.len()
                && (b[i].is_ascii_alphanumeric()
                    || matches!(b[i], b'_' | b'@' | b'#' | b'$')
                    || b[i] >= 0x80)
            {
                i += 1;
            }
            out.push((Tok::Word(sql[start..i].to_ascii_uppercase()), start, i));
        } else {
            out.push((Tok::Punct(c as char), i, i + 1));
            i += 1;
        }
    }
    out
}

fn ident(t: &Tok) -> Option<&str> {
    match t {
        Tok::Word(w) => Some(w),
        Tok::Ident(s) => Some(s),
        Tok::Punct(_) => None,
    }
}

/// Where the `CREATE [OR ALTER] | ALTER` verb of an object's batch is, checked against the
/// entry's kind and name: `(start, end)` byte span of the verb.
fn header(o: &Object) -> Result<(usize, usize), Error> {
    let bad = |m: String| Error::Manifest(format!("bundle: {}: {m}", o.name));
    let t = tokens(&o.sql);
    let word = |i: usize| match t.get(i) {
        Some((Tok::Word(w), _, _)) => w.as_str(),
        _ => "",
    };
    let (verb_end, at) = match (word(0), word(1), word(2)) {
        ("CREATE", "OR", "ALTER") => (t[2].2, 3),
        ("CREATE" | "ALTER", _, _) => (t[0].2, 1),
        _ => return Err(bad("must start with CREATE or ALTER".into())),
    };
    let kind_ok = match word(at) {
        "PROC" | "PROCEDURE" => o.kind == Kind::Procedure,
        "FUNCTION" => o.kind == Kind::Function,
        "VIEW" => o.kind == Kind::View,
        _ => false,
    };
    if !kind_ok {
        return Err(bad(format!("does not create a {}", o.kind.as_str())));
    }
    // `name` or `schema . name`
    let parts: Vec<&str> = match (t.get(at + 1), t.get(at + 2), t.get(at + 3)) {
        (Some((a, ..)), Some((Tok::Punct('.'), ..)), Some((b, ..))) => {
            vec![ident(a).unwrap_or(""), ident(b).unwrap_or("")]
        }
        (Some((a, ..)), ..) => vec![ident(a).unwrap_or("")],
        _ => Vec::new(),
    };
    let named = Name::parse(&parts.join(".")).ok();
    if named.as_ref().map(|n| n.to_string().to_lowercase())
        != Some(o.name.to_string().to_lowercase())
    {
        return Err(bad(format!(
            "creates {:?}, not the entry's name",
            parts.join(".")
        )));
    }
    Ok((t[0].1, verb_end))
}

/// The batch with its verb set to `CREATE` (object missing) or `ALTER` (object present):
/// what SQL Server before 2016 SP1 needs in place of `CREATE OR ALTER`. `ALTER` keeps the
/// object's grants and extended properties.
///
/// # Errors
/// [`Error::Manifest`] for a script or a batch whose header does not match the entry.
pub fn with_verb(o: &Object, exists: bool) -> Result<String, Error> {
    if o.kind == Kind::Script {
        return Err(Error::Manifest("bundle: a script has no verb".into()));
    }
    let (start, end) = header(o)?;
    let verb = if exists { "ALTER" } else { "CREATE" };
    Ok(format!("{}{verb}{}", &o.sql[..start], &o.sql[end..]))
}

/// T-SQL features newer than SQL Server 2008, with the level they need: `min(engine major ×
/// 10, compatibility level)` must reach it (conservative for engine-only features).
const FEATURES: [(&str, u64); 33] = [
    ("IIF", 110),
    ("TRY_CONVERT", 110),
    ("TRY_CAST", 110),
    ("TRY_PARSE", 110),
    ("PARSE", 110),
    ("CONCAT", 110),
    ("FORMAT", 110),
    ("CHOOSE", 110),
    ("EOMONTH", 110),
    ("DATEFROMPARTS", 110),
    ("DATETIMEFROMPARTS", 110),
    ("DATETIME2FROMPARTS", 110),
    ("TIMEFROMPARTS", 110),
    ("LAG", 110),
    ("LEAD", 110),
    ("FIRST_VALUE", 110),
    ("LAST_VALUE", 110),
    ("STRING_SPLIT", 130),
    ("OPENJSON", 130),
    ("JSON_VALUE", 130),
    ("JSON_QUERY", 130),
    ("ISJSON", 130),
    ("STRING_AGG", 140),
    ("CONCAT_WS", 140),
    ("TRIM", 140),
    ("TRANSLATE", 140),
    ("GREATEST", 160),
    ("LEAST", 160),
    ("DATETRUNC", 160),
    // statements, matched below
    ("THROW", 110),
    ("OFFSET FETCH", 110),
    ("SEQUENCE", 110),
    ("DROP IF EXISTS", 130),
];

/// Features of `sql` (outside comments and strings) that need more than `level`
/// (`min(engine major × 10, compatibility level)`), each with the level it needs.
/// `CREATE OR ALTER` in an object header is not listed: the installer rewrites it.
pub fn too_new(sql: &str, level: u64) -> Vec<(&'static str, u64)> {
    let t = tokens(sql);
    let word = |i: usize| match t.get(i) {
        Some((Tok::Word(w), ..)) => w.as_str(),
        _ => "",
    };
    let mut found: Vec<(&'static str, u64)> = Vec::new();
    let mut add = |name: &str| {
        if let Some(f) = FEATURES.iter().find(|(n, _)| *n == name)
            && f.1 > level
            && !found.contains(f)
        {
            found.push(*f);
        }
    };
    for i in 0..t.len() {
        let w = word(i);
        if w.is_empty() {
            continue;
        }
        let after_dot = i > 0 && t[i - 1].0 == Tok::Punct('.');
        let call = t.get(i + 1).map(|x| &x.0) == Some(&Tok::Punct('('));
        if call && !after_dot && FEATURES[..29].iter().any(|(n, _)| *n == w) {
            add(w);
        }
        match w {
            "THROW" => add("THROW"),
            "OFFSET" if (i + 1..i + 5).any(|j| matches!(word(j), "ROW" | "ROWS")) => {
                add("OFFSET FETCH")
            }
            "SEQUENCE" if matches!(word(i.wrapping_sub(1)), "CREATE" | "ALTER" | "DROP") => {
                add("SEQUENCE")
            }
            "NEXT" if word(i + 1) == "VALUE" && word(i + 2) == "FOR" => add("SEQUENCE"),
            "DROP" if word(i + 2) == "IF" && word(i + 3) == "EXISTS" => add("DROP IF EXISTS"),
            _ => {}
        }
    }
    found
}

/// Features of an object's batch newer than `level`. The `CREATE OR ALTER` of an object's
/// header is allowed (the installer rewrites it); any other one needs SQL Server 2016 SP1.
pub fn object_too_new(o: &Object, level: u64) -> Vec<(&'static str, u64)> {
    let mut f = too_new(&o.sql, level);
    let t = tokens(&o.sql);
    let word = |i: usize| match t.get(i) {
        Some((Tok::Word(w), ..)) => w.as_str(),
        _ => "",
    };
    let skip_header = o.kind != Kind::Script;
    let other = (0..t.len()).any(|i| {
        word(i) == "CREATE"
            && word(i + 1) == "OR"
            && word(i + 2) == "ALTER"
            && !(skip_header && i == 0)
    });
    if other && level < 130 {
        f.push(("CREATE OR ALTER", 130));
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(kind: Kind, name: &str, sql: &str) -> Object {
        Object {
            kind,
            name: Name::parse(name).unwrap(),
            sql: sql.into(),
        }
    }

    pub fn sample() -> Value {
        json!({
            "product": "einvoice", "version": "1.4.0", "published_at": 1_763_500_000u64,
            "objects": [
                {"kind": "script", "name": "dbo.add_MaCQT",
                 "sql": "IF COL_LENGTH('dbo.HoaDon', 'MaCQT') IS NULL ALTER TABLE dbo.HoaDon ADD MaCQT varchar(50) NULL"},
                {"kind": "procedure", "name": "dbo.usp_GetBill",
                 "sql": "-- header comment\nCREATE OR ALTER PROCEDURE [dbo].[usp_GetBill] @Id int AS SELECT 1 AS x"}
            ],
            "requires": {"procedures": ["dbo.usp_GetBill"], "tables": {"dbo.HoaDon": ["MaCQT"]}}
        })
    }

    #[test]
    fn signs_verifies_and_round_trips() {
        let key = SigningKey::from_seed(&[7u8; 32]);
        let mut b = Bundle::from_json(&sample()).unwrap();
        let trusted = TrustedKeys::new()
            .with_key("rel", &B64.encode(key.verifying_key().as_bytes()))
            .unwrap();
        assert!(b.verify(&trusted).is_err(), "unsigned");
        b.sign("rel", &key);
        b.verify(&trusted).unwrap();
        let back = Bundle::from_json(&b.to_json()).unwrap();
        back.verify(&trusted).unwrap();
        assert_eq!(back.stamp(), "einvoice@1.4.0");
        let mut tampered = back.clone();
        tampered.objects[1].sql.push_str(" UNION SELECT 2");
        assert!(tampered.verify(&trusted).is_err());
    }

    #[test]
    fn refuses_an_entry_that_creates_something_else() {
        let with = |kind: &str, sql: &str| {
            let mut v = sample();
            let o = v.get_mut("objects").and_then(Value::as_array_mut).unwrap()[1]
                .as_object_mut()
                .unwrap();
            o.insert("kind", kind);
            o.insert("sql", sql);
            Bundle::from_json(&v)
        };
        assert!(with("procedure", "CREATE PROCEDURE dbo.usp_GetBill AS SELECT 1").is_ok());
        assert!(with("procedure", "CREATE PROCEDURE dbo.usp_Other AS SELECT 1").is_err());
        assert!(with("procedure", "CREATE VIEW dbo.usp_GetBill AS SELECT 1 AS x").is_err());
        assert!(with("procedure", "SELECT 1").is_err());
        assert!(
            with(
                "trigger",
                "CREATE TRIGGER dbo.usp_GetBill ON t AFTER INSERT AS SELECT 1"
            )
            .is_err()
        );
    }

    #[test]
    fn rewrites_the_verb_for_old_engines() {
        let o = obj(
            Kind::Procedure,
            "dbo.usp_GetBill",
            "/* c */ create or alter proc dbo.usp_GetBill AS SELECT 'CREATE OR ALTER'",
        );
        assert_eq!(
            with_verb(&o, false).unwrap(),
            "/* c */ CREATE proc dbo.usp_GetBill AS SELECT 'CREATE OR ALTER'"
        );
        assert_eq!(
            with_verb(&o, true).unwrap(),
            "/* c */ ALTER proc dbo.usp_GetBill AS SELECT 'CREATE OR ALTER'"
        );
        let f = obj(
            Kind::Function,
            "fn_Tcvn3",
            "CREATE FUNCTION fn_Tcvn3(@s varchar(10)) RETURNS nvarchar(10) AS BEGIN RETURN @s END",
        );
        assert!(
            with_verb(&f, true)
                .unwrap()
                .starts_with("ALTER FUNCTION fn_Tcvn3(")
        );
    }

    #[test]
    fn flags_syntax_newer_than_the_site() {
        let sql = "CREATE PROCEDURE dbo.p AS BEGIN TRY \
                   SELECT IIF(1=1, TRY_CONVERT(int, '1'), 0), dbo.TRIM(x) FROM t \
                   ORDER BY 1 OFFSET 10 ROWS FETCH NEXT 5 ROWS ONLY; \
                   -- THROW in a comment is fine\n \
                   SELECT 'FORMAT(' + [CONCAT] FROM t \
                   END TRY BEGIN CATCH THROW; END CATCH";
        let names: Vec<&str> = too_new(sql, 100).iter().map(|f| f.0).collect();
        assert_eq!(names, ["IIF", "TRY_CONVERT", "OFFSET FETCH", "THROW"]);
        assert!(too_new(sql, 110).is_empty());
        let cursor = "FETCH NEXT FROM c INTO @x; DROP TABLE #t; SELECT NEXT_ID FROM t";
        assert!(too_new(cursor, 100).is_empty(), "2008 cursor syntax");
        let modern = "DROP TABLE IF EXISTS #t; SELECT STRING_AGG(x, ',') FROM t";
        assert_eq!(
            too_new(modern, 120),
            [("DROP IF EXISTS", 130), ("STRING_AGG", 140)]
        );
        assert!(too_new(modern, 140).is_empty());
        let script = obj(
            Kind::Script,
            "s",
            "CREATE OR ALTER VIEW dbo.v AS SELECT 1 AS x",
        );
        assert_eq!(object_too_new(&script, 100), [("CREATE OR ALTER", 130)]);
        let header = obj(
            Kind::View,
            "dbo.v",
            "CREATE OR ALTER VIEW dbo.v AS SELECT 1 AS x",
        );
        assert!(object_too_new(&header, 100).is_empty());
        assert!(object_too_new(&script, 130).is_empty());
    }
}
