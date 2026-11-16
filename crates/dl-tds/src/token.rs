//! Token stream parser: turns one response message into results, messages and session
//! state changes.

use std::sync::Arc;

use crate::collation::Collation;
use crate::results::{Column, Message, MessageKind, ResultSet, Results, Row};
use crate::types::TypeInfo;
use crate::wire::Cursor;
use crate::{Error, Value};

pub(crate) const RETURNSTATUS: u8 = 0x79;
pub(crate) const COLMETADATA: u8 = 0x81;
pub(crate) const ALTMETADATA: u8 = 0x88;
pub(crate) const DATACLASSIFICATION: u8 = 0xA3;
pub(crate) const TABNAME: u8 = 0xA4;
pub(crate) const COLINFO: u8 = 0xA5;
pub(crate) const ORDER: u8 = 0xA9;
pub(crate) const ERROR: u8 = 0xAA;
pub(crate) const INFO: u8 = 0xAB;
pub(crate) const RETURNVALUE: u8 = 0xAC;
pub(crate) const LOGINACK: u8 = 0xAD;
pub(crate) const FEATUREEXTACK: u8 = 0xAE;
pub(crate) const ROW: u8 = 0xD1;
pub(crate) const NBCROW: u8 = 0xD2;
pub(crate) const ALTROW: u8 = 0xD3;
pub(crate) const ENVCHANGE: u8 = 0xE3;
pub(crate) const SESSIONSTATE: u8 = 0xE4;
pub(crate) const SSPI: u8 = 0xED;
pub(crate) const FEDAUTHINFO: u8 = 0xEE;
pub(crate) const DONE: u8 = 0xFD;
pub(crate) const DONEPROC: u8 = 0xFE;
pub(crate) const DONEINPROC: u8 = 0xFF;

// DONE status bits.
#[cfg(test)]
pub(crate) const DONE_MORE: u16 = 0x01;
pub(crate) const DONE_ERROR: u16 = 0x02;
pub(crate) const DONE_COUNT: u16 = 0x10;
pub(crate) const DONE_ATTN: u16 = 0x20;
pub(crate) const DONE_SRVERROR: u16 = 0x100;

/// Connection state that ENVCHANGE tokens update.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Session {
    pub(crate) database: String,
    pub(crate) language: String,
    /// New packet size announced by the server (consumed by the client).
    pub(crate) packet_size: Option<usize>,
    pub(crate) collation: Option<Collation>,
    /// Transaction descriptor for ALL_HEADERS; 0 outside a transaction.
    pub(crate) transaction: u64,
    pub(crate) routing: Option<(String, u16)>,
}

/// LOGINACK contents.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct LoginAck {
    pub(crate) tds_version: u32,
    pub(crate) prog_name: String,
    pub(crate) version: (u8, u8, u16),
}

/// One parsed response message.
#[derive(Debug, Default)]
pub(crate) struct Response {
    pub(crate) results: Results,
    pub(crate) messages: Vec<Message>,
    /// The first failing statement's error (its last ERROR token), as go-mssqldb reports.
    pub(crate) error: Option<Error>,
    pub(crate) login_ack: Option<LoginAck>,
    /// A DONE with the ATTN bit (cancellation acknowledged) was seen.
    pub(crate) attention_ack: bool,
    /// An SSPI token was seen (integrated authentication continuation).
    pub(crate) sspi: bool,
}

