//! The prompted enroll for an agent on a different machine than SQL Server
//! (`enroll_prompt.go`, flag `--sql-prompt`).
//!
//! Prompt texts are Vietnamese without diacritics on purpose: the installer runs in a
//! legacy Windows console whose code page cannot be relied on to render them.

use std::io::{BufRead, Write};

use super::ping::{Ping, PingError, ping_negotiated};
use super::{DEFAULT_SQL_HOST, DEFAULT_SQL_PORT};
use crate::dsn::{Dsn, Transport};

/// What the operator typed (`sqlAnswers`); kept across a failed test so the next round
/// offers every previous answer as the default.
#[derive(Debug, Clone, Default)]
struct Answers {
    host: String,
    port_or_inst: String,
    user: String,
    password: String,
    fo_db: String,
    pos_db: String,
    /// Negotiated by the last test; reset each round.
    transport: Transport,
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

impl Answers {
    /// A numeric `port_or_inst` is a TCP port, anything else a named instance.
    fn dsn(&self, database: &str) -> Dsn {
        let mut d = Dsn {
            host: self.host.clone(),
            database: database.to_owned(),
            user: self.user.clone(),
            password: self.password.clone(),
            ..Dsn::default()
        }
        .with_transport(&self.transport);
        if is_digits(&self.port_or_inst) {
            d.port = self.port_or_inst.clone();
        } else {
            d.instance = self.port_or_inst.clone();
        }
        d
    }
}

/// `splitServerAddress`: the forms operators copy out of SSMS (`host\instance`,
/// `host,port`, `host:port`) split into the bare host and the port or instance.
pub(super) fn split_server_address(s: &str) -> (String, String) {
    for sep in ['\\', ',', ':'] {
        if let Some(i) = s.find(sep).filter(|&i| i > 0) {
            return (s[..i].trim().to_owned(), s[i + 1..].trim().to_owned());
        }
    }
    (s.to_owned(), String::new())
}

/// Turns console echo off and returns the function that turns it back on.
pub type HideInput<'a> = &'a dyn Fn() -> Box<dyn FnOnce()>;

pub(super) struct Prompter<'a, 'b> {
    pub(super) input: &'a mut dyn BufRead,
    pub(super) out: &'a mut dyn Write,
    pub(super) hide: Option<HideInput<'b>>,
}

impl Prompter<'_, '_> {
    /// One trimmed line. A closed input is an error rather than an empty answer, so a
    /// non-interactive run fails instead of looping on re-prompts.
    fn line(&mut self) -> Result<String, String> {
        let mut s = String::new();
        match self.input.read_line(&mut s) {
            Ok(0) => Err("agent: read answer: EOF".to_owned()),
            Ok(_) => Ok(s.trim().to_owned()),
            Err(e) if s.is_empty() => Err(format!("agent: read answer: {e}")),
            Err(_) => Ok(s.trim().to_owned()),
        }
    }

    fn ask(&mut self, label: &str, def: &str) -> Result<String, String> {
        if def.is_empty() {
            say!(self.out, "  {label}: ");
        } else {
            say!(self.out, "  {label} [{def}]: ");
        }
        let s = self.line()?;
        Ok(if s.is_empty() { def.to_owned() } else { s })
    }

    fn ask_required(&mut self, label: &str, def: &str) -> Result<String, String> {
        loop {
            let s = self.ask(label, def)?;
            if !s.is_empty() {
                return Ok(s);
            }
            say!(self.out, "  (bat buoc nhap)\n");
        }
    }

    /// A password with console echo off; an empty answer keeps `current`, which is never
    /// printed.
    fn ask_secret(&mut self, label: &str, current: &str) -> Result<String, String> {
        say!(self.out, "  {label} (Enter = giu nguyen): ");
        let restore = self.hide.map(|h| h());
        let r = self.line();
        if let Some(restore) = restore {
            restore();
        }
        say!(self.out, "\n");
        let s = r?;
        Ok(if s.is_empty() { current.to_owned() } else { s })
    }

