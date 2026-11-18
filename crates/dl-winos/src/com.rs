//! Late-bound COM automation through `IDispatch`, the way VBScript, `dynamic` in C# and
//! `win32com.client.Dispatch` in Python drive objects: create by ProgID, then get and set
//! properties and call methods by name.
//!
//! ```no_run
//! use dl_winos::com::{self, Object};
//!
//! let _apartment = com::init()?; // once per thread, before any object
//! let voice = Object::create("SAPI.SpVoice")?;
//! voice.set("Rate", 1)?;
//! voice.call("Speak", &["Xin chào".into(), 0.into()])?;
//!
//! let voices: Object = voice.call("GetVoices", &[])?.try_into()?;
//! let count: i32 = voices.get("Count")?.try_into()?;
//! for i in 0..count {
//!     let token: Object = voices.call("Item", &[i.into()])?.try_into()?;
//!     println!("{}", token.call("GetDescription", &[])?.as_str().unwrap_or("?"));
//! }
//! # Ok::<(), dl_winos::Error>(())
//! ```
//!
//! # Threads
//!
//! [`init`] joins the calling thread to a single-threaded apartment (STA). [`Apartment`],
//! [`Object`] and [`Variant`] are neither `Send` nor `Sync`: an object may only be used on
//! the thread that created it. Applications that use COM from async code or from several
//! threads run it on one dedicated thread and send it work over a channel. Each object
//! keeps the apartment initialised, so dropping the [`Apartment`] guard before the last
//! object is harmless: `CoUninitialize` runs when both are gone.

use core::cell::RefCell;
use core::fmt;

use crate::wide::to_wide;
use crate::{Error, com_sys};

/// `HRESULT` values callers commonly check, as returned by
/// [`Error::hresult`](crate::Error::hresult).
pub mod hresult {
    /// `S_FALSE`: success, but COM was already initialised on the thread.
    pub const S_FALSE: u32 = 0x0000_0001;
    /// `E_NOINTERFACE`: the object does not support `IDispatch`.
    pub const E_NOINTERFACE: u32 = 0x8000_4002;
    /// `E_POINTER`: a call returned a NULL interface pointer.
    pub const E_POINTER: u32 = 0x8000_4003;
    /// `E_OUTOFMEMORY`.
    pub const E_OUTOFMEMORY: u32 = 0x8007_000E;
    /// `E_INVALIDARG`.
    pub const E_INVALIDARG: u32 = 0x8007_0057;
    /// `RPC_E_CHANGED_MODE`: the thread is already in the multithreaded apartment.
    pub const RPC_E_CHANGED_MODE: u32 = 0x8001_0106;
    /// `CO_E_NOTINITIALIZED`: [`init`](super::init) was not called on this thread.
    pub const CO_E_NOTINITIALIZED: u32 = 0x8004_01F0;
    /// `CO_E_CLASSSTRING`: the ProgID is not registered (for example an OPOS control
    /// object that is not installed).
    pub const CO_E_CLASSSTRING: u32 = 0x8004_01F3;
    /// `REGDB_E_CLASSNOTREG`: the CLSID is registered but its server is not (often a
    /// 32-bit-only component in a 64-bit process, or the other way round).
    pub const REGDB_E_CLASSNOTREG: u32 = 0x8004_0154;
    /// `DISP_E_MEMBERNOTFOUND`: the member exists but not for this kind of access (for
    /// example a put on a read-only property).
    pub const DISP_E_MEMBERNOTFOUND: u32 = 0x8002_0003;
    /// `DISP_E_PARAMNOTFOUND`: a required argument is missing.
    pub const DISP_E_PARAMNOTFOUND: u32 = 0x8002_0004;
    /// `DISP_E_TYPEMISMATCH`: an argument has a type the member does not accept.
    pub const DISP_E_TYPEMISMATCH: u32 = 0x8002_0005;
    /// `DISP_E_UNKNOWNNAME`: no member with that name.
    pub const DISP_E_UNKNOWNNAME: u32 = 0x8002_0006;
    /// `DISP_E_EXCEPTION`: the member raised an exception that carried no `scode`.
    pub const DISP_E_EXCEPTION: u32 = 0x8002_0009;
    /// `DISP_E_OVERFLOW`: an argument does not fit the member's parameter type.
    pub const DISP_E_OVERFLOW: u32 = 0x8002_000A;
    /// `DISP_E_BADPARAMCOUNT`: wrong number of arguments.
    pub const DISP_E_BADPARAMCOUNT: u32 = 0x8002_000E;
}