/// Parses a complete response message. Every length is bounds-checked; malformed input
/// yields [`Error::Malformed`] with the token type and the offset within `msg`.
pub(crate) fn parse(msg: &[u8], sess: &mut Session) -> Result<Response, Error> {
    let mut cur = Cursor::new(msg);
    let mut resp = Response::default();
    let mut current: Option<(Arc<[Column]>, Vec<Row>)> = None;
    let mut columns: Option<Arc<[Column]>> = None;
    let mut group_errors: Vec<Message> = Vec::new();

    let close_set = |current: &mut Option<(Arc<[Column]>, Vec<Row>)>, results: &mut Results| {
        if let Some((columns, rows)) = current.take() {
            results.sets.push(ResultSet { columns, rows });
        }
    };

    while !cur.is_empty() {
        cur.token = 0;
        let token = cur.u8()?;
        cur.token = token;
        match token {
            COLMETADATA => {
                close_set(&mut current, &mut resp.results);
                let count = cur.u16()?;
                if count == 0xFFFF {
                    columns = None;
                    continue;
                }
                let mut cols = Vec::with_capacity((count as usize).min(cur.remaining()));
                for _ in 0..count {
                    let _user_type = cur.u32()?;
                    let flags = cur.u16()?;
                    let ti = TypeInfo::read(&mut cur, true)?;
                    let name = cur.b_varchar()?;
                    cols.push(Column {
                        name,
                        nullable: flags & 1 != 0,
                        ti,
                    });
                }
                let cols: Arc<[Column]> = cols.into();
                columns = Some(cols.clone());
                current = Some((cols, Vec::new()));
            }
            ROW | NBCROW => {
                let Some(cols) = columns.clone() else {
                    return Err(cur.err("row without column metadata"));
                };
                let mut values = Vec::with_capacity(cols.len());
                if token == ROW {
                    for c in cols.iter() {
                        values.push(c.ti.read_value(&mut cur)?);
                    }
                } else {
                    let bitmap = cur.take(cols.len().div_ceil(8))?;
                    for (i, c) in cols.iter().enumerate() {
                        if bitmap[i / 8] & (1 << (i % 8)) != 0 {
                            values.push(Value::Null);
                        } else {
                            values.push(c.ti.read_value(&mut cur)?);
                        }
                    }
                }
                let row = Row {
                    columns: cols.clone(),
                    values,
                };
                match &mut current {
                    Some((_, rows)) => rows.push(row),
                    None => current = Some((cols, vec![row])),
                }
            }
            DONE | DONEPROC | DONEINPROC => {
                let status = cur.u16()?;
                let _cur_cmd = cur.u16()?;
                let count = cur.u64()?;
                close_set(&mut current, &mut resp.results);
                if status & DONE_COUNT != 0 {
                    resp.results.row_counts.push(count);
                }
                if status & DONE_ATTN != 0 {
                    resp.attention_ack = true;
                }
                if token != DONEINPROC {
                    let failed =
                        status & (DONE_ERROR | DONE_SRVERROR) != 0 || !group_errors.is_empty();
                    if failed && resp.error.is_none() {
                        resp.error = Some(match group_errors.last() {
                            Some(m) => m.to_error(),
                            None => Error::Server {
                                number: 0,
                                state: 0,
                                class: 0,
                                message: "Request failed but didn't provide reason".into(),
                                server: String::new(),
                                procedure: String::new(),
                                line: 0,
                            },
                        });
                    }
                    group_errors.clear();
                }
            }
            ERROR | INFO => {
                let len = cur.u16()? as usize;
                let mut t = cur.sub(len)?;
                let m = Message {
                    kind: if token == ERROR {
                        MessageKind::Error
                    } else {
                        MessageKind::Info
                    },
                    number: t.i32()?,
                    state: t.u8()?,
                    class: t.u8()?,
                    message: t.us_varchar()?,
                    server: t.b_varchar()?,
                    procedure: t.b_varchar()?,
                    line: t.i32()?,
                };
                if token == ERROR {
                    group_errors.push(m.clone());
                }
                resp.messages.push(m);
            }
            ENVCHANGE => {
                let len = cur.u16()? as usize;
                let mut t = cur.sub(len)?;
                env_change(&mut t, sess)?;
            }
            RETURNSTATUS => resp.results.return_statuses.push(cur.i32()?),
            RETURNVALUE => {
                let _ordinal = cur.u16()?;
                let name = cur.b_varchar()?;
                let _status = cur.u8()?;
                let _user_type = cur.u32()?;
                let _flags = cur.u16()?;
                let ti = TypeInfo::read(&mut cur, false)?;
                let value = ti.read_value(&mut cur)?;
                let name = name.strip_prefix('@').unwrap_or(&name).to_string();
                resp.results.output.push((name, value));
            }
            LOGINACK => {
                let len = cur.u16()? as usize;
                let mut t = cur.sub(len)?;
                let _interface = t.u8()?;
                let tds_version = u32::from_be_bytes(t.array()?);
                // SQL Server pads the name with NULs inside the declared length.
                let prog_name = t.b_varchar()?.trim_end_matches('\0').to_string();
                let v: [u8; 4] = t.array()?;
                resp.login_ack = Some(LoginAck {
                    tds_version,
                    prog_name,
                    version: (v[0], v[1], u16::from_be_bytes([v[2], v[3]])),
                });
            }
            FEATUREEXTACK => loop {
                let id = cur.u8()?;
                if id == 0xFF {
                    break;
                }
                let len = cur.u32()? as usize;
                cur.skip(len)?;
            },
            ORDER | TABNAME | COLINFO => {
                let len = cur.u16()? as usize;
                cur.skip(len)?;
            }
            SSPI => {
                let len = cur.u16()? as usize;
                cur.skip(len)?;
                resp.sspi = true;
            }
            FEDAUTHINFO | SESSIONSTATE => {
                let len = cur.u32()? as usize;
                cur.skip(len)?;
            }
            ALTMETADATA | ALTROW => {
                return Err(Error::Unsupported(
                    "COMPUTE BY results (ALTMETADATA/ALTROW)",
                ));
            }
            DATACLASSIFICATION => return Err(Error::Unsupported("data classification tokens")),
            _ => return Err(cur.err("unknown token")),
        }
    }
    close_set(&mut current, &mut resp.results);
    if resp.error.is_none() {
        resp.error = group_errors.last().map(Message::to_error);
    }
    Ok(resp)
}

