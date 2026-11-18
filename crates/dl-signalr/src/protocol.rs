//! The JSON hub protocol, version 1: the handshake, the message codec and the
//! record-separator framing.
//!
//! Pure functions without I/O, available on every target (including
//! `wasm32-unknown-unknown`), so a browser build can run the protocol over its own
//! WebSocket.
//!
//! ```
//! use dl_signalr::protocol::{self, HubMessage, RecordReader};
//!
//! let frame = protocol::encode(&HubMessage::Invocation {
//!     invocation_id: Some("1".into()),
//!     target: "Echo".into(),
//!     arguments: vec!["xin chào".into()],
//! });
//! assert_eq!(frame, "{\"arguments\":[\"xin chào\"],\"invocationId\":\"1\",\"target\":\"Echo\",\"type\":1}\u{1e}");
//!
//! // One WebSocket message may hold several hub messages, and a hub message may span
//! // several WebSocket messages.
//! let mut records = RecordReader::new(1 << 20);
//! records.push(b"{\"type\":6}\x1e{\"type\":3,\"invocationId\":\"1\",\"res")?;
//! records.push("ult\":\"xin chào\"}\u{1e}".as_bytes())?;
//! assert_eq!(protocol::parse(records.next_record().unwrap())?, HubMessage::Ping);
//! match protocol::parse(records.next_record().unwrap())? {
//!     HubMessage::Completion { result, .. } => assert_eq!(result.unwrap(), "xin chào"),
//!     other => panic!("{other:?}"),
//! }
//! assert!(records.next_record().is_none());
//! # Ok::<(), dl_signalr::Error>(())
//! ```

use dl_json::{Map, ParseOptions, Value};

use crate::Error;

/// The byte that ends every message of the JSON protocol (ASCII "record separator").
pub const RECORD_SEPARATOR: u8 = 0x1e;

/// The handshake request: `{"protocol":"json","version":1}` and the separator.
pub const HANDSHAKE_REQUEST: &str = "{\"protocol\":\"json\",\"version\":1}\u{1e}";

/// A ping message with its separator.
pub const PING: &str = "{\"type\":6}\u{1e}";

/// Message type 1: a method call (both directions).
pub const TYPE_INVOCATION: i64 = 1;
/// Message type 2: one item of a streamed result.
pub const TYPE_STREAM_ITEM: i64 = 2;
/// Message type 3: the result or error of an invocation.
pub const TYPE_COMPLETION: i64 = 3;
/// Message type 4: a call that expects a stream (client to server).
pub const TYPE_STREAM_INVOCATION: i64 = 4;
/// Message type 5: cancels a stream (client to server).
pub const TYPE_CANCEL_INVOCATION: i64 = 5;
/// Message type 6: keepalive.
pub const TYPE_PING: i64 = 6;
/// Message type 7: the connection is being closed.
pub const TYPE_CLOSE: i64 = 7;

/// One hub message.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum HubMessage {
    /// A method call. Without an `invocation_id` no completion is expected.
    Invocation {
        /// Set when the caller waits for a completion.
        invocation_id: Option<String>,
        /// The method name.
        target: String,
        /// The arguments.
        arguments: Vec<Value>,
    },
    /// One item of a streamed result.
    StreamItem {
        /// The invocation the item belongs to.
        invocation_id: String,
        /// The item.
        item: Value,
    },
    /// The end of an invocation.
    Completion {
        /// The invocation that completed.
        invocation_id: String,
        /// The result; `None` when the message has no `result` member. A method
        /// returning `void` completes with `Some(Value::Null)` from ASP.NET Core.
        result: Option<Value>,
        /// The error text when the method failed.
        error: Option<String>,
    },
    /// Keepalive.
    Ping,
    /// The sender is closing the connection.
    Close {
        /// Why, when the connection closes because of an error.
        error: Option<String>,
        /// Whether the server allows the client to reconnect.
        allow_reconnect: bool,
    },
    /// A message type this crate does not handle (4 and 5, and newer types such as the
    /// stateful-reconnect acks). Only the type is kept.
    Other {
        /// The `type` member.
        kind: i64,
    },
}

/// The options used for every parse: numbers keep their exact source text.
fn parse_options() -> ParseOptions {
    ParseOptions {
        keep_raw_numbers: true,
        ..ParseOptions::default()
    }
}

/// Removes an optional string member; `null` counts as absent.
fn take_string(obj: &mut Map, key: &str, wrong: &'static str) -> Result<Option<String>, Error> {
    match obj.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(Error::InvalidMessage(wrong)),
    }
}