/// `VARTYPE` values, as returned by [`Variant::vt`] and carried by
/// [`Variant::Unsupported`].
pub mod vt {
    /// `VT_EMPTY`.
    pub const EMPTY: u16 = 0;
    /// `VT_NULL`.
    pub const NULL: u16 = 1;
    /// `VT_I2`: 16-bit signed integer.
    pub const I2: u16 = 2;
    /// `VT_I4`: 32-bit signed integer (`long` in OPOS and VB).
    pub const I4: u16 = 3;
    /// `VT_R4`: 32-bit float.
    pub const R4: u16 = 4;
    /// `VT_R8`: 64-bit float.
    pub const R8: u16 = 5;
    /// `VT_CY`: currency (not converted).
    pub const CY: u16 = 6;
    /// `VT_DATE`: OLE automation date (not converted).
    pub const DATE: u16 = 7;
    /// `VT_BSTR`: string.
    pub const BSTR: u16 = 8;
    /// `VT_DISPATCH`: automation object.
    pub const DISPATCH: u16 = 9;
    /// `VT_ERROR`: an `SCODE`, also used for omitted optional arguments (not converted).
    pub const ERROR: u16 = 10;
    /// `VT_BOOL`: `VARIANT_TRUE` (-1) or `VARIANT_FALSE` (0).
    pub const BOOL: u16 = 11;
    /// `VT_VARIANT`: only valid together with [`BYREF`].
    pub const VARIANT: u16 = 12;
    /// `VT_UNKNOWN`: an `IUnknown` pointer.
    pub const UNKNOWN: u16 = 13;
    /// `VT_DECIMAL` (not converted).
    pub const DECIMAL: u16 = 14;
    /// `VT_I1`: 8-bit signed integer.
    pub const I1: u16 = 16;
    /// `VT_UI1`: byte.
    pub const UI1: u16 = 17;
    /// `VT_UI2`: 16-bit unsigned integer.
    pub const UI2: u16 = 18;
    /// `VT_UI4`: 32-bit unsigned integer.
    pub const UI4: u16 = 19;
    /// `VT_I8`: 64-bit signed integer.
    pub const I8: u16 = 20;
    /// `VT_UI8`: 64-bit unsigned integer.
    pub const UI8: u16 = 21;
    /// `VT_INT`: machine `int` (32-bit on Windows).
    pub const INT: u16 = 22;
    /// `VT_UINT`: machine `unsigned int` (32-bit on Windows).
    pub const UINT: u16 = 23;
    /// `VT_ARRAY` flag: a `SAFEARRAY` of the base type (not converted).
    pub const ARRAY: u16 = 0x2000;
    /// `VT_BYREF` flag: the variant points to a value of the base type.
    pub const BYREF: u16 = 0x4000;
}

/// Most member names an [`Object`] remembers the `DISPID` of.
const NAME_CACHE: usize = 64;

/// Joins the current thread to a single-threaded apartment (`CoInitializeEx` with
/// `COINIT_APARTMENTTHREADED`). Call it on every thread that uses COM, before
/// [`Object::create`].
///
/// A second call on the same thread while a guard or an object is alive returns a guard
/// for the same apartment. If the thread already belongs to the multithreaded apartment
/// (`RPC_E_CHANGED_MODE`) the guard is still returned, does not uninitialise anything, and
/// [`Apartment::is_multithreaded`] is true; objects then work through COM's proxies.
pub fn init() -> Result<Apartment, Error> {
    com_sys::init().map(|inner| Apartment { inner })
}

/// Keeps COM initialised on the current thread; `CoUninitialize` runs when this guard and
/// every [`Object`] created on the thread are dropped. Not `Send`.
pub struct Apartment {
    inner: com_sys::Init,
}

impl Apartment {
    /// Whether the thread was already in the multithreaded apartment when [`init`] ran
    /// (`RPC_E_CHANGED_MODE`), so this guard did not initialise COM itself.
    pub fn is_multithreaded(&self) -> bool {
        com_sys::is_multithreaded(&self.inner)
    }
}

