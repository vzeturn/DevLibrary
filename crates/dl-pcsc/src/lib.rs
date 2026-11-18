#![doc = include_str!("../README.md")]

pub mod apdu;
mod convert;
mod error;
#[cfg(any(windows, unix))]
mod sys;
#[cfg(not(any(windows, unix)))]
#[path = "sys_stub.rs"]
mod sys;

use core::cell::Cell;
use core::fmt;
use core::marker::PhantomData;
use core::ops::BitOr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

pub use error::{Error, codes};

/// The pseudo reader name for reader hot-plug events (`\\?PnP?\Notification`). Pass it to
/// [`Context::wait`] with the other readers: the call then also returns when a reader is
/// added or removed. Supported by Windows and by pcsc-lite built with hot-plug support.
///
/// On Windows the reader count must be in the upper 16 bits of `current`
/// (`State::from_bits(count << 16)`); passing the returned state back does that.
pub const PNP_NOTIFICATION: &str = r"\\?PnP?\Notification";

/// Loads the platform's PC/SC library if that has not happened yet. Every other call does
/// this on first use; calling it at start-up lets a program log early that PC/SC is
/// missing ([`Error::Unsupported`] with the reason).
pub fn load_library() -> Result<(), Error> {
    sys::load_library()
}

/// Scope of a resource manager context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// `SCARD_SCOPE_USER`: in the user's domain.
    User,
    /// `SCARD_SCOPE_SYSTEM`: in the system's domain.
    System,
}

impl Scope {
    fn raw(self) -> u32 {
        match self {
            Scope::User => 0,
            Scope::System => 2,
        }
    }
}

/// How a connection shares the card with other applications.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Share {
    /// `SCARD_SHARE_SHARED`: other applications may connect too.
    Shared,
    /// `SCARD_SHARE_EXCLUSIVE`: no other application may connect.
    Exclusive,
    /// `SCARD_SHARE_DIRECT`: direct access to the reader, even without a card (pass
    /// [`Protocols::NONE`]).
    Direct,
}

impl Share {
    fn raw(self) -> u32 {
        match self {
            Share::Exclusive => 1,
            Share::Shared => 2,
            Share::Direct => 3,
        }
    }
}

/// A set of card protocols acceptable for a connection.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Protocols(u32);

impl Protocols {
    /// No protocol (`SCARD_PROTOCOL_UNDEFINED`), for [`Share::Direct`].
    pub const NONE: Protocols = Protocols(0);
    /// T=0.
    pub const T0: Protocols = Protocols(1);
    /// T=1.
    pub const T1: Protocols = Protocols(2);
    /// Raw protocol (`SCARD_PROTOCOL_RAW`, whose value differs between Windows and
    /// pcsc-lite; this crate maps it).
    pub const RAW: Protocols = Protocols(4);
    /// T=0 or T=1 (`SCARD_PROTOCOL_ANY`), the usual choice; contactless readers present
    /// cards as T=1.
    pub const ANY: Protocols = Protocols(3);

    /// Whether every protocol of `other` is in `self`.
    pub const fn contains(self, other: Protocols) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for Protocols {
    type Output = Protocols;
    fn bitor(self, rhs: Protocols) -> Protocols {
        Protocols(self.0 | rhs.0)
    }
}

impl fmt::Debug for Protocols {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = [
            (Protocols::T0, "T0"),
            (Protocols::T1, "T1"),
            (Protocols::RAW, "RAW"),
        ];
        let set: Vec<&str> = names
            .iter()
            .filter(|(p, _)| self.contains(*p))
            .map(|(_, n)| *n)
            .collect();
        if set.is_empty() {
            f.write_str("Protocols(NONE)")
        } else {
            write!(f, "Protocols({})", set.join(" | "))
        }
    }
}

/// The protocol of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Protocol {
    /// T=0.
    T0,
    /// T=1.
    T1,
    /// Raw.
    Raw,
}

/// What happens to the card when a connection or transaction ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Disposition {
    /// `SCARD_LEAVE_CARD`: do nothing (what dropping a [`Card`] does).
    #[default]
    Leave,
    /// `SCARD_RESET_CARD`: reset the card.
    Reset,
    /// `SCARD_UNPOWER_CARD`: power the card down.
    Unpower,
    /// `SCARD_EJECT_CARD`: eject the card (if the reader can).
    Eject,
}

impl Disposition {
    fn raw(self) -> u32 {
        match self {
            Disposition::Leave => 0,
            Disposition::Reset => 1,
            Disposition::Unpower => 2,
            Disposition::Eject => 3,
        }
    }
}