    /// A yes/no question where Enter means yes.
    fn confirm(&mut self, question: &str) -> Result<bool, String> {
        say!(self.out, "{question} [Y/n]: ");
        let s = self.line()?;
        Ok(s.is_empty() || s.eq_ignore_ascii_case("y") || s.eq_ignore_ascii_case("yes"))
    }

    fn collect(&mut self, mut a: Answers) -> Result<Answers, String> {
        a.host = self.ask_required("Dia chi may SQL Server (IP hoac ten may)", &a.host)?;
        let (host, rest) = split_server_address(&a.host);
        if !rest.is_empty() {
            a.host = host;
            a.port_or_inst = rest;
        }
        a.port_or_inst = self.ask_required("Cong TCP hoac ten instance", &a.port_or_inst)?;
        a.user = self.ask_required("Tai khoan SQL", &a.user)?;
        a.password = self.ask_secret("Mat khau SQL", &a.password)?;
        a.fo_db = self.ask_required("Database FO", &a.fo_db)?;
        a.pos_db = self.ask_required("Database POS", &a.pos_db)?;
        Ok(a)
    }
}

/// `promptSQL`: asks for each connection part, tests FO and POS (falling back on its own
/// to an older transport, see [`ping_negotiated`]), and on failure shows the error and
/// asks again with the previous answers as defaults. The operator can stop retrying and
/// install anyway; the service keeps retrying the connection.
pub(super) fn prompt_sql(
    p: &mut Prompter<'_, '_>,
    mut fo: Dsn,
    mut pos: Dsn,
    ping: Ping<'_>,
) -> Result<(Dsn, Dsn), String> {
    let mut a = Answers {
        host: fo.host.clone(),
        port_or_inst: DEFAULT_SQL_PORT.to_owned(),
        user: fo.user.clone(),
        password: fo.password.clone(),
        fo_db: fo.database.clone(),
        pos_db: pos.database.clone(),
        transport: Transport::default(),
    };
    if a.host == DEFAULT_SQL_HOST {
        a.host.clear(); // "this machine" is not a meaningful default for a remote server
    }
    say!(
        p.out,
        "Nhap thong tin ket noi SQL Server cua SMILE (Enter = giu gia tri trong ngoac).\n"
    );
    loop {
        a = p.collect(a)?;
        // Every round renegotiates: the answers may point at a different server.
        a.transport = Transport::default();
        let mut first_err: Option<PingError> = None;
        for (label, which) in [("FO", 0), ("POS", 1)] {
            let database = if which == 0 {
                a.fo_db.clone()
            } else {
                a.pos_db.clone()
            };
            say!(p.out, "  kiem tra ket noi {label} ({database}) ... ");
            // FO negotiates the transport; POS (same server) starts from it.
            let (negotiated, r) = ping_negotiated(&a.dsn(&database), ping);
            a.transport = negotiated.transport();
            if which == 0 {
                fo = negotiated;
            } else {
                pos = negotiated;
            }
            match r {
                Err(e) => {
                    say!(p.out, "LOI: {e}\n");
                    first_err.get_or_insert(e);
                }
                Ok(()) => {
                    let note = a.transport.describe();
                    if note.is_empty() {
                        say!(p.out, "OK\n");
                    } else {
                        say!(p.out, "OK ({note})\n");
                    }
                }
            }
        }
        // Both databases live on one server: seal one transport for both.
        fo = fo.with_transport(&a.transport);
        if first_err.is_none() {
            say!(p.out, "\n");
            return Ok((fo, pos));
        }
        say!(
            p.out,
            "Khong ket noi duoc. Kiem tra: dia chi/cong, SQL Server da bat TCP/IP, firewall, tai khoan va mat khau, ten database.\n"
        );
        if !p.confirm("Nhap lai thong tin ket noi?")? {
            say!(
                p.out,
                "Tiep tuc cai dat voi thong tin hien tai; dich vu se tu thu ket noi lai.\n\n"
            );
            return Ok((fo, pos));
        }
    }
}