fn env_change(t: &mut Cursor<'_>, sess: &mut Session) -> Result<(), Error> {
    let kind = t.u8()?;
    match kind {
        1 => sess.database = t.b_varchar()?,
        2 => sess.language = t.b_varchar()?,
        4 => {
            let at = t.offset();
            let s = t.b_varchar()?;
            let n: usize = s.parse().map_err(|_| t.err_at(at, "invalid packet size"))?;
            if !(512..=32767).contains(&n) {
                return Err(t.err_at(at, "packet size out of range"));
            }
            sess.packet_size = Some(n);
        }
        7 => {
            let new = t.b_varbyte()?;
            if let Ok(b) = <[u8; 5]>::try_from(new) {
                sess.collation = Some(Collation::from_bytes(b));
            }
        }
        8 => {
            let at = t.offset();
            let new = t.b_varbyte()?;
            let b: [u8; 8] = new
                .try_into()
                .map_err(|_| t.err_at(at, "transaction descriptor is not 8 bytes"))?;
            sess.transaction = u64::from_le_bytes(b);
        }
        9 | 10 | 17 => sess.transaction = 0,
        20 => {
            let _len = t.u16()?;
            let protocol = t.u8()?;
            let port = t.u16()?;
            let server = t.us_varchar()?;
            if protocol == 0 {
                sess.routing = Some((server, port));
            }
        }
        // Charset, sort order, DTC, mirroring partner, reset ack, user instance: ignored.
        // The token length bounds the record, so anything left over is skipped.
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::types::ty;
    use crate::wire::to_utf16;

    /// Small builder for hand-made token streams.
    #[derive(Default)]
    pub(crate) struct B(pub(crate) Vec<u8>);

    impl B {
        pub(crate) fn u8(mut self, v: u8) -> B {
            self.0.push(v);
            self
        }
        pub(crate) fn u16(mut self, v: u16) -> B {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        pub(crate) fn u32(mut self, v: u32) -> B {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        pub(crate) fn u64(mut self, v: u64) -> B {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        pub(crate) fn raw(mut self, v: &[u8]) -> B {
            self.0.extend_from_slice(v);
            self
        }
        pub(crate) fn bvar(self, s: &str) -> B {
            let n = s.encode_utf16().count() as u8;
            self.u8(n).raw(&to_utf16(s))
        }
        pub(crate) fn usvar(self, s: &str) -> B {
            let n = s.encode_utf16().count() as u16;
            self.u16(n).raw(&to_utf16(s))
        }
        /// A token with a u16 length prefix around `body`.
        pub(crate) fn len16(self, token: u8, body: B) -> B {
            self.u8(token).u16(body.0.len() as u16).raw(&body.0)
        }
        pub(crate) fn done(self, token: u8, status: u16, count: u64) -> B {
            self.u8(token).u16(status).u16(0xC1).u64(count)
        }
        pub(crate) fn error(
            self,
            token: u8,
            number: i32,
            class: u8,
            msg: &str,
            proc_: &str,
            line: i32,
        ) -> B {
            let body = B::default()
                .u32(number as u32)
                .u8(1)
                .u8(class)
                .usvar(msg)
                .bvar("srv")
                .bvar(proc_)
                .u32(line as u32);
            self.len16(token, body)
        }
        /// COLMETADATA for (name, flags, type info bytes).
        pub(crate) fn colmeta(self, cols: &[(&str, u16, &[u8])]) -> B {
            let mut b = self.u8(COLMETADATA).u16(cols.len() as u16);
            for (name, flags, ti) in cols {
                b = b.u32(0).u16(*flags).raw(ti).bvar(name);
            }
            b
        }
    }

    pub(crate) fn run(b: &B) -> Response {
        parse(&b.0, &mut Session::default()).unwrap()
    }

    #[test]
    fn select_with_rows_and_nbcrow() {
        let b = B::default()
            .colmeta(&[
                ("id", 0, &[ty::INT4]),
                ("name", 1, &[ty::NVARCHAR, 20, 0, 9, 4, 0xD0, 0, 52]),
            ])
            .u8(ROW)
            .u32(1)
            .u16(4)
            .raw(&to_utf16("ab"))
            .u8(NBCROW)
            .u8(0b10)
            .u32(2)
            .done(DONE, DONE_COUNT, 2);
        let r = run(&b);
        assert!(r.error.is_none());
        let set = &r.results.sets[0];
        assert_eq!(set.columns()[1].name(), "name");
        assert!(set.columns()[1].nullable());
        assert_eq!(set.columns()[1].type_name(), "NVARCHAR");
        assert_eq!(set.columns()[1].max_length(), Some(20));
        assert_eq!(set.rows()[0].get::<i32>(0).unwrap(), 1);
        assert_eq!(set.rows()[0].get::<String>("NAME").unwrap(), "ab");
        assert_eq!(set.rows()[1].get::<Option<String>>(1).unwrap(), None);
        assert_eq!(r.results.rows_affected(), 2);
    }

    #[test]
    fn three_result_sets_and_return_status() {
        let b = B::default()
            .colmeta(&[("a", 0, &[ty::INT4])])
            .u8(ROW)
            .u32(1)
            .done(DONEINPROC, DONE_MORE | DONE_COUNT, 1)
            .colmeta(&[("b", 0, &[ty::BIT]), ("c", 0, &[ty::INT8])])
            .done(DONEINPROC, DONE_MORE | DONE_COUNT, 0)
            .colmeta(&[("d", 1, &[ty::FLTN, 8])])
            .u8(ROW)
            .u8(8)
            .u64(2.5f64.to_bits())
            .u8(ROW)
            .u8(0)
            .done(DONEINPROC, DONE_MORE | DONE_COUNT, 2)
            .u8(RETURNSTATUS)
            .u32(7)
            .done(DONEPROC, DONE_MORE, 0)
            .done(DONE, 0, 0);
        let r = run(&b);
        let sets = r.results.sets();
        assert_eq!(sets.len(), 3);
        assert_eq!(sets[0].len(), 1);
        assert!(sets[1].is_empty());
        assert_eq!(sets[1].columns().len(), 2);
        assert_eq!(sets[2].rows()[0].get::<f64>(0).unwrap(), 2.5);
        assert!(sets[2].rows()[1].value(0).unwrap().is_null());
        assert_eq!(r.results.return_status(), Some(7));
        assert_eq!(r.results.row_counts(), &[1, 0, 2]);
    }

    #[test]
    fn errors_are_grouped_by_done() {
        let b = B::default()
            .error(INFO, 5701, 0, "Changed database context to 'x'.", "", 1)
            .error(ERROR, 102, 15, "Incorrect syntax near 'FROM'.", "", 3)
            .error(ERROR, 319, 16, "Second error.", "", 4)
            .done(DONE, DONE_ERROR | DONE_MORE, 0)
            .error(ERROR, 208, 16, "Invalid object name.", "", 9)
            .done(DONE, DONE_ERROR, 0);
        let r = run(&b);
        assert_eq!(r.messages.len(), 4);
        assert_eq!(r.messages[0].kind, MessageKind::Info);
        match r.error {
            Some(Error::Server {
                number,
                class,
                line,
                ref message,
                ref server,
                ..
            }) => {
                assert_eq!((number, class, line), (319, 16, 4));
                assert_eq!(message, "Second error.");
                assert_eq!(server, "srv");
            }
            ref e => panic!("{e:?}"),
        }
        // DONE_ERROR without an ERROR token.
        let r = run(&B::default().done(DONE, DONE_ERROR, 0));
        assert!(matches!(r.error, Some(Error::Server { number: 0, .. })));
        // Errors inside a procedure surface at DONEPROC.
        let b = B::default()
            .error(ERROR, 50000, 16, "boom", "sp_X", 12)
            .done(DONEINPROC, DONE_MORE, 0)
            .done(DONEPROC, DONE_ERROR, 0);
        match run(&b).error {
            Some(Error::Server {
                ref procedure,
                line,
                ..
            }) => {
                assert_eq!(procedure, "sp_X");
                assert_eq!(line, 12);
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn envchange_updates_session() {
        let b = B::default()
            .len16(
                ENVCHANGE,
                B::default().u8(1).bvar("SMILE_FO").bvar("master"),
            )
            .len16(ENVCHANGE, B::default().u8(2).bvar("us_english").bvar(""))
            .len16(ENVCHANGE, B::default().u8(4).bvar("32767").bvar("4096"))
            .len16(
                ENVCHANGE,
                B::default().u8(7).u8(5).raw(&[0x2A, 4, 0xD0, 0, 0]).u8(0),
            )
            .len16(ENVCHANGE, B::default().u8(8).u8(8).u64(0x1234).u8(0))
            .len16(ENVCHANGE, B::default().u8(3).bvar("iso_1").bvar(""))
            .len16(ENVCHANGE, B::default().u8(99).raw(&[1, 2, 3]));
        let mut s = Session::default();
        parse(&b.0, &mut s).unwrap();
        assert_eq!(s.database, "SMILE_FO");
        assert_eq!(s.language, "us_english");
        assert_eq!(s.packet_size, Some(32767));
        assert_eq!(s.collation.unwrap().code_page(), Some(1258));
        assert_eq!(s.transaction, 0x1234);
        let b = B::default().len16(ENVCHANGE, B::default().u8(9).u8(0).u8(8).u64(0x1234));
        parse(&b.0, &mut s).unwrap();
        assert_eq!(s.transaction, 0);
        let b = B::default().len16(
            ENVCHANGE,
            B::default()
                .u8(20)
                .u16(13)
                .u8(0)
                .u16(1433)
                .usvar("srv2")
                .u16(0),
        );
        parse(&b.0, &mut s).unwrap();
        assert_eq!(s.routing, Some(("srv2".into(), 1433)));
        // Bad packet size, bad transaction descriptor.
        let b = B::default().len16(ENVCHANGE, B::default().u8(4).bvar("abc").bvar(""));
        assert!(parse(&b.0, &mut s).is_err());
        let b = B::default().len16(ENVCHANGE, B::default().u8(8).u8(3).raw(&[1, 2, 3]).u8(0));
        assert!(matches!(
            parse(&b.0, &mut s),
            Err(Error::Malformed {
                token: ENVCHANGE,
                ..
            })
        ));
    }

    #[test]
    fn login_ack_feature_ack_and_skipped_tokens() {
        let b = B::default()
            .len16(
                LOGINACK,
                B::default()
                    .u8(1)
                    .raw(&[0x74, 0, 0, 4])
                    .bvar("Microsoft SQL Server")
                    .raw(&[16, 0, 0x10, 0x27]),
            )
            .u8(FEATUREEXTACK)
            .u8(0x0A)
            .u32(1)
            .u8(1)
            .u8(0xFF)
            .len16(ORDER, B::default().u16(1))
            .len16(TABNAME, B::default().raw(&[1, 2, 3]))
            .len16(COLINFO, B::default().raw(&[1, 1, 0]))
            .len16(SSPI, B::default().raw(&[9, 9]))
            .u8(SESSIONSTATE)
            .u32(2)
            .raw(&[0, 0])
            .done(DONE, 0, 0);
        let r = run(&b);
        let ack = r.login_ack.unwrap();
        assert_eq!(ack.tds_version, 0x7400_0004);
        assert_eq!(ack.prog_name, "Microsoft SQL Server");
        assert_eq!(ack.version, (16, 0, 0x1027));
        assert!(r.sspi);
    }

    #[test]
    fn return_values_and_attention() {
        let b = B::default()
            .u8(RETURNVALUE)
            .u16(1)
            .bvar("@out")
            .u8(1)
            .u32(0)
            .u16(0)
            .raw(&[ty::INTN, 4])
            .u8(4)
            .u32(99)
            .done(DONE, DONE_ATTN, 0);
        let r = run(&b);
        assert_eq!(
            r.results.output_params(),
            &[("out".to_string(), Value::I32(99))]
        );
        assert!(r.attention_ack);
    }

    #[test]
    fn malformed_streams_report_token_and_offset() {
        // ROW before COLMETADATA.
        assert!(matches!(
            parse(&[ROW, 0], &mut Session::default()),
            Err(Error::Malformed {
                token: ROW,
                offset: 1,
                ..
            })
        ));
        // Unknown token.
        assert!(matches!(
            parse(
                &[DONE, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01],
                &mut Session::default()
            ),
            Err(Error::Malformed {
                token: 0x01,
                offset: 14,
                ..
            })
        ));
        // Truncated DONE.
        assert!(matches!(
            parse(&[DONE, 0, 0, 0], &mut Session::default()),
            Err(Error::Malformed { token: DONE, .. })
        ));
        // ERROR length larger than the message.
        assert!(parse(&[ERROR, 0xFF, 0x00, 1, 2], &mut Session::default()).is_err());
        // COLMETADATA claiming 65000 columns with no data.
        assert!(parse(&[COLMETADATA, 0xE8, 0xFD], &mut Session::default()).is_err());
        // ALTROW is unsupported.
        assert!(matches!(
            parse(&[ALTROW], &mut Session::default()),
            Err(Error::Unsupported(_))
        ));
    }
}