impl fmt::Debug for Apartment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Apartment")
            .field("multithreaded", &self.is_multithreaded())
            .finish()
    }
}

/// How [`com_sys::invoke`] calls `IDispatch::Invoke`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InvokeKind {
    /// `DISPATCH_PROPERTYGET`.
    Get,
    /// `DISPATCH_PROPERTYPUT` with the named argument `DISPID_PROPERTYPUT`.
    Put,
    /// `DISPATCH_PROPERTYPUTREF` with the named argument `DISPID_PROPERTYPUT`.
    PutRef,
    /// `DISPATCH_METHOD | DISPATCH_PROPERTYGET`.
    Method,
}

/// An automation object: a counted `IDispatch` reference. `Clone` adds a reference and
/// `Drop` releases one. Two `Object`s are equal when they hold the same interface pointer.
/// Not `Send`: use it on the thread that created it.
pub struct Object {
    raw: com_sys::Dispatch,
    /// `(name, DISPID)` pairs already resolved with `GetIDsOfNames`.
    names: RefCell<Vec<(Box<str>, i32)>>,
}

impl Object {
    /// Creates an object from its ProgID, for example `SAPI.SpVoice` or
    /// `OPOS.POSPrinter`: `CLSIDFromProgID`, then `CoCreateInstance` with
    /// `CLSCTX_INPROC_SERVER | CLSCTX_LOCAL_SERVER` asking for `IDispatch`.
    ///
    /// Fails with [`hresult::CO_E_NOTINITIALIZED`] when [`init`] has not been called on
    /// this thread and with [`hresult::CO_E_CLASSSTRING`] when the ProgID is not
    /// registered.
    pub fn create(prog_id: &str) -> Result<Object, Error> {
        if prog_id.is_empty() {
            return Err(Error::InvalidInput("empty ProgID"));
        }
        let wide = to_wide(prog_id)?;
        com_sys::create(&wide, prog_id).map(Object::from_raw)
    }