/// Parses one message (the bytes between two separators, without the separator).
///
/// Numbers are kept exactly as sent ([`dl_json::Number::as_raw_str`]), so decimals and
/// 64-bit integers survive a round trip. Unknown members (such as `headers`) are
/// ignored.
///
/// # Errors
/// [`Error::Json`] (with the position inside the message) for invalid JSON and
/// [`Error::InvalidMessage`] for a JSON value that is not a hub message.
pub fn parse(record: &[u8]) -> Result<HubMessage, Error> {
    let value = dl_json::parse_slice_with(record, parse_options())?;
    let Value::Object(mut obj) = value else {
        return Err(Error::InvalidMessage("message is not a JSON object"));
    };
    let kind = match obj.get("type") {
        Some(Value::Number(n)) => n
            .as_i64()
            .ok_or(Error::InvalidMessage("type is not an integer"))?,
        Some(_) => return Err(Error::InvalidMessage("type is not a number")),
        None => return Err(Error::InvalidMessage("message without a type")),
    };
    let message = match kind {
        TYPE_INVOCATION => {
            let invocation_id =
                take_string(&mut obj, "invocationId", "invocationId is not a string")?;
            let target = take_string(&mut obj, "target", "target is not a string")?
                .ok_or(Error::InvalidMessage("invocation without a target"))?;
            let arguments = match obj.remove("arguments") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(a)) => a,
                Some(_) => return Err(Error::InvalidMessage("arguments is not an array")),
            };
            HubMessage::Invocation {
                invocation_id,
                target,
                arguments,
            }
        }
        TYPE_STREAM_ITEM => HubMessage::StreamItem {
            invocation_id: take_string(&mut obj, "invocationId", "invocationId is not a string")?
                .ok_or(Error::InvalidMessage("stream item without an invocationId"))?,
            item: obj.remove("item").unwrap_or(Value::Null),
        },
        TYPE_COMPLETION => HubMessage::Completion {
            invocation_id: take_string(&mut obj, "invocationId", "invocationId is not a string")?
                .ok_or(Error::InvalidMessage("completion without an invocationId"))?,
            error: take_string(&mut obj, "error", "error is not a string")?,
            result: obj.remove("result"),
        },
        TYPE_PING => HubMessage::Ping,
        TYPE_CLOSE => HubMessage::Close {
            error: take_string(&mut obj, "error", "error is not a string")?,
            allow_reconnect: match obj.remove("allowReconnect") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => b,
                Some(_) => return Err(Error::InvalidMessage("allowReconnect is not a boolean")),
            },
        },
        kind => HubMessage::Other { kind },
    };
    Ok(message)
}

/// Appends `s` as a JSON string.
fn push_json_str(out: &mut String, s: &str) {
    dl_json::write_to(&Value::from(s), out);
}

/// Encodes a message as JSON followed by the record separator.
///
/// Invocations use the member order of the JavaScript client (`arguments`,
/// `invocationId`, `target`, `type`); the protocol allows any order.
pub fn encode(message: &HubMessage) -> String {
    let mut s = String::with_capacity(64);
    match message {
        HubMessage::Invocation {
            invocation_id,
            target,
            arguments,
        } => {
            s.push_str("{\"arguments\":[");
            for (i, a) in arguments.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                dl_json::write_to(a, &mut s);
            }
            s.push(']');
            if let Some(id) = invocation_id {
                s.push_str(",\"invocationId\":");
                push_json_str(&mut s, id);
            }
            s.push_str(",\"target\":");
            push_json_str(&mut s, target);
            s.push_str(",\"type\":1}");
        }
        HubMessage::StreamItem {
            invocation_id,
            item,
        } => {
            s.push_str("{\"type\":2,\"invocationId\":");
            push_json_str(&mut s, invocation_id);
            s.push_str(",\"item\":");
            dl_json::write_to(item, &mut s);
            s.push('}');
        }
        HubMessage::Completion {
            invocation_id,
            result,
            error,
        } => {
            s.push_str("{\"type\":3,\"invocationId\":");
            push_json_str(&mut s, invocation_id);
            if let Some(e) = error {
                s.push_str(",\"error\":");
                push_json_str(&mut s, e);
            } else if let Some(r) = result {
                s.push_str(",\"result\":");
                dl_json::write_to(r, &mut s);
            }
            s.push('}');
        }
        HubMessage::Ping => s.push_str("{\"type\":6}"),
        HubMessage::Close {
            error,
            allow_reconnect,
        } => {
            s.push_str("{\"type\":7");
            if let Some(e) = error {
                s.push_str(",\"error\":");
                push_json_str(&mut s, e);
            }
            if *allow_reconnect {
                s.push_str(",\"allowReconnect\":true");
            }
            s.push('}');
        }
        HubMessage::Other { kind } => {
            s.push_str("{\"type\":");
            s.push_str(&kind.to_string());
            s.push('}');
        }
    }
    s.push(char::from(RECORD_SEPARATOR));
    s
}

