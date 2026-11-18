//! The blocking client: connect, login, run batches and RPCs.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use dl_tls::{TlsStream, Verify};

use crate::Error;
use crate::config::{Config, Encrypt};
use crate::login::{Login, TDS_74, TDS_80};
use crate::packet::{self, MessageReader};
use crate::params::{self, Param};
use crate::prelogin::{self, ENCRYPT_NOT_SUP, ENCRYPT_OFF, ENCRYPT_ON};
use crate::results::{Message, Results};
use crate::sspi::{self, Negotiate};
use crate::tls::TdsTlsTransport;
use crate::token::{self, DONE, DONE_ATTN, LoginAck, Session};

/// How long to wait for the server to acknowledge an ATTENTION (go-mssqldb uses 5 s).
const CANCEL_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// Most SSPI round trips after LOGIN7 (NTLM needs 1, Kerberos 0 or 1).
const MAX_SSPI_ROUNDS: usize = 8;

type Tls = TlsStream<TdsTlsTransport<TcpStream>>;

enum Stream {
    Plain(TcpStream),
    Tls(Box<Tls>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            Stream::Tls(t) => t.get_ref().get_ref(),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(t) => t.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(t) => t.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(t) => t.flush(),
        }
    }
}

/// A connection to SQL Server.
///
/// All calls block. A `Client` is `Send` but not shareable; use one per thread or a pool.
/// Every request reads the complete response before returning (results are
/// materialised), so the connection is always ready for the next request.
///
/// ```no_run
/// use dl_tds::{Client, Config, Param};
///
/// let cfg: Config = "sqlserver://smile:pw@localhost:1433?database=SMILE_FO".parse()?;
/// let mut c = Client::connect(&cfg)?;
/// let rs = c.query("EXEC dbo.sp_FetchFO @ID = @ID", &[Param::new("ID", "123")])?;
/// for set in rs.sets() {
///     for row in set.rows() {
///         let s: String = row.get(0)?;
///         println!("{s}");
///     }
/// }
/// let n = c.execute("UPDATE t SET x = 1 WHERE id = @id", &[Param::new("id", 5i64)])?;
/// c.batch("CREATE OR ALTER PROCEDURE dbo.p AS SELECT 1")?;
/// c.ping()?;
/// # let _ = n;
/// # Ok::<(), dl_tds::Error>(())
/// ```
pub struct Client {
    stream: Stream,
    reader: MessageReader,
    packet_size: usize,
    session: Session,
    messages: Vec<Message>,
    broken: bool,
    timeout: Option<Duration>,
    login_ack: LoginAck,
    encrypted: bool,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("database", &self.session.database)
            .field("packet_size", &self.packet_size)
            .field("encrypted", &self.encrypted)
            .field("broken", &self.broken)
            .finish_non_exhaustive()
    }
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

fn client_version() -> (u8, u8, u8) {
    let p = |s: &str| s.parse::<u8>().unwrap_or(0);
    (
        p(env!("CARGO_PKG_VERSION_MAJOR")),
        p(env!("CARGO_PKG_VERSION_MINOR")),
        p(env!("CARGO_PKG_VERSION_PATCH")),
    )
}

/// Sends the PRELOGIN payload and parses the server's answer.
fn prelogin_exchange<S: Read + Write>(
    s: &mut S,
    reader: &mut MessageReader,
    payload: &[u8],
) -> Result<prelogin::Response, Error> {
    let mut framed = Vec::new();
    packet::frame(
        packet::PRELOGIN,
        payload,
        packet::DEFAULT_PACKET_SIZE,
        &mut framed,
    );
    s.write_all(&framed)?;
    s.flush()?;
    let (ptype, reply) = reader.read_message(s)?;
    if ptype != packet::REPLY {
        return Err(Error::Malformed {
            token: 0,
            offset: 0,
            what: "PRELOGIN response is not a REPLY packet",
        });
    }
    prelogin::decode(&reply)
}

/// Every failure of the TLS handshake, I/O included, is [`Error::Tls`]: a server that
/// cannot speak the offered versions often just closes or resets the connection, and
/// callers must be able to tell that from a failure before or after the handshake.
fn handshake_error(e: dl_tls::Error) -> Error {
    Error::Tls(e)
}