/// Reader state flags (`SCARD_STATE_*`) of [`Context::wait`]. The upper 16 bits hold the
/// reader's event counter (incremented on every card insertion or removal), see
/// [`State::event_count`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct State(u32);

impl State {
    /// `SCARD_STATE_UNAWARE`: the current state is unknown; the wait returns at once.
    pub const UNAWARE: State = State(0x0000);
    /// `SCARD_STATE_IGNORE`: ignore this reader.
    pub const IGNORE: State = State(0x0001);
    /// `SCARD_STATE_CHANGED`: the state differs from the given current state.
    pub const CHANGED: State = State(0x0002);
    /// `SCARD_STATE_UNKNOWN`: the reader name is not recognized (e.g. it was unplugged).
    pub const UNKNOWN: State = State(0x0004);
    /// `SCARD_STATE_UNAVAILABLE`: the reader state is not available.
    pub const UNAVAILABLE: State = State(0x0008);
    /// `SCARD_STATE_EMPTY`: no card in the reader.
    pub const EMPTY: State = State(0x0010);
    /// `SCARD_STATE_PRESENT`: a card is in the reader.
    pub const PRESENT: State = State(0x0020);
    /// `SCARD_STATE_ATRMATCH`: the card's ATR matches a given ATR.
    pub const ATRMATCH: State = State(0x0040);
    /// `SCARD_STATE_EXCLUSIVE`: the card is used exclusively by another application.
    pub const EXCLUSIVE: State = State(0x0080);
    /// `SCARD_STATE_INUSE`: the card is in use by other applications.
    pub const INUSE: State = State(0x0100);
    /// `SCARD_STATE_MUTE`: the card does not answer (unreadable).
    pub const MUTE: State = State(0x0200);
    /// `SCARD_STATE_UNPOWERED`: the card is not powered.
    pub const UNPOWERED: State = State(0x0400);

    const NAMES: [(State, &'static str); 11] = [
        (State::IGNORE, "IGNORE"),
        (State::CHANGED, "CHANGED"),
        (State::UNKNOWN, "UNKNOWN"),
        (State::UNAVAILABLE, "UNAVAILABLE"),
        (State::EMPTY, "EMPTY"),
        (State::PRESENT, "PRESENT"),
        (State::ATRMATCH, "ATRMATCH"),
        (State::EXCLUSIVE, "EXCLUSIVE"),
        (State::INUSE, "INUSE"),
        (State::MUTE, "MUTE"),
        (State::UNPOWERED, "UNPOWERED"),
    ];

    /// The state with these raw bits (flags and event counter).
    pub const fn from_bits(bits: u32) -> State {
        State(bits)
    }

    /// The raw bits.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Whether every flag of `other` is set in `self`.
    pub const fn contains(self, other: State) -> bool {
        self.0 & other.0 == other.0
    }

    /// The event counter in the upper 16 bits. For [`PNP_NOTIFICATION`] it is the number
    /// of readers on Windows.
    pub const fn event_count(self) -> u16 {
        (self.0 >> 16) as u16
    }

    /// A card is present and answers (`PRESENT` without `MUTE`).
    pub const fn has_card(self) -> bool {
        self.contains(State::PRESENT) && !self.contains(State::MUTE)
    }
}

impl BitOr for State {
    type Output = State;
    fn bitor(self, rhs: State) -> State {
        State(self.0 | rhs.0)
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let set: Vec<&str> = State::NAMES
            .iter()
            .filter(|(s, _)| self.contains(*s))
            .map(|(_, n)| *n)
            .collect();
        let flags = if set.is_empty() {
            "UNAWARE".to_owned()
        } else {
            set.join(" | ")
        };
        write!(f, "State({flags}; events {})", self.event_count())
    }
}

/// A reader to watch in [`Context::wait`], with the state the caller knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderWait {
    /// Reader name from [`Context::list_readers`], or [`PNP_NOTIFICATION`].
    pub name: String,
    /// The state the caller last saw ([`State::UNAWARE`] at first); the wait returns when
    /// the reader's state differs from it.
    pub current: State,
}

impl ReaderWait {
    /// A reader with the state the caller knows.
    pub fn new(name: impl Into<String>, current: State) -> ReaderWait {
        ReaderWait {
            name: name.into(),
            current,
        }
    }
}

/// A reader's state returned by [`Context::wait`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderStatus {
    /// Reader name.
    pub name: String,
    /// The new state (`dwEventState`), with [`State::CHANGED`] set when it differs from the
    /// state passed in.
    pub state: State,
    /// The card's ATR when a card is present.
    pub atr: Vec<u8>,
}