/// Checks the server's handshake response (the first message, `{}` on success).
///
/// # Errors
/// [`Error::Handshake`] with the server's `error` text, [`Error::Json`] or
/// [`Error::InvalidMessage`] for a malformed response.
pub fn parse_handshake_response(record: &[u8]) -> Result<(), Error> {
    let value = dl_json::parse_slice_with(record, parse_options())?;
    let Value::Object(obj) = value else {
        return Err(Error::InvalidMessage(
            "handshake response is not a JSON object",
        ));
    };
    match obj.get("error") {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(e)) => Err(Error::Handshake(e.clone())),
        Some(_) => Err(Error::InvalidMessage("handshake error is not a string")),
    }
}

/// Splits a byte stream into messages at the record separator.
///
/// Feed it the payload of every text (or binary) WebSocket message with
/// [`push`](RecordReader::push), then take messages with
/// [`next_record`](RecordReader::next_record) until it returns `None`. One WebSocket
/// message may carry several hub messages, and one hub message may be split across
/// WebSocket messages.
#[derive(Debug, Clone)]
pub struct RecordReader {
    buf: Vec<u8>,
    /// Start of the bytes not returned yet.
    start: usize,
    /// Bytes after the last separator (the unfinished message).
    tail: usize,
    max: usize,
}

impl RecordReader {
    /// A reader that refuses messages longer than `max_record_size` bytes.
    pub fn new(max_record_size: usize) -> RecordReader {
        RecordReader {
            buf: Vec::new(),
            start: 0,
            tail: 0,
            max: max_record_size,
        }
    }

    /// Appends received bytes.
    ///
    /// # Errors
    /// [`Error::MessageTooLarge`] when a message (finished or not) exceeds the limit;
    /// the reader should then be dropped with the connection.
    pub fn push(&mut self, data: &[u8]) -> Result<(), Error> {
        let mut segments = data.split(|&b| b == RECORD_SEPARATOR);
        let mut run = self.tail + segments.next().map_or(0, <[u8]>::len);
        for segment in segments {
            if run > self.max {
                return Err(Error::MessageTooLarge { limit: self.max });
            }
            run = segment.len();
        }
        if run > self.max {
            return Err(Error::MessageTooLarge { limit: self.max });
        }
        if self.start > 0 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        self.buf.extend_from_slice(data);
        self.tail = run;
        Ok(())
    }

    /// The next complete message, without its separator.
    pub fn next_record(&mut self) -> Option<&[u8]> {
        let pending = &self.buf[self.start..];
        if pending.len() == self.tail {
            return None; // no separator in the pending bytes
        }
        let end = pending.iter().position(|&b| b == RECORD_SEPARATOR)?;
        let begin = self.start;
        self.start += end + 1;
        Some(&self.buf[begin..begin + end])
    }