    pub(crate) fn from_raw(raw: com_sys::Dispatch) -> Object {
        Object {
            raw,
            names: RefCell::new(Vec::new()),
        }
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn raw(&self) -> &com_sys::Dispatch {
        &self.raw
    }

    /// The `DISPID` of `name`, from the cache or `GetIDsOfNames`.
    fn dispid(&self, name: &str) -> Result<i32, Error> {
        if name.is_empty() {
            return Err(Error::InvalidInput("empty member name"));
        }
        let cached = self
            .names
            .borrow()
            .iter()
            .find(|(n, _)| &**n == name)
            .map(|(_, id)| *id);
        if let Some(id) = cached {
            return Ok(id);
        }
        let wide = to_wide(name)?;
        let id = com_sys::dispid(&self.raw, &wide, name)?;
        let mut names = self.names.borrow_mut();
        if names.len() < NAME_CACHE {
            names.push((name.into(), id));
        }
        Ok(id)
    }

    /// Reads property `name` (`DISPATCH_PROPERTYGET`).
    pub fn get(&self, name: &str) -> Result<Variant, Error> {
        let id = self.dispid(name)?;
        com_sys::invoke(&self.raw, id, name, InvokeKind::Get, &[])
    }

    /// Writes property `name` (`DISPATCH_PROPERTYPUT`), for example
    /// `printer.set("DeviceEnabled", true)`.
    pub fn set(&self, name: &str, value: impl Into<Variant>) -> Result<(), Error> {
        let id = self.dispid(name)?;
        com_sys::invoke(&self.raw, id, name, InvokeKind::Put, &[value.into()]).map(drop)
    }

    /// Assigns an object reference to property `name` (`DISPATCH_PROPERTYPUTREF`, VB's
    /// `Set obj.Name = value`), for example `voice.set_ref("Voice", &token)`.
    pub fn set_ref(&self, name: &str, value: &Object) -> Result<(), Error> {
        let id = self.dispid(name)?;
        let arg = [Variant::Object(value.clone())];
        com_sys::invoke(&self.raw, id, name, InvokeKind::PutRef, &arg).map(drop)
    }

    /// Calls method `name` with `args` in their declared order and returns its result
    /// ([`Variant::Empty`] for methods without one). Uses
    /// `DISPATCH_METHOD | DISPATCH_PROPERTYGET`, so parameterised properties such as
    /// `Item(i)` work too.
    pub fn call(&self, name: &str, args: &[Variant]) -> Result<Variant, Error> {
        let id = self.dispid(name)?;
        com_sys::invoke(&self.raw, id, name, InvokeKind::Method, args)
    }
}

impl Clone for Object {
    fn clone(&self) -> Object {
        Object {
            raw: self.raw.clone(),
            names: RefCell::new(self.names.borrow().clone()),
        }
    }
}

impl PartialEq for Object {
    fn eq(&self, other: &Object) -> bool {
        self.raw.addr() == other.raw.addr()
    }
}

impl Eq for Object {}

impl fmt::Debug for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Object({:#x})", self.raw.addr())
    }
}

/// An automation value (`VARIANT`).
///
/// Values sent to COM become `VT_EMPTY`, `VT_NULL`, `VT_BOOL`, `VT_I4`, `VT_I8`, `VT_R8`,
/// `VT_BSTR` or `VT_DISPATCH`. Results are converted back: `VT_I1`, `VT_UI1`, `VT_I2`,
/// `VT_UI2`, `VT_I4` and `VT_INT` become [`Variant::I32`]; `VT_UI4`, `VT_UINT`, `VT_I8` and
/// `VT_UI8` (up to `i64::MAX`) become [`Variant::I64`]; `VT_R4` and `VT_R8` become
/// [`Variant::F64`]; `VT_UNKNOWN` becomes an [`Object`] when it supports `IDispatch`; a
/// NULL object reference (VB's `Nothing`) becomes [`Variant::Null`]; `VT_BYREF` values are
/// read through the reference. Everything else is [`Variant::Unsupported`].
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Variant {
    /// `VT_EMPTY`: no value.
    #[default]
    Empty,
    /// `VT_NULL`: SQL-style null, or a NULL object reference in a result.
    Null,
    /// `VT_BOOL`.
    Bool(bool),
    /// `VT_I4` (results also from the smaller integer types).
    I32(i32),
    /// `VT_I8` (results also from `VT_UI4` and `VT_UI8`). Some older servers accept only
    /// `VT_I4`; pass [`Variant::I32`] to them.
    I64(i64),
    /// `VT_R8` (results also from `VT_R4`).
    F64(f64),
    /// `VT_BSTR`. May contain NUL characters; invalid UTF-16 in results becomes U+FFFD.
    String(String),
    /// `VT_DISPATCH`.
    Object(Object),
    /// A result of a type this module does not convert; carries its `VARTYPE` (see
    /// [`vt`]). Cannot be passed to COM.
    Unsupported(u16),
}

impl Variant {
    /// The `VARTYPE` this value is sent as (see [`vt`]).
    pub fn vt(&self) -> u16 {
        match self {
            Variant::Empty => vt::EMPTY,
            Variant::Null => vt::NULL,
            Variant::Bool(_) => vt::BOOL,
            Variant::I32(_) => vt::I4,
            Variant::I64(_) => vt::I8,
            Variant::F64(_) => vt::R8,
            Variant::String(_) => vt::BSTR,
            Variant::Object(_) => vt::DISPATCH,
            Variant::Unsupported(t) => *t,
        }
    }

    /// The string, if this is [`Variant::String`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Variant::String(s) => Some(s),
            _ => None,
        }
    }

    /// The object, if this is [`Variant::Object`].
    pub fn as_object(&self) -> Option<&Object> {
        match self {
            Variant::Object(o) => Some(o),
            _ => None,
        }
    }

    fn unexpected(&self, expected: &'static str) -> Error {
        Error::UnexpectedVariant {
            expected,
            vt: self.vt(),
        }
    }
}

impl From<bool> for Variant {
    fn from(v: bool) -> Variant {
        Variant::Bool(v)
    }
}

impl From<i32> for Variant {
    fn from(v: i32) -> Variant {
        Variant::I32(v)
    }
}

impl From<i64> for Variant {
    fn from(v: i64) -> Variant {
        Variant::I64(v)
    }
}

impl From<f64> for Variant {
    fn from(v: f64) -> Variant {
        Variant::F64(v)
    }
}

impl From<&str> for Variant {
    fn from(v: &str) -> Variant {
        Variant::String(v.into())
    }
}

impl From<String> for Variant {
    fn from(v: String) -> Variant {
        Variant::String(v)
    }
}