impl ReaderStatus {
    /// The entry for the next [`Context::wait`]: this reader with this state as `current`.
    pub fn to_wait(&self) -> ReaderWait {
        ReaderWait::new(self.name.clone(), self.state)
    }
}

/// The card state of a connection ([`Card::status`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CardState {
    /// The state is unknown.
    Unknown,
    /// No card in the reader.
    Absent,
    /// A card is present but not in position for use.
    Present,
    /// A card is in position for use but not powered.
    Swallowed,
    /// The card is powered, the reader driver does not know its mode.
    Powered,
    /// The card was reset and awaits PTS negotiation.
    Negotiable,
    /// The card was reset and a protocol has been established.
    Specific,
}

/// Result of [`Card::status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardStatus {
    /// Reader name.
    pub reader: String,
    /// Card state.
    pub state: CardState,
    /// Active protocol, `None` if undefined.
    pub protocol: Option<Protocol>,
    /// The card's ATR.
    pub atr: Vec<u8>,
}

/// The context handle shared with [`Canceller`]s; `None` once released.
type Shared = Arc<Mutex<Option<sys::Handle>>>;

/// A PC/SC resource manager context (`SCardEstablishContext`), released on drop.
///
/// A context may move to another thread but not be used by two threads at once (it is
/// `Send`, not `Sync`), as PC/SC requires. To stop a blocking [`wait`](Context::wait) from
/// another thread, use a [`Canceller`].
pub struct Context {
    handle: sys::Handle,
    shared: Shared,
    _not_sync: PhantomData<Cell<()>>,
}

impl fmt::Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context").finish_non_exhaustive()
    }
}

impl Context {
    /// Connects to the resource manager.
    ///
    /// Errors with [`Error::Unsupported`] when the PC/SC library is absent and with an
    /// error for which [`Error::is_no_service`] is true when the service (Windows SCardSvr,
    /// pcscd) is not running. On Windows 8 and later the service runs only while a reader
    /// is plugged in.
    pub fn establish(scope: Scope) -> Result<Context, Error> {
        let handle = sys::establish(scope.raw())?;
        Ok(Context {
            handle,
            shared: Arc::new(Mutex::new(Some(handle))),
            _not_sync: PhantomData,
        })
    }

    /// Names of the connected readers; empty (not an error) when there are none.
    pub fn list_readers(&self) -> Result<Vec<String>, Error> {
        sys::list_readers(self.handle)
    }

    /// Blocks until the state of one of `readers` differs from its `current` state, the
    /// timeout expires (an error for which [`Error::is_timeout`] is true; `None` waits
    /// forever) or a [`Canceller`] cancels the wait ([`Error::is_cancelled`]). Returns the
    /// new state of every reader, in order (`SCardGetStatusChange`).
    ///
    /// Pass each returned state back as `current` ([`ReaderStatus::to_wait`]) to wait for
    /// the next change. With [`State::UNAWARE`] the call returns at once.
    pub fn wait(
        &self,
        readers: &[ReaderWait],
        timeout: Option<Duration>,
    ) -> Result<Vec<ReaderStatus>, Error> {
        if readers.is_empty() {
            return Err(Error::InvalidArgument("no readers to wait for".to_owned()));
        }
        sys::get_status_change(self.handle, timeout_ms(timeout), readers)
    }

    /// Waits until a responsive card is in `reader` ([`State::has_card`]) and returns the
    /// reader's state, with the card's ATR. Returns at once if a card is already there.
    ///
    /// Errors as [`wait`](Context::wait) does, and with `SCARD_E_UNKNOWN_READER` when the
    /// reader is unplugged during the wait.
    pub fn wait_for_card(
        &self,
        reader: &str,
        timeout: Option<Duration>,
    ) -> Result<ReaderStatus, Error> {
        let deadline = timeout.and_then(|t| Instant::now().checked_add(t));
        let mut current = State::UNAWARE;
        loop {
            let left = match (timeout, deadline) {
                (None, _) => None,
                (Some(_), Some(d)) => Some(d.saturating_duration_since(Instant::now())),
                // The deadline does not fit in an `Instant`: wait as long as PC/SC allows.
                (Some(t), None) => Some(t),
            };
            let status = self
                .wait(&[ReaderWait::new(reader, current)], left)?
                .pop()
                .ok_or(Error::Pcsc {
                    op: "SCardGetStatusChange",
                    code: codes::SCARD_E_UNEXPECTED,
                })?;
            if status.state.has_card() {
                return Ok(status);
            }
            if status.state.contains(State::UNKNOWN) {
                return Err(Error::Pcsc {
                    op: "SCardGetStatusChange",
                    code: codes::SCARD_E_UNKNOWN_READER,
                });
            }
            current = status.state;
        }
    }