    /// Bytes received but not returned yet.
    pub fn buffered(&self) -> usize {
        self.buf.len() - self.start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> HubMessage {
        parse(s.as_bytes()).unwrap()
    }

    fn args(v: &[Value]) -> Vec<Value> {
        v.to_vec()
    }

    #[test]
    fn handshake_and_ping_bytes() {
        assert_eq!(
            HANDSHAKE_REQUEST.as_bytes(),
            b"{\"protocol\":\"json\",\"version\":1}\x1e"
        );
        assert_eq!(PING.as_bytes(), b"{\"type\":6}\x1e");
        assert_eq!(encode(&HubMessage::Ping), PING);
        parse_handshake_response(b"{}").unwrap();
        parse_handshake_response(b"{\"minorVersion\":1}").unwrap();
        match parse_handshake_response(
            br#"{"error":"Requested protocol 'messagepack' is not available."}"#,
        ) {
            Err(Error::Handshake(e)) => assert!(e.contains("messagepack")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse_handshake_response(b"[]"),
            Err(Error::InvalidMessage(_))
        ));
        assert!(matches!(
            parse_handshake_response(b"{"),
            Err(Error::Json(_))
        ));
    }

    #[test]
    fn encodes_like_the_proven_client() {
        // The frames the speaker agent sends to the production hub.
        let join = HubMessage::Invocation {
            invocation_id: None,
            target: "JoinRvc".into(),
            arguments: vec![5.into()],
        };
        assert_eq!(
            encode(&join),
            "{\"arguments\":[5],\"target\":\"JoinRvc\",\"type\":1}\u{1e}"
        );
        let ack = HubMessage::Invocation {
            invocation_id: Some("2".into()),
            target: "AckPrintJob".into(),
            arguments: vec!["id".into(), true.into(), Value::Null, false.into()],
        };
        assert_eq!(
            encode(&ack),
            "{\"arguments\":[\"id\",true,null,false],\"invocationId\":\"2\",\"target\":\"AckPrintJob\",\"type\":1}\u{1e}"
        );
        let empty = HubMessage::Invocation {
            invocation_id: Some("q\"1".into()),
            target: "Nothing".into(),
            arguments: vec![],
        };
        assert_eq!(
            encode(&empty),
            "{\"arguments\":[],\"invocationId\":\"q\\\"1\",\"target\":\"Nothing\",\"type\":1}\u{1e}"
        );
    }

    /// The examples of the protocol specification (aspnetcore `HubProtocol.md`).
    #[test]
    fn spec_examples_decode() {
        assert_eq!(
            p(r#"{"type":1,"invocationId":"123","target":"Send","arguments":[42,"Test Message"]}"#),
            HubMessage::Invocation {
                invocation_id: Some("123".into()),
                target: "Send".into(),
                arguments: args(&[42.into(), "Test Message".into()]),
            }
        );
        assert_eq!(
            p(
                r#"{"type":1,"headers":{"Foo":"Bar"},"target":"Send","arguments":[42,"Test Message"]}"#
            ),
            HubMessage::Invocation {
                invocation_id: None,
                target: "Send".into(),
                arguments: args(&[42.into(), "Test Message".into()]),
            }
        );
        assert_eq!(
            p(r#"{"type":2,"invocationId":"123","item":42}"#),
            HubMessage::StreamItem {
                invocation_id: "123".into(),
                item: 42.into()
            }
        );
        assert_eq!(
            p(r#"{"type":3,"invocationId":"123","error":"It didn't work!"}"#),
            HubMessage::Completion {
                invocation_id: "123".into(),
                result: None,
                error: Some("It didn't work!".into())
            }
        );
        assert_eq!(
            p(r#"{"type":3,"invocationId":"123","result":42}"#),
            HubMessage::Completion {
                invocation_id: "123".into(),
                result: Some(42.into()),
                error: None
            }
        );
        assert_eq!(
            p(r#"{"type":3,"invocationId":"123"}"#),
            HubMessage::Completion {
                invocation_id: "123".into(),
                result: None,
                error: None
            }
        );
        assert_eq!(
            p(r#"{"type":3,"invocationId":"123","result":null}"#),
            HubMessage::Completion {
                invocation_id: "123".into(),
                result: Some(Value::Null),
                error: None
            }
        );
        assert_eq!(p(r#"{"type":6}"#), HubMessage::Ping);
        assert_eq!(
            p(r#"{"type":7}"#),
            HubMessage::Close {
                error: None,
                allow_reconnect: false
            }
        );
        assert_eq!(
            p(
                r#"{"type":7,"error":"Connection closed because of an error!","allowReconnect":true}"#
            ),
            HubMessage::Close {
                error: Some("Connection closed because of an error!".into()),
                allow_reconnect: true
            }
        );
        assert_eq!(
            p(r#"{"type":5,"invocationId":"123"}"#),
            HubMessage::Other { kind: 5 }
        );
        assert_eq!(
            p(r#"{"type":4,"invocationId":"1","target":"Counter","arguments":[]}"#),
            HubMessage::Other { kind: 4 }
        );
    }

    #[test]
    fn round_trips_and_keeps_numbers_exact() {
        let text = r#"{"type":3,"invocationId":"7","result":{"total":12345678901234567890.1234567890,"id":18446744073709551617,"small":-0.10}}"#;
        let msg = p(text);
        let HubMessage::Completion {
            result: Some(r), ..
        } = &msg
        else {
            panic!("{msg:?}")
        };
        assert_eq!(
            r["total"].as_number().unwrap().as_raw_str(),
            Some("12345678901234567890.1234567890")
        );
        assert_eq!(
            encode(&msg),
            format!("{text}\u{1e}"),
            "re-encoding keeps every digit"
        );
        for m in [
            HubMessage::Invocation {
                invocation_id: Some("1".into()),
                target: "Bàn".into(),
                arguments: vec![dl_json::json!({"a": [1, 2.5, null]}), "\u{1e}\n".into()],
            },
            HubMessage::StreamItem {
                invocation_id: "2".into(),
                item: Value::Null,
            },
            HubMessage::Completion {
                invocation_id: "3".into(),
                result: Some(true.into()),
                error: None,
            },
            HubMessage::Completion {
                invocation_id: "4".into(),
                result: None,
                error: Some("boom".into()),
            },
            HubMessage::Close {
                error: Some("bye".into()),
                allow_reconnect: true,
            },
            HubMessage::Close {
                error: None,
                allow_reconnect: false,
            },
            HubMessage::Other { kind: 9 },
        ] {
            let wire = encode(&m);
            let body = wire.strip_suffix('\u{1e}').unwrap();
            // A separator inside a string is escaped, so it cannot split the message.
            assert!(!body.as_bytes().contains(&RECORD_SEPARATOR), "{wire}");
            assert_eq!(parse(body.as_bytes()).unwrap(), m);
        }
    }

    #[test]
    fn malformed_messages_are_typed_errors() {
        match parse(b"{\"type\":1,") {
            Err(Error::Json(e)) => assert_eq!(e.offset(), 10),
            other => panic!("{other:?}"),
        }
        for (input, want) in [
            (&b"[1]"[..], "message is not a JSON object"),
            (b"{}", "message without a type"),
            (b"{\"type\":\"1\"}", "type is not a number"),
            (b"{\"type\":1.5}", "type is not an integer"),
            (b"{\"type\":1}", "invocation without a target"),
            (b"{\"type\":1,\"target\":5}", "target is not a string"),
            (
                b"{\"type\":1,\"target\":\"x\",\"arguments\":{}}",
                "arguments is not an array",
            ),
            (b"{\"type\":3}", "completion without an invocationId"),
            (
                b"{\"type\":3,\"invocationId\":1}",
                "invocationId is not a string",
            ),
            (b"{\"type\":2}", "stream item without an invocationId"),
            (
                b"{\"type\":7,\"allowReconnect\":1}",
                "allowReconnect is not a boolean",
            ),
        ] {
            match parse(input) {
                Err(Error::InvalidMessage(m)) => assert_eq!(m, want),
                other => panic!("{}: {other:?}", String::from_utf8_lossy(input)),
            }
        }
        assert!(matches!(parse(b"\xff"), Err(Error::Json(_))));
    }

    #[test]
    fn records_split_and_join() {
        let mut r = RecordReader::new(64);
        assert!(r.next_record().is_none());
        r.push(b"{\"a\":1}\x1e{\"a\":2}\x1e{\"a").unwrap();
        assert_eq!(r.next_record().unwrap(), b"{\"a\":1}");
        assert_eq!(r.next_record().unwrap(), b"{\"a\":2}");
        assert!(r.next_record().is_none());
        assert_eq!(r.buffered(), 3);
        r.push(b"\":3").unwrap();
        assert!(r.next_record().is_none());
        r.push(b"}\x1e\x1e").unwrap();
        assert_eq!(r.next_record().unwrap(), b"{\"a\":3}");
        assert_eq!(r.next_record().unwrap(), b"");
        assert!(r.next_record().is_none());
        assert_eq!(r.buffered(), 0);
        r.push(b"").unwrap();
        assert!(r.next_record().is_none());
    }

    #[test]
    fn records_over_the_limit_are_refused() {
        let mut r = RecordReader::new(4);
        r.push(b"1234\x1e12").unwrap();
        assert!(matches!(
            r.push(b"345"),
            Err(Error::MessageTooLarge { limit: 4 })
        ));
        let mut r = RecordReader::new(4);
        r.push(b"123").unwrap();
        // Finished by this push but too long in total.
        assert!(r.push(b"45\x1e").is_err());
        let mut r = RecordReader::new(4);
        assert!(r.push(b"12\x1e12345\x1e").is_err());
        let mut r = RecordReader::new(4);
        r.push(b"1234").unwrap();
        r.push(b"\x1e").unwrap();
        assert_eq!(r.next_record().unwrap(), b"1234");
    }

    #[test]
    fn many_small_pushes_stay_linear() {
        // A 4 MiB message in 1024 pieces is quadratic if every push rescans the buffer.
        let mut r = RecordReader::new(8 << 20);
        let chunk = [b'x'; 4096];
        for _ in 0..1024 {
            r.push(&chunk).unwrap();
            assert!(r.next_record().is_none());
        }
        r.push(&[RECORD_SEPARATOR]).unwrap();
        assert_eq!(r.next_record().unwrap().len(), 4 << 20);
    }
}