impl From<Object> for Variant {
    fn from(v: Object) -> Variant {
        Variant::Object(v)
    }
}

impl TryFrom<Variant> for bool {
    type Error = Error;

    /// Accepts [`Variant::Bool`].
    fn try_from(v: Variant) -> Result<bool, Error> {
        match v {
            Variant::Bool(b) => Ok(b),
            other => Err(other.unexpected("bool")),
        }
    }
}

impl TryFrom<Variant> for i32 {
    type Error = Error;

    /// Accepts [`Variant::I32`] and an [`Variant::I64`] that fits.
    fn try_from(v: Variant) -> Result<i32, Error> {
        match v {
            Variant::I32(n) => Ok(n),
            Variant::I64(n) => i32::try_from(n).map_err(|_| v.unexpected("i32")),
            other => Err(other.unexpected("i32")),
        }
    }
}

impl TryFrom<Variant> for i64 {
    type Error = Error;

    /// Accepts [`Variant::I32`] and [`Variant::I64`].
    fn try_from(v: Variant) -> Result<i64, Error> {
        match v {
            Variant::I32(n) => Ok(n.into()),
            Variant::I64(n) => Ok(n),
            other => Err(other.unexpected("i64")),
        }
    }
}

impl TryFrom<Variant> for f64 {
    type Error = Error;

    /// Accepts [`Variant::F64`] and [`Variant::I32`].
    fn try_from(v: Variant) -> Result<f64, Error> {
        match v {
            Variant::F64(n) => Ok(n),
            Variant::I32(n) => Ok(n.into()),
            other => Err(other.unexpected("f64")),
        }
    }
}

impl TryFrom<Variant> for String {
    type Error = Error;

    /// Accepts [`Variant::String`].
    fn try_from(v: Variant) -> Result<String, Error> {
        match v {
            Variant::String(s) => Ok(s),
            other => Err(other.unexpected("String")),
        }
    }
}

impl TryFrom<Variant> for Object {
    type Error = Error;

    /// Accepts [`Variant::Object`].
    fn try_from(v: Variant) -> Result<Object, Error> {
        match v {
            Variant::Object(o) => Ok(o),
            other => Err(other.unexpected("Object")),
        }
    }
}

/// The size in bytes of a non-pointer value of type `t` inside a `VARIANT`, or `None` for
/// types [`decode_scalar`] does not handle.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn scalar_size(t: u16) -> Option<usize> {
    match t {
        vt::I1 | vt::UI1 => Some(1),
        vt::I2 | vt::UI2 | vt::BOOL => Some(2),
        vt::I4 | vt::UI4 | vt::INT | vt::UINT | vt::R4 => Some(4),
        vt::I8 | vt::UI8 | vt::R8 => Some(8),
        _ => None,
    }
}