    /// A handle that cancels blocking waits on this context from another thread.
    pub fn canceller(&self) -> Canceller {
        Canceller {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Connects to the card in `reader` (`SCardConnect`).
    ///
    /// Errors with `SCARD_E_NO_SMARTCARD` ([`Error::is_removed`]) when there is no card and
    /// `SCARD_E_SHARING_VIOLATION` when another application holds it exclusively.
    pub fn connect(
        &self,
        reader: &str,
        share: Share,
        protocols: Protocols,
    ) -> Result<Card<'_>, Error> {
        let raw = convert::protocols_to_raw(protocols, convert::PROTOCOL_RAW);
        let (handle, protocol) = sys::connect(self.handle, reader, share.raw(), raw)?;
        Ok(Card {
            handle,
            protocol,
            _ctx: PhantomData,
        })
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // Take the handle under the lock: a running `cancel` finishes first and no later
        // `cancel` can use the released handle.
        let taken = self
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if taken.is_some() {
            sys::release(self.handle);
        }
    }
}

/// Cancels blocking [`Context::wait`] calls of a context from any thread (`SCardCancel`).
/// Cheap to clone; stays valid after the context is dropped (then cancelling does nothing).
#[derive(Clone)]
pub struct Canceller {
    shared: Shared,
}

impl fmt::Debug for Canceller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Canceller").finish_non_exhaustive()
    }
}

impl Canceller {
    /// Makes a pending or the next wait on the context return an error for which
    /// [`Error::is_cancelled`] is true. Does nothing once the context is dropped.
    pub fn cancel(&self) -> Result<(), Error> {
        let guard = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        match *guard {
            Some(handle) => sys::cancel(handle),
            None => Ok(()),
        }
    }
}

/// `None` = forever; otherwise milliseconds, capped just below `INFINITE`.
fn timeout_ms(timeout: Option<Duration>) -> u32 {
    match timeout {
        None => sys::INFINITE,
        Some(t) => u32::try_from(t.as_millis())
            .unwrap_or(u32::MAX)
            .min(sys::INFINITE - 1),
    }
}

/// Receive buffer for short APDUs: 256 data bytes and the status word.
const SHORT_RESPONSE: usize = 258;
/// Receive buffer for extended APDUs: 65536 data bytes and the status word.
const EXTENDED_RESPONSE: usize = 65538;

/// A connection to a card ([`Context::connect`]). Dropping it disconnects with
/// [`Disposition::Leave`].
pub struct Card<'ctx> {
    handle: sys::Handle,
    /// Active protocol, platform value.
    protocol: u32,
    _ctx: PhantomData<&'ctx Context>,
}

impl fmt::Debug for Card<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Card")
            .field("protocol", &self.protocol())
            .finish_non_exhaustive()
    }
}