fn dial(cfg: &Config) -> Result<TcpStream, Error> {
    let host = cfg.host();
    let dial_host = if host == "."
        || host.eq_ignore_ascii_case("(local)")
        || host.eq_ignore_ascii_case("localhost")
    {
        "127.0.0.1"
    } else {
        host
    };
    let port = match (cfg.port().filter(|&p| p != 0), cfg.instance()) {
        (Some(p), _) => p,
        (None, Some(inst)) => crate::browser::resolve(dial_host, inst, cfg.connect_timeout())?,
        (None, None) => 1433,
    };
    let addrs: Vec<SocketAddr> = (dial_host, port).to_socket_addrs()?.collect();
    let mut last = io::Error::new(
        io::ErrorKind::NotFound,
        format!("no address for {dial_host}"),
    );
    for addr in addrs {
        let r = if cfg.connect_timeout().is_zero() {
            TcpStream::connect(addr)
        } else {
            TcpStream::connect_timeout(&addr, cfg.connect_timeout())
        };
        match r {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(Error::Io(last))
}

impl Client {
    /// Connects, negotiates encryption and logs in.
    ///
    /// # Errors
    /// [`Error::Io`] (network), [`Error::Tls`], [`Error::EncryptionNotSupported`],
    /// [`Error::Server`] (login failed, e.g. error 18456), [`Error::Browser`] (named
    /// instance not found), [`Error::Routing`], [`Error::Malformed`]. With
    /// [`Config::integrated_auth`]: [`Error::Sspi`] when Windows cannot authenticate, and
    /// [`Error::Unsupported`] outside Windows.
    pub fn connect(cfg: &Config) -> Result<Client, Error> {
        // Integrated authentication: get the user's credentials before touching the
        // network, so a platform without SSPI fails fast.
        let mut negotiate = if cfg.integrated_auth() {
            Some(Negotiate::acquire()?)
        } else {
            None
        };
        let tcp = dial(cfg)?;
        tcp.set_nodelay(true)?;
        // Bound the handshake even without a configured timeout.
        let login_timeout = cfg
            .timeout()
            .or_else(|| Some(cfg.connect_timeout()).filter(|d| !d.is_zero()));
        tcp.set_read_timeout(login_timeout)?;
        tcp.set_write_timeout(login_timeout)?;

        let tls_config = || {
            dl_tls::Config::new(cfg.host_name_in_certificate())
                .verify(if cfg.trust_server_certificate() {
                    Verify::None
                } else {
                    Verify::Full
                })
                .min_version(cfg.tls_min())
        };
        let strict = cfg.encrypt() == Encrypt::Strict;
        let mut reader = MessageReader::new();
        let (maj, min, patch) = client_version();
        let enc = match cfg.encrypt() {
            Encrypt::Disabled => ENCRYPT_NOT_SUP,
            Encrypt::Off => ENCRYPT_OFF,
            Encrypt::Required => ENCRYPT_ON,
            Encrypt::Strict => prelogin::ENCRYPT_STRICT,
        };
        let pl = prelogin::encode(
            enc,
            cfg.instance().unwrap_or(""),
            [maj, min, 0, patch, 0, 0],
        );

        let (stream, login_only) = if strict {
            // TDS 8: TLS directly on the socket, then PRELOGIN inside it.
            let t = TdsTlsTransport::new(tcp, false, packet::DEFAULT_PACKET_SIZE);
            let tls = TlsStream::connect(&tls_config().alpn(&[b"tds/8.0"]), t)
                .map_err(handshake_error)?;
            let mut s = Stream::Tls(Box::new(tls));
            prelogin_exchange(&mut s, &mut reader, &pl)?;
            (s, false)
        } else {
            let mut tcp = tcp;
            let server = prelogin_exchange(&mut tcp, &mut reader, &pl)?;
            if cfg.encrypt() == Encrypt::Required
                && matches!(server.encryption, ENCRYPT_NOT_SUP | ENCRYPT_OFF)
            {
                return Err(Error::EncryptionNotSupported);
            }
            // TLS handshake inside PRELOGIN packets (TDS 7.x), as go-mssqldb: whenever the
            // server did not answer NOT_SUP. Answer OFF means "encrypt the login only".
            if server.encryption != ENCRYPT_NOT_SUP {
                let t = TdsTlsTransport::new(tcp, true, packet::DEFAULT_PACKET_SIZE);
                let mut tls = TlsStream::connect(&tls_config(), t).map_err(handshake_error)?;
                tls.get_mut()
                    .finish_handshake()
                    .map_err(|e| handshake_error(dl_tls::Error::Io(e)))?;
                debug_assert!(!tls.get_ref().in_handshake());
                (Stream::Tls(Box::new(tls)), server.encryption == ENCRYPT_OFF)
            } else {
                (Stream::Plain(tcp), false)
            }
        };
        let encrypted = matches!(stream, Stream::Tls(_)) && !login_only;

        // LOGIN7.
        let server_name = match cfg.instance() {
            Some(i) => format!("{}\\{}", cfg.host(), i),
            None => cfg.host().to_string(),
        };
        let sspi_token = match negotiate.as_mut() {
            Some(n) => {
                let t = n.initialize(&sspi::target_name(cfg))?;
                if t.is_empty() {
                    return Err(Error::Unsupported(
                        "the SSPI package produced no initial token",
                    ));
                }
                t
            }
            None => Vec::new(),
        };
        let login = Login {
            tds_version: if strict { TDS_80 } else { TDS_74 },
            packet_size: cfg.packet_size() as u32,
            client_prog_ver: (maj as u32) << 24 | (min as u32) << 16 | patch as u32,
            client_pid: std::process::id(),
            read_only_intent: cfg.read_only_intent(),
            host_name: cfg.workstation().to_string(),
            user_name: cfg.user().to_string(),
            password: cfg.password().to_string(),
            app_name: cfg.app_name().to_string(),
            server_name,
            library_name: "dl-tds".into(),
            language: cfg.language.clone(),
            database: cfg.database().to_string(),
            sspi: sspi_token,
            ..Login::default()
        };
        let payload = login.encode();
        let mut framed = Vec::new();
        packet::frame(
            packet::LOGIN7,
            &payload,
            packet::DEFAULT_PACKET_SIZE,
            &mut framed,
        );
        let stream = match (login_only, stream) {
            (true, Stream::Tls(mut tls)) => {
                // Only the first LOGIN7 packet is encrypted, then TLS is dropped.
                let first = packet::first_packet_len(payload.len(), packet::DEFAULT_PACKET_SIZE);
                tls.write_all(&framed[..first])?;
                tls.flush()?;
                let (transport, leftover) = tls.into_parts();
                reader.push_raw(&leftover);
                let mut tcp = transport.into_inner();
                tcp.write_all(&framed[first..])?;
                tcp.flush()?;
                Stream::Plain(tcp)
            }
            (_, mut s) => {
                s.write_all(&framed)?;
                s.flush()?;
                s
            }
        };

        let mut client = Client {
            stream,
            reader,
            packet_size: packet::DEFAULT_PACKET_SIZE,
            session: Session::default(),
            messages: Vec::new(),
            broken: false,
            timeout: cfg.timeout(),
            login_ack: LoginAck::default(),
            encrypted,
        };
        let ack = client.finish_login(negotiate.as_mut())?;
        client.login_ack = ack;
        client.apply_packet_size();
        client.set_timeout(cfg.timeout())?;
        Ok(client)
    }

    /// Reads the login response(s) until LOGINACK. With integrated authentication each
    /// SSPI token the server sends is answered with an SSPI message.
    fn finish_login(&mut self, mut negotiate: Option<&mut Negotiate>) -> Result<LoginAck, Error> {
        let mut rounds = 0;
        loop {
            let (ptype, msg) = self.reader.read_message(&mut self.stream)?;
            if ptype != packet::REPLY {
                return Err(Error::Malformed {
                    token: 0,
                    offset: 0,
                    what: "login response is not a REPLY packet",
                });
            }
            let resp = token::parse(&msg, &mut self.session)?;
            self.messages.extend(resp.messages);
            if let Some(e) = resp.error {
                return Err(e);
            }
            if let Some((server, port)) = self.session.routing.take() {
                return Err(Error::Routing { server, port });
            }
            match (resp.login_ack, resp.sspi, negotiate.as_deref_mut()) {
                (Some(ack), challenge, neg) => {
                    // Kerberos mutual authentication: the server's last token (AP-REP)
                    // comes with LOGINACK; SSPI verifies it, nothing is sent back.
                    if let (Some(tok), Some(neg)) = (challenge, neg)
                        && !neg.is_done()
                    {
                        neg.next(&tok)?;
                    }
                    return Ok(ack);
                }
                (None, Some(challenge), Some(neg)) => {
                    rounds += 1;
                    if rounds > MAX_SSPI_ROUNDS {
                        return Err(Error::Malformed {
                            token: token::SSPI,
                            offset: 0,
                            what: "too many SSPI round trips during login",
                        });
                    }
                    let answer = if neg.is_done() {
                        Vec::new()
                    } else {
                        neg.next(&challenge)?
                    };
                    // SPNEGO's last server token (accept-completed + mechListMIC) arrives
                    // in a message of its own and needs no answer: LOGINACK follows. An
                    // empty SSPI message here would be read by the server as a request
                    // and shift every later response by one.
                    if !answer.is_empty() {
                        let mut framed = Vec::new();
                        packet::frame(
                            packet::SSPI,
                            &answer,
                            packet::DEFAULT_PACKET_SIZE,
                            &mut framed,
                        );
                        write_packets(&mut self.stream, &framed)?;
                        self.stream.flush()?;
                    }
                }
                (None, Some(_), None) => {
                    return Err(Error::Unsupported(
                        "the server asked for integrated (SSPI) authentication; set Integrated Security=SSPI",
                    ));
                }
                (None, None, _) => {
                    return Err(Error::Malformed {
                        token: 0,
                        offset: msg.len(),
                        what: "login response has no LOGINACK",
                    });
                }
            }
        }
    }

    fn apply_packet_size(&mut self) {
        if let Some(n) = self.session.packet_size.take() {
            self.packet_size = n;
        }
    }

    /// Changes the read/write timeout for later requests (`None` = wait forever).
    ///
    /// # Errors
    /// [`Error::Io`] if the socket rejects the setting.
    pub fn set_timeout(&mut self, timeout: Option<Duration>) -> Result<(), Error> {
        let timeout = timeout.filter(|d| !d.is_zero());
        let tcp = self.stream.tcp();
        tcp.set_read_timeout(timeout)?;
        tcp.set_write_timeout(timeout)?;
        self.timeout = timeout;
        Ok(())
    }

    /// Runs `sql`. Without parameters it is sent as a plain SQL batch; with parameters as
    /// `sp_executesql` (like go-mssqldb), so `@name` placeholders refer to `params`.
    ///
    /// # Errors
    /// [`Error::Server`] if any statement failed (the results are discarded),
    /// [`Error::Timeout`], [`Error::Io`], [`Error::Malformed`], [`Error::Param`],
    /// [`Error::UnsupportedCodePage`], [`Error::Broken`].
    pub fn query(&mut self, sql: &str, params: &[Param]) -> Result<Results, Error> {
        if params.is_empty() {
            return self.batch(sql);
        }
        let payload = params::sp_executesql(
            sql,
            params,
            self.session.transaction,
            self.session.collation.unwrap_or_default(),
        )?;
        self.request(packet::RPC, &payload)
    }

    /// Calls the stored procedure `procedure` (e.g. `dbo.MM_Op_GetLog`) as an RPC request
    /// with named parameters, the way go-mssqldb runs a query text that is a bare
    /// procedure name. Every result set, the procedure's return status
    /// ([`Results::return_status`]) and the values of [`Param::output`] parameters
    /// ([`Results::output_params`]) are returned. No SQL text is built from the
    /// parameters.
    ///
    /// # Errors
    /// As [`query`](Self::query); [`Error::Param`] for an empty procedure name.
    pub fn call(&mut self, procedure: &str, params: &[Param]) -> Result<Results, Error> {
        let payload = params::rpc_call(
            procedure,
            params,
            self.session.transaction,
            self.session.collation.unwrap_or_default(),
        )?;
        self.request(packet::RPC, &payload)
    }

    /// Runs `sql` like [`query`](Self::query) and returns the number of rows affected
    /// (go-mssqldb's `RowsAffected`: the sum of all row counts the server reported).
    ///
    /// # Errors
    /// As [`query`](Self::query).
    pub fn execute(&mut self, sql: &str, params: &[Param]) -> Result<u64, Error> {
        self.query(sql, params).map(|r| r.rows_affected())
    }

    /// Sends `sql` as a plain SQL batch (no parameters), e.g. `CREATE PROCEDURE`, DDL, or
    /// several statements. Server errors carry the line number within the batch.
    ///
    /// # Errors
    /// As [`query`](Self::query).
    pub fn batch(&mut self, sql: &str) -> Result<Results, Error> {
        let payload = params::sql_batch(sql, self.session.transaction);
        self.request(packet::SQL_BATCH, &payload)
    }

    /// Checks the connection with `select 1;` (what go-mssqldb's `Ping` runs).
    ///
    /// # Errors
    /// As [`query`](Self::query).
    pub fn ping(&mut self) -> Result<(), Error> {
        self.batch("select 1;").map(|_| ())
    }

    /// Returns and clears the INFO and ERROR messages received so far (`PRINT` output,
    /// "Changed database context", every ERROR of failed statements, ...).
    pub fn take_messages(&mut self) -> Vec<Message> {
        std::mem::take(&mut self.messages)
    }

    /// Current database (tracked from ENVCHANGE).
    pub fn database(&self) -> &str {
        &self.session.database
    }

    /// `true` inside an explicit transaction (`BEGIN TRAN` ... `COMMIT`).
    pub fn in_transaction(&self) -> bool {
        self.session.transaction != 0
    }

    /// Negotiated packet size.
    pub fn packet_size(&self) -> usize {
        self.packet_size
    }

    /// `true` if the whole session is encrypted (not just the login).
    pub fn is_encrypted(&self) -> bool {
        self.encrypted
    }

    /// Server version from LOGINACK: (major, minor, build), e.g. (16, 0, 4135).
    pub fn server_version(&self) -> (u8, u8, u16) {
        self.login_ack.version
    }

    /// Server product name from LOGINACK ("Microsoft SQL Server").
    pub fn server_name(&self) -> &str {
        &self.login_ack.prog_name
    }

    /// TDS version the server accepted (0x74000004 for TDS 7.4).
    pub fn tds_version(&self) -> u32 {
        self.login_ack.tds_version
    }

    /// `true` after an unrecoverable I/O or protocol failure; reconnect.
    pub fn is_broken(&self) -> bool {
        self.broken
    }

    /// Closes the connection (sends TLS close_notify when encrypted).
    pub fn close(mut self) {
        if let Stream::Tls(t) = &mut self.stream {
            let _ = t.shutdown();
        }
        let _ = self.stream.tcp().shutdown(std::net::Shutdown::Both);
    }

    fn request(&mut self, ptype: u8, payload: &[u8]) -> Result<Results, Error> {
        if self.broken {
            return Err(Error::Broken);
        }
        let mut framed = Vec::with_capacity(payload.len() + 64);
        packet::frame(ptype, payload, self.packet_size, &mut framed);
        if let Err(e) = write_packets(&mut self.stream, &framed).and_then(|_| self.stream.flush()) {
            self.broken = true;
            return Err(Error::Io(e));
        }
        let (rtype, msg) = match self.reader.read_message(&mut self.stream) {
            Ok(m) => m,
            Err(Error::Io(e)) if is_timeout(&e) => {
                self.cancel();
                return Err(Error::Timeout);
            }
            Err(e) => {
                self.broken = true;
                return Err(e);
            }
        };
        if rtype != packet::REPLY {
            self.broken = true;
            return Err(Error::Malformed {
                token: 0,
                offset: 0,
                what: "response is not a REPLY packet",
            });
        }
        let resp = token::parse(&msg, &mut self.session);
        self.apply_packet_size();
        let resp = resp?;
        self.messages.extend(resp.messages);
        if let Some((server, port)) = self.session.routing.take() {
            return Err(Error::Routing { server, port });
        }
        match resp.error {
            Some(e) => Err(e),
            None => Ok(resp.results),
        }
    }

    /// Sends ATTENTION and drains the response until the server acknowledges it.
    fn cancel(&mut self) {
        let mut att = Vec::new();
        packet::frame(packet::ATTENTION, &[], self.packet_size, &mut att);
        if self
            .stream
            .write_all(&att)
            .and_then(|_| self.stream.flush())
            .is_err()
        {
            self.broken = true;
            return;
        }
        let _ = self
            .stream
            .tcp()
            .set_read_timeout(Some(CANCEL_DRAIN_TIMEOUT));
        loop {
            match self.reader.read_message(&mut self.stream) {
                Ok((_, msg)) => {
                    let acked = match token::parse(&msg, &mut self.session) {
                        Ok(r) => r.attention_ack,
                        // A cut-off token stream: look at the final DONE directly.
                        Err(_) => {
                            msg.len() >= 13 && msg[msg.len() - 13] == DONE && {
                                let s =
                                    u16::from_le_bytes([msg[msg.len() - 12], msg[msg.len() - 11]]);
                                s & DONE_ATTN != 0
                            }
                        }
                    };
                    if acked {
                        break;
                    }
                }
                Err(_) => {
                    self.broken = true;
                    break;
                }
            }
        }
        let timeout = self.timeout;
        let _ = self.stream.tcp().set_read_timeout(timeout);
    }
}

/// Writes framed packets. Over TLS each TDS packet goes through its own `write`, so every
/// packet starts a new TLS record: SQL Server drops encrypted sessions whose records
/// split packets differently (the same reason go-mssqldb writes one packet per TLS write).
fn write_packets<W: Write>(w: &mut W, framed: &[u8]) -> io::Result<()> {
    let mut rest = framed;
    while rest.len() >= packet::HEADER_LEN {
        let len = usize::from(u16::from_be_bytes([rest[2], rest[3]]))
            .clamp(packet::HEADER_LEN, rest.len());
        w.write_all(&rest[..len])?;
        rest = &rest[len..];
    }
    w.write_all(rest)
}