/// Converts a non-pointer value of type `t` whose bytes are `b` (little-endian, as on every
/// Windows target; only the first [`scalar_size`] bytes are used).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn decode_scalar(t: u16, b: [u8; 8]) -> Option<Variant> {
    let [b0, b1, b2, b3, ..] = b;
    let w = [b0, b1];
    let d = [b0, b1, b2, b3];
    Some(match t {
        vt::I1 => Variant::I32(i8::from_le_bytes([b0]).into()),
        vt::UI1 => Variant::I32(b0.into()),
        vt::I2 => Variant::I32(i16::from_le_bytes(w).into()),
        vt::UI2 => Variant::I32(u16::from_le_bytes(w).into()),
        vt::BOOL => Variant::Bool(i16::from_le_bytes(w) != 0),
        vt::I4 | vt::INT => Variant::I32(i32::from_le_bytes(d)),
        vt::UI4 | vt::UINT => Variant::I64(u32::from_le_bytes(d).into()),
        vt::R4 => Variant::F64(f32::from_le_bytes(d).into()),
        vt::I8 => Variant::I64(i64::from_le_bytes(b)),
        vt::UI8 => match i64::try_from(u64::from_le_bytes(b)) {
            Ok(n) => Variant::I64(n),
            Err(_) => Variant::Unsupported(vt::UI8),
        },
        vt::R8 => Variant::F64(f64::from_le_bytes(b)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le(v: &[u8]) -> [u8; 8] {
        let mut b = [0xAA; 8];
        b[..v.len()].copy_from_slice(v);
        b
    }

    #[test]
    fn scalars_decode() {
        let cases: &[(u16, &[u8], Variant)] = &[
            (vt::I1, &[0xFF], Variant::I32(-1)),
            (vt::UI1, &[200], Variant::I32(200)),
            (vt::I2, &(-5i16).to_le_bytes(), Variant::I32(-5)),
            (vt::UI2, &[0xFF, 0xFF], Variant::I32(65_535)),
            (vt::BOOL, &[0xFF, 0xFF], Variant::Bool(true)),
            (vt::BOOL, &[0, 0], Variant::Bool(false)),
            (vt::I4, &i32::MIN.to_le_bytes(), Variant::I32(i32::MIN)),
            (vt::INT, &7i32.to_le_bytes(), Variant::I32(7)),
            (
                vt::UI4,
                &u32::MAX.to_le_bytes(),
                Variant::I64(4_294_967_295),
            ),
            (vt::UINT, &1u32.to_le_bytes(), Variant::I64(1)),
            (vt::R4, &1.5f32.to_le_bytes(), Variant::F64(1.5)),
            (vt::I8, &i64::MIN.to_le_bytes(), Variant::I64(i64::MIN)),
            (
                vt::UI8,
                &(i64::MAX as u64).to_le_bytes(),
                Variant::I64(i64::MAX),
            ),
            (
                vt::UI8,
                &u64::MAX.to_le_bytes(),
                Variant::Unsupported(vt::UI8),
            ),
            (vt::R8, &(-0.25f64).to_le_bytes(), Variant::F64(-0.25)),
        ];
        for (t, bytes, want) in cases {
            assert_eq!(scalar_size(*t), Some(bytes.len()), "vt {t}");
            assert_eq!(decode_scalar(*t, le(bytes)).as_ref(), Some(want), "vt {t}");
        }
        for t in [
            vt::EMPTY,
            vt::NULL,
            vt::BSTR,
            vt::DISPATCH,
            vt::DATE,
            vt::CY,
            999,
        ] {
            assert_eq!(scalar_size(t), None);
            assert_eq!(decode_scalar(t, [0; 8]), None);
        }
    }

    #[test]
    fn conversions() {
        assert_eq!(Variant::from(true), Variant::Bool(true));
        assert_eq!(Variant::from(3), Variant::I32(3));
        assert_eq!(Variant::from(3i64), Variant::I64(3));
        assert_eq!(Variant::from(0.5), Variant::F64(0.5));
        assert_eq!(Variant::from("a"), Variant::String("a".into()));
        assert_eq!(Variant::from(String::from("b")).as_str(), Some("b"));
        assert_eq!(Variant::default(), Variant::Empty);
        assert_eq!(Variant::I32(1).as_str(), None);
        assert!(Variant::Null.as_object().is_none());

        assert_eq!(i32::try_from(Variant::I32(-1)), Ok(-1));
        assert_eq!(i32::try_from(Variant::I64(5)), Ok(5));
        assert_eq!(
            i32::try_from(Variant::I64(1 << 40)),
            Err(Error::UnexpectedVariant {
                expected: "i32",
                vt: vt::I8
            })
        );
        assert_eq!(i64::try_from(Variant::I32(-2)), Ok(-2));
        assert_eq!(f64::try_from(Variant::I32(2)), Ok(2.0));
        assert_eq!(bool::try_from(Variant::Bool(true)), Ok(true));
        assert_eq!(
            String::try_from(Variant::String("x".into())),
            Ok("x".to_string())
        );
        assert_eq!(
            bool::try_from(Variant::I32(1)),
            Err(Error::UnexpectedVariant {
                expected: "bool",
                vt: vt::I4
            })
        );
        assert_eq!(
            Object::try_from(Variant::Empty).unwrap_err(),
            Error::UnexpectedVariant {
                expected: "Object",
                vt: vt::EMPTY
            }
        );
        assert_eq!(
            String::try_from(Variant::Unsupported(vt::DATE)).unwrap_err(),
            Error::UnexpectedVariant {
                expected: "String",
                vt: vt::DATE
            }
        );
        let vts: Vec<u16> = [
            Variant::Empty,
            Variant::Null,
            Variant::Bool(false),
            Variant::I32(0),
            Variant::I64(0),
            Variant::F64(0.0),
            Variant::String(String::new()),
        ]
        .iter()
        .map(Variant::vt)
        .collect();
        assert_eq!(vts, [0, 1, 11, 3, 20, 5, 8]);
    }

    #[test]
    fn bad_names() {
        assert_eq!(
            Object::create("").unwrap_err(),
            Error::InvalidInput("empty ProgID")
        );
        assert!(matches!(
            Object::create("SAPI.SpVoice\0"),
            Err(Error::InvalidInput(_))
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_elsewhere() {
        assert_eq!(init().unwrap_err(), Error::Unsupported);
        assert_eq!(
            Object::create("SAPI.SpVoice").unwrap_err(),
            Error::Unsupported
        );
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn dictionary() -> Object {
        Object::create("Scripting.Dictionary").unwrap()
    }

    #[test]
    fn values_round_trip_through_a_dictionary() {
        let _apt = init().unwrap();
        let dict = dictionary();
        let inner = dictionary();
        let values = [
            Variant::Empty,
            Variant::Null,
            Variant::Bool(true),
            Variant::Bool(false),
            Variant::I32(i32::MIN),
            Variant::I64(i64::MAX),
            Variant::F64(-1.25e300),
            Variant::String(String::new()),
            Variant::String("Phở bò 🍜 a\0b".into()),
            Variant::Object(inner.clone()),
        ];
        for (i, v) in values.iter().enumerate() {
            dict.call("Add", &[Variant::I32(i as i32), v.clone()])
                .unwrap();
        }
        assert_eq!(
            dict.get("Count").unwrap(),
            Variant::I32(values.len() as i32)
        );
        for (i, v) in values.iter().enumerate() {
            let got = dict.call("Item", &[Variant::I32(i as i32)]).unwrap();
            assert_eq!(&got, v, "item {i}");
        }
        assert_eq!(
            dict.call("Exists", &[Variant::I32(3)]).unwrap(),
            Variant::Bool(true)
        );
        assert_eq!(
            dict.call("Exists", &["nope".into()]).unwrap(),
            Variant::Bool(false)
        );
        // A method without a result.
        assert_eq!(
            dict.call("Remove", &[Variant::I32(0)]).unwrap(),
            Variant::Empty
        );
        // Property put and get; CompareMode can only change while the dictionary is empty.
        dict.call("RemoveAll", &[]).unwrap();
        dict.set("CompareMode", 1).unwrap();
        assert_eq!(dict.get("CompareMode").unwrap(), Variant::I32(1));
        // TextCompare: keys now match case-insensitively.
        dict.call("Add", &["Key".into(), 1.into()]).unwrap();
        assert_eq!(
            dict.call("Exists", &["KEY".into()]).unwrap(),
            Variant::Bool(true)
        );
        // Names are case-insensitive in IDispatch; the cache keeps both spellings.
        assert_eq!(dict.get("count").unwrap(), Variant::I32(1));
        let copy = dict.clone();
        assert_eq!(copy, dict);
        assert_ne!(copy, inner);
        assert_eq!(copy.get("Count").unwrap(), Variant::I32(1));
    }

    #[test]
    fn errors_carry_hresult_and_detail() {
        let _apt = init().unwrap();
        let e = Object::create("DlWinos.NoSuchObject").unwrap_err();
        assert_eq!(e.hresult(), Some(hresult::CO_E_CLASSSTRING), "{e}");
        assert_eq!(e.operation(), Some("CLSIDFromProgID"));
        assert!(e.to_string().contains("DlWinos.NoSuchObject"), "{e}");

        let dict = dictionary();
        let e = dict.get("NoSuchMember").unwrap_err();
        assert_eq!(e.hresult(), Some(hresult::DISP_E_UNKNOWNNAME), "{e}");
        assert_eq!(e.operation(), Some("GetIDsOfNames"));
        assert!(e.to_string().contains("NoSuchMember"), "{e}");

        // The object raises an exception (DISP_E_EXCEPTION) whose scode is VB error 457,
        // "This key is already associated with an element of this collection". The
        // dictionary sets no description, so the detail is the member name.
        dict.call("Add", &["k".into(), 1.into()]).unwrap();
        let e = dict.call("Add", &["k".into(), 2.into()]).unwrap_err();
        assert_eq!(e.hresult(), Some(0x800A_01C9), "{e}");
        assert_eq!(e.operation(), Some("IDispatch::Invoke"));
        assert!(e.to_string().contains("`Add`"), "{e}");

        // WScript.Shell fills in EXCEPINFO's description and source.
        let shell = Object::create("WScript.Shell").unwrap();
        let e = shell
            .call("RegRead", &[r"HKCU\Software\dl-winos no such key\v".into()])
            .unwrap_err();
        assert_eq!(e.hresult(), Some(0x8007_0002), "{e}");
        assert!(e.to_string().contains("`RegRead`: "), "{e}");

        // Count is read-only.
        let e = dict.set("Count", 3).unwrap_err();
        assert!(e.hresult().is_some(), "{e}");
        assert!(e.to_string().contains("Count"), "{e}");

        // Wrong argument count.
        let e = dict.call("Exists", &[]).unwrap_err();
        assert!(e.hresult().is_some(), "{e}");

        assert_eq!(
            dict.call("Add", &[Variant::Unsupported(vt::DATE), 1.into()])
                .unwrap_err(),
            Error::InvalidInput("Variant::Unsupported cannot be passed to COM")
        );
        assert_eq!(
            dict.get("").unwrap_err(),
            Error::InvalidInput("empty member name")
        );
        assert!(matches!(dict.get("a\0"), Err(Error::InvalidInput(_))));
    }

    #[test]
    fn create_needs_init() {
        std::thread::spawn(|| {
            let e = Object::create("Scripting.Dictionary").unwrap_err();
            assert_eq!(e.hresult(), Some(hresult::CO_E_NOTINITIALIZED), "{e}");
            let apt = init().unwrap();
            assert!(!apt.is_multithreaded());
            let again = init().unwrap();
            let dict = dictionary();
            drop(apt);
            drop(again);
            // The object keeps the apartment alive.
            assert_eq!(dict.get("Count").unwrap(), Variant::I32(0));
            drop(dict);
            // Everything is gone: the next create fails again until init.
            let e = Object::create("Scripting.Dictionary").unwrap_err();
            assert_eq!(e.hresult(), Some(hresult::CO_E_NOTINITIALIZED), "{e}");
            let _apt = init().unwrap();
            dictionary();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn sapi_voice_speaks_to_a_wav_file() {
        let _apt = init().unwrap();
        let voice = Object::create("SAPI.SpVoice").unwrap();
        voice.set("Rate", 2).unwrap();
        assert_eq!(voice.get("Rate").unwrap(), Variant::I32(2));
        voice.set("Volume", 80).unwrap();
        assert_eq!(voice.get("Volume").unwrap(), Variant::I32(80));

        let voices: Object = voice.call("GetVoices", &[]).unwrap().try_into().unwrap();
        let count: i32 = voices.get("Count").unwrap().try_into().unwrap();
        assert!(count > 0);
        let token: Object = voices
            .call("Item", &[0.into()])
            .unwrap()
            .try_into()
            .unwrap();
        let name: String = token
            .call("GetDescription", &[])
            .unwrap()
            .try_into()
            .unwrap();
        assert!(!name.is_empty());
        voice.set_ref("Voice", &token).unwrap();
        let current: Object = voice.get("Voice").unwrap().try_into().unwrap();
        assert_eq!(
            current.call("GetDescription", &[]).unwrap().as_str(),
            Some(name.as_str())
        );

        let path = std::env::temp_dir().join(format!("dl-winos-sapi-{}.wav", std::process::id()));
        let path_text = path.to_string_lossy().into_owned();
        let stream = Object::create("SAPI.SpFileStream").unwrap();
        // SSFMCreateForWrite = 3.
        stream
            .call("Open", &[path_text.as_str().into(), 3.into(), false.into()])
            .unwrap();
        voice.set_ref("AudioOutputStream", &stream).unwrap();
        // SVSFDefault = 0: synchronous. Returns the stream number.
        let n = voice
            .call("Speak", &["Hello from dl winos".into(), 0.into()])
            .unwrap();
        assert!(matches!(n, Variant::I32(_)), "{n:?}");
        stream.call("Close", &[]).unwrap();
        drop((voice, stream));

        let wav = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(wav.len() > 44 + 1000, "{} bytes", wav.len());
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
    }
}