impl Card<'_> {
    /// The protocol negotiated at connect time; `None` for a [`Share::Direct`] connection
    /// without a protocol.
    pub fn protocol(&self) -> Option<Protocol> {
        convert::protocol_from_raw(self.protocol, convert::PROTOCOL_RAW)
    }

    /// Sends a command APDU and returns the response including the status word
    /// (`SCardTransmit`). Responses of short APDUs can be up to 258 bytes, of extended
    /// APDUs up to 65538.
    ///
    /// Errors with [`Error::InvalidArgument`] for an APDU shorter than 4 bytes and with
    /// `SCARD_W_REMOVED_CARD` ([`Error::is_removed`]) when the card was taken away.
    pub fn transmit(&self, apdu: &[u8]) -> Result<Vec<u8>, Error> {
        if apdu.len() < 4 {
            return Err(Error::InvalidArgument(format!(
                "APDU of {} bytes is shorter than its 4-byte header",
                apdu.len()
            )));
        }
        let capacity = if apdu::is_extended(apdu) {
            EXTENDED_RESPONSE
        } else {
            SHORT_RESPONSE
        };
        sys::transmit(self.handle, self.protocol, apdu, capacity)
    }

    /// Reads the card UID with [`apdu::get_uid`] (`FF CA 00 00 00`) and returns it as
    /// uppercase hex ([`apdu::uid_hex`]).
    pub fn uid(&self) -> Result<String, Error> {
        let response = self.transmit(&apdu::get_uid())?;
        apdu::uid_hex(&response)
    }

    /// Reader name, card state, protocol and ATR (`SCardStatus`).
    pub fn status(&self) -> Result<CardStatus, Error> {
        let (reader, state, protocol, atr) = sys::status(self.handle)?;
        Ok(CardStatus {
            reader,
            state: convert::card_state(state),
            protocol: convert::protocol_from_raw(protocol, convert::PROTOCOL_RAW),
            atr,
        })
    }

    /// Starts exclusive use of the card for a sequence of commands
    /// (`SCardBeginTransaction`); blocks while another application holds a transaction.
    pub fn begin_transaction(&self) -> Result<(), Error> {
        sys::begin_transaction(self.handle)
    }

    /// Ends the transaction started with [`begin_transaction`](Card::begin_transaction)
    /// (`SCardEndTransaction`).
    pub fn end_transaction(&self, disposition: Disposition) -> Result<(), Error> {
        sys::end_transaction(self.handle, disposition.raw())
    }

    /// Disconnects with the given disposition (`SCardDisconnect`).
    pub fn disconnect(self, disposition: Disposition) -> Result<(), Error> {
        let handle = self.handle;
        // The handle is released here; `Drop` must not disconnect again. `Card` owns no
        // other resource, so forgetting it leaks nothing.
        core::mem::forget(self);
        sys::disconnect(handle, disposition.raw())
    }
}

impl Drop for Card<'_> {
    fn drop(&mut self) {
        // Errors are ignored: the card may already be gone.
        let _ = sys::disconnect(self.handle, Disposition::Leave.raw());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}
    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn thread_safety() {
        assert_send::<Context>();
        assert_send_sync::<Canceller>();
        assert_send_sync::<Error>();
    }

    #[test]
    fn state_flags() {
        let s = State::from_bits(0x0003_0122);
        assert!(s.contains(State::PRESENT | State::CHANGED | State::INUSE));
        assert!(!s.contains(State::EMPTY));
        assert_eq!(s.event_count(), 3);
        assert!(s.has_card());
        assert!(!(State::PRESENT | State::MUTE).has_card());
        assert_eq!(
            format!("{s:?}"),
            "State(CHANGED | PRESENT | INUSE; events 3)"
        );
        assert_eq!(format!("{:?}", State::UNAWARE), "State(UNAWARE; events 0)");
        assert_eq!(State::default(), State::UNAWARE);
    }

    #[test]
    fn protocols_and_dispositions() {
        assert!(Protocols::ANY.contains(Protocols::T0));
        assert!(Protocols::ANY.contains(Protocols::T1));
        assert!(!Protocols::ANY.contains(Protocols::RAW));
        assert_eq!(Protocols::T0 | Protocols::T1, Protocols::ANY);
        assert_eq!(format!("{:?}", Protocols::ANY), "Protocols(T0 | T1)");
        assert_eq!(format!("{:?}", Protocols::NONE), "Protocols(NONE)");
        assert_eq!(Disposition::default(), Disposition::Leave);
        assert_eq!(
            [
                Disposition::Leave,
                Disposition::Reset,
                Disposition::Unpower,
                Disposition::Eject
            ]
            .map(Disposition::raw),
            [0, 1, 2, 3]
        );
        assert_eq!(
            [Share::Exclusive, Share::Shared, Share::Direct].map(Share::raw),
            [1, 2, 3]
        );
        assert_eq!([Scope::User, Scope::System].map(Scope::raw), [0, 2]);
    }

    #[test]
    fn timeouts() {
        assert_eq!(timeout_ms(None), 0xFFFF_FFFF);
        assert_eq!(timeout_ms(Some(Duration::ZERO)), 0);
        assert_eq!(timeout_ms(Some(Duration::from_millis(1500))), 1500);
        assert_eq!(timeout_ms(Some(Duration::from_secs(u64::MAX))), 0xFFFF_FFFE);
    }

    #[test]
    fn pnp_name() {
        assert_eq!(PNP_NOTIFICATION.len(), 20);
        assert!(PNP_NOTIFICATION.starts_with("\\\\?PnP?\\"));
    }

    #[test]
    fn status_to_wait() {
        let s = ReaderStatus {
            name: "R".into(),
            state: State::EMPTY | State::CHANGED,
            atr: Vec::new(),
        };
        assert_eq!(
            s.to_wait(),
            ReaderWait::new("R", State::EMPTY | State::CHANGED)
        );
    }
}
