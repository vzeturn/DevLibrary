//! COM declarations and the safe wrappers behind [`crate::com`]. With `sys_windows.rs`
//! the only file in the crate that contains `unsafe`.
//!
//! Ownership rules followed throughout: a [`Dispatch`] owns one reference (released on
//! drop); an [`OwnedVariant`] owns its contents (`VariantClear` on drop); decoding a
//! `VARIANT` into a [`Variant`] copies strings and adds a reference to objects, so the
//! source is always cleared by its owner afterwards.
#![allow(unsafe_code)]

use core::cell::RefCell;
use core::ffi::c_void;
use core::ptr::{self, NonNull};
use std::rc::{Rc, Weak};

use crate::Error;
use crate::com::{InvokeKind, Object, Variant, decode_scalar, hresult, scalar_size, vt};

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const IID_NULL: Guid = Guid {
    data1: 0,
    data2: 0,
    data3: 0,
    data4: [0; 8],
};
/// `{00020400-0000-0000-C000-000000000046}`.
const IID_IDISPATCH: Guid = Guid {
    data1: 0x0002_0400,
    data2: 0,
    data3: 0,
    data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
};

const COINIT_APARTMENTTHREADED: u32 = 0x2;
const CLSCTX_INPROC_SERVER: u32 = 0x1;
const CLSCTX_LOCAL_SERVER: u32 = 0x4;
const LOCALE_USER_DEFAULT: u32 = 0x0400;
const DISPATCH_METHOD: u16 = 0x1;
const DISPATCH_PROPERTYGET: u16 = 0x2;
const DISPATCH_PROPERTYPUT: u16 = 0x4;
const DISPATCH_PROPERTYPUTREF: u16 = 0x8;
const DISPID_PROPERTYPUT: i32 = -3;
const S_OK: i32 = 0;
const S_FALSE: i32 = 1;

/// The `VARIANT` value union: 8 bytes on 32-bit Windows, 16 on 64-bit (the `BRECORD`
/// pair of pointers), 8-byte aligned on both.
#[repr(C)]
#[derive(Clone, Copy)]
union VariantData {
    ll: i64,
    dbl: f64,
    ptr: *mut c_void,
    record: [*mut c_void; 2],
}

#[repr(C)]
struct RawVariant {
    vt: u16,
    reserved: [u16; 3],
    data: VariantData,
}

impl RawVariant {
    /// `VT_EMPTY` with every byte initialised (zero), so any union field can be read.
    fn empty() -> RawVariant {
        RawVariant {
            vt: vt::EMPTY,
            reserved: [0; 3],
            data: VariantData {
                record: [ptr::null_mut(); 2],
            },
        }
    }
}

/// A `VARIANT` this code owns; `VariantClear` frees its string or releases its object.
/// `repr(transparent)` so a `[OwnedVariant]` is a valid `VARIANTARG` array.
#[repr(transparent)]
struct OwnedVariant(RawVariant);

impl Drop for OwnedVariant {
    fn drop(&mut self) {
        // SAFETY: self.0 is a valid VARIANT (built by to_raw, zeroed, or filled in by COM)
        // that owns its contents; it is cleared exactly once.
        unsafe { VariantClear(&mut self.0) };
    }
}

#[repr(C)]
struct DispParams {
    args: *mut RawVariant,
    named_args: *mut i32,
    arg_count: u32,
    named_count: u32,
}

#[repr(C)]
struct ExcepInfo {
    code: u16,
    reserved: u16,
    source: *mut u16,
    description: *mut u16,
    help_file: *mut u16,
    help_context: u32,
    reserved_ptr: *mut c_void,
    deferred_fill_in: Option<unsafe extern "system" fn(*mut ExcepInfo) -> i32>,
    scode: i32,
}

/// An `EXCEPINFO` whose strings are freed on drop.
struct Excep(ExcepInfo);

impl Excep {
    fn new() -> Excep {
        Excep(ExcepInfo {
            code: 0,
            reserved: 0,
            source: ptr::null_mut(),
            description: ptr::null_mut(),
            help_file: ptr::null_mut(),
            help_context: 0,
            reserved_ptr: ptr::null_mut(),
            deferred_fill_in: None,
            scode: 0,
        })
    }
}

impl Drop for Excep {
    fn drop(&mut self) {
        for b in [self.0.source, self.0.description, self.0.help_file] {
            // SAFETY: each field is NULL (allowed) or a BSTR the callee allocated for us to
            // free; each is freed once.
            unsafe { SysFreeString(b) };
        }
    }
}

#[repr(C)]
struct IUnknownVtbl {
    query_interface: unsafe extern "system" fn(
        this: *mut c_void,
        iid: *const Guid,
        out: *mut *mut c_void,
    ) -> i32,
    add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    release: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

#[repr(C)]
struct IDispatchVtbl {
    unknown: IUnknownVtbl,
    get_type_info_count: *const c_void,
    get_type_info: *const c_void,
    get_ids_of_names: unsafe extern "system" fn(
        this: *mut c_void,
        iid: *const Guid,
        names: *const *const u16,
        count: u32,
        lcid: u32,
        ids: *mut i32,
    ) -> i32,
    invoke: unsafe extern "system" fn(
        this: *mut c_void,
        id: i32,
        iid: *const Guid,
        lcid: u32,
        flags: u16,
        params: *mut DispParams,
        result: *mut RawVariant,
        excep: *mut ExcepInfo,
        arg_err: *mut u32,
    ) -> i32,
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> i32;
    fn CoUninitialize();
    fn CLSIDFromProgID(prog_id: *const u16, clsid: *mut Guid) -> i32;
    fn CoCreateInstance(
        clsid: *const Guid,
        outer: *mut c_void,
        context: u32,
        iid: *const Guid,
        out: *mut *mut c_void,
    ) -> i32;
}

#[link(name = "oleaut32")]
unsafe extern "system" {
    fn SysAllocStringLen(s: *const u16, len: u32) -> *mut u16;
    fn SysFreeString(s: *mut u16);
    fn SysStringLen(s: *const u16) -> u32;
    fn VariantClear(v: *mut RawVariant) -> i32;
}

fn com_err(op: &'static str, hr: i32, detail: String) -> Error {
    Error::Com {
        op,
        hresult: hr as u32,
        detail,
    }
}

// ---------------------------------------------------------------------------------------
// Apartment
// ---------------------------------------------------------------------------------------

/// The per-thread initialisation. Lives in an `Rc`, so it is `!Send` and drops on the
/// thread that created it.
struct Guard {
    uninit: bool,
    multithreaded: bool,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.uninit {
            // SAFETY: balances the successful CoInitializeEx (S_OK or S_FALSE) made by
            // init() on this thread; the Rc keeps Guard on that thread.
            unsafe { CoUninitialize() };
        }
    }
}

thread_local! {
    static CURRENT: RefCell<Weak<Guard>> = const { RefCell::new(Weak::new()) };
}

/// A share of the current thread's COM initialisation.
#[derive(Clone)]
pub(crate) struct Init(Rc<Guard>);

fn current() -> Option<Init> {
    CURRENT
        .try_with(|c| c.borrow().upgrade())
        .ok()
        .flatten()
        .map(Init)
}

pub(crate) fn init() -> Result<Init, Error> {
    if let Some(i) = current() {
        return Ok(i);
    }
    // SAFETY: the reserved argument must be NULL and the flag is a valid COINIT value.
    let hr = unsafe { CoInitializeEx(ptr::null_mut(), COINIT_APARTMENTTHREADED) };
    let guard = match hr {
        S_OK | S_FALSE => Guard {
            uninit: true,
            multithreaded: false,
        },
        _ if hr as u32 == hresult::RPC_E_CHANGED_MODE => Guard {
            uninit: false,
            multithreaded: true,
        },
        _ => return Err(com_err("CoInitializeEx", hr, String::new())),
    };
    let rc = Rc::new(guard);
    // During thread teardown the slot may be gone; the guard then simply is not shared.
    let _ = CURRENT.try_with(|c| *c.borrow_mut() = Rc::downgrade(&rc));
    Ok(Init(rc))
}

pub(crate) fn is_multithreaded(i: &Init) -> bool {
    i.0.multithreaded
}

// ---------------------------------------------------------------------------------------
// Interface pointers
// ---------------------------------------------------------------------------------------

/// The `IUnknown` part of the vtable of interface `p`.
///
/// # Safety
/// `p` must be a live COM interface pointer.
unsafe fn unknown_vtbl<'a>(p: NonNull<c_void>) -> &'a IUnknownVtbl {
    // SAFETY: every COM interface pointer points to a pointer to its vtable, which starts
    // with the three IUnknown methods and outlives the object (caller's contract).
    unsafe { &**p.as_ptr().cast::<*const IUnknownVtbl>() }
}

/// Adds a reference to `p` and returns the new count (meaningful only for tests).
///
/// # Safety
/// `p` must be a live COM interface pointer.
unsafe fn add_ref(p: NonNull<c_void>) -> u32 {
    // SAFETY: p is live (caller's contract).
    let f = unsafe { unknown_vtbl(p) }.add_ref;
    // SAFETY: AddRef on a live interface.
    unsafe { f(p.as_ptr()) }
}

/// Releases one reference to `p` and returns the remaining count.
///
/// # Safety
/// `p` must be a live COM interface pointer and the caller must own the released
/// reference.
unsafe fn release(p: NonNull<c_void>) -> u32 {
    // SAFETY: p is live (caller's contract).
    let f = unsafe { unknown_vtbl(p) }.release;
    // SAFETY: the caller gives up the reference it owns.
    unsafe { f(p.as_ptr()) }
}

/// `QueryInterface(IID_IDispatch)`: a new reference, or `None`.
///
/// # Safety
/// `p` must be a live COM interface pointer.
unsafe fn query_dispatch(p: NonNull<c_void>) -> Option<NonNull<c_void>> {
    // SAFETY: p is live (caller's contract).
    let f = unsafe { unknown_vtbl(p) }.query_interface;
    let mut out: *mut c_void = ptr::null_mut();
    // SAFETY: QueryInterface on a live interface with a valid IID and output place.
    let hr = unsafe { f(p.as_ptr(), &IID_IDISPATCH, &mut out) };
    if hr < 0 { None } else { NonNull::new(out) }
}

/// One counted `IDispatch` reference plus a share of the apartment it lives in.
pub(crate) struct Dispatch {
    ptr: NonNull<c_void>,
    apartment: Init,
}

impl Dispatch {
    fn vtbl(&self) -> &IDispatchVtbl {
        // SAFETY: self.ptr is a live IDispatch (we own a reference), so it points to a
        // pointer to an IDispatch vtable that outlives the object.
        unsafe { &**self.ptr.as_ptr().cast::<*const IDispatchVtbl>() }
    }

    pub(crate) fn addr(&self) -> usize {
        self.ptr.as_ptr() as usize
    }

    /// The pointer with a new reference the caller owns.
    fn new_ref(&self) -> NonNull<c_void> {
        // SAFETY: self.ptr is live; the added reference is handed to the caller.
        unsafe { add_ref(self.ptr) };
        self.ptr
    }
}

impl Clone for Dispatch {
    fn clone(&self) -> Dispatch {
        Dispatch {
            ptr: self.new_ref(),
            apartment: self.apartment.clone(),
        }
    }
}

impl Drop for Dispatch {
    fn drop(&mut self) {
        // SAFETY: self.ptr is live and this value owns one reference, released once. The
        // apartment field drops afterwards, so COM is still initialised here.
        unsafe { release(self.ptr) };
    }
}

pub(crate) fn create(prog_id: &[u16], name: &str) -> Result<Dispatch, Error> {
    let Some(apartment) = current() else {
        return Err(Error::Com {
            op: "CoCreateInstance",
            hresult: hresult::CO_E_NOTINITIALIZED,
            detail: "call com::init() on this thread first".into(),
        });
    };
    let mut clsid = IID_NULL;
    // SAFETY: prog_id is NUL-terminated and outlives the call; clsid is a valid place.
    let hr = unsafe { CLSIDFromProgID(prog_id.as_ptr(), &mut clsid) };
    if hr < 0 {
        return Err(com_err("CLSIDFromProgID", hr, format!("ProgID `{name}`")));
    }
    let mut out: *mut c_void = ptr::null_mut();
    // SAFETY: clsid and the IID are valid GUIDs; NULL means no aggregation; out is a valid
    // place for the interface pointer.
    let hr = unsafe {
        CoCreateInstance(
            &clsid,
            ptr::null_mut(),
            CLSCTX_INPROC_SERVER | CLSCTX_LOCAL_SERVER,
            &IID_IDISPATCH,
            &mut out,
        )
    };
    if hr < 0 {
        return Err(com_err("CoCreateInstance", hr, format!("ProgID `{name}`")));
    }
    let Some(ptr) = NonNull::new(out) else {
        return Err(com_err(
            "CoCreateInstance",
            hresult::E_POINTER as i32,
            format!("ProgID `{name}`"),
        ));
    };
    Ok(Dispatch { ptr, apartment })
}

pub(crate) fn dispid(d: &Dispatch, wide: &[u16], name: &str) -> Result<i32, Error> {
    let names = [wide.as_ptr()];
    let mut id = 0i32;
    // SAFETY: d.ptr is a live IDispatch; names holds one NUL-terminated string that
    // outlives the call; id is a valid place for one DISPID.
    let hr = unsafe {
        (d.vtbl().get_ids_of_names)(
            d.ptr.as_ptr(),
            &IID_NULL,
            names.as_ptr(),
            1,
            LOCALE_USER_DEFAULT,
            &mut id,
        )
    };
    if hr < 0 {
        let detail = if hr as u32 == hresult::DISP_E_UNKNOWNNAME {
            format!("unknown member `{name}`")
        } else {
            format!("member `{name}`")
        };
        return Err(com_err("GetIDsOfNames", hr, detail));
    }
    Ok(id)
}

// ---------------------------------------------------------------------------------------
// VARIANT conversion
// ---------------------------------------------------------------------------------------

/// The string in BSTR `b`; NULL is the empty string.
fn bstr_to_string(b: *const u16) -> String {
    if b.is_null() {
        return String::new();
    }
    // SAFETY: b is a non-NULL BSTR (callers pass only BSTR fields or NULL), whose length
    // prefix SysStringLen reads.
    let len = unsafe { SysStringLen(b) } as usize;
    // SAFETY: a BSTR holds `len` initialised UTF-16 units at b.
    let units = unsafe { core::slice::from_raw_parts(b, len) };
    String::from_utf16_lossy(units)
}

fn to_raw(v: &Variant) -> Result<OwnedVariant, Error> {
    let mut r = RawVariant::empty();
    match v {
        Variant::Empty => {}
        Variant::Null => r.vt = vt::NULL,
        Variant::Bool(b) => {
            r.vt = vt::BOOL;
            // VARIANT_TRUE is -1 in the low 16 bits.
            r.data.ll = if *b { -1 } else { 0 };
        }
        Variant::I32(n) => {
            r.vt = vt::I4;
            r.data.ll = i64::from(*n);
        }
        Variant::I64(n) => {
            r.vt = vt::I8;
            r.data.ll = *n;
        }
        Variant::F64(x) => {
            r.vt = vt::R8;
            r.data.dbl = *x;
        }
        Variant::String(s) => {
            let units: Vec<u16> = s.encode_utf16().collect();
            let len = u32::try_from(units.len())
                .map_err(|_| Error::InvalidInput("string too long for a BSTR"))?;
            // SAFETY: units has `len` readable UTF-16 units; SysAllocStringLen copies them.
            let b = unsafe { SysAllocStringLen(units.as_ptr(), len) };
            if b.is_null() {
                return Err(com_err(
                    "SysAllocStringLen",
                    hresult::E_OUTOFMEMORY as i32,
                    String::new(),
                ));
            }
            r.vt = vt::BSTR;
            r.data.ptr = b.cast();
        }
        Variant::Object(o) => {
            r.vt = vt::DISPATCH;
            r.data.ptr = o.raw().new_ref().as_ptr();
        }
        Variant::Unsupported(_) => {
            return Err(Error::InvalidInput(
                "Variant::Unsupported cannot be passed to COM",
            ));
        }
    }
    Ok(OwnedVariant(r))
}

/// An [`Object`] for interface pointer `p`, adding a reference (the VARIANT keeps its own).
fn object_from(p: *mut c_void, t: u16, apartment: &Init) -> Variant {
    let Some(p) = NonNull::new(p) else {
        return Variant::Null;
    };
    let owned = if t == vt::DISPATCH {
        // SAFETY: a VT_DISPATCH VARIANT holds a live IDispatch pointer.
        unsafe { add_ref(p) };
        Some(p)
    } else {
        // SAFETY: a VT_UNKNOWN VARIANT holds a live IUnknown pointer.
        unsafe { query_dispatch(p) }
    };
    match owned {
        Some(ptr) => Variant::Object(Object::from_raw(Dispatch {
            ptr,
            apartment: apartment.clone(),
        })),
        None => Variant::Unsupported(t),
    }
}

/// Converts `v` without taking ownership: strings are copied and objects get their own
/// reference, so the caller still clears `v`.
fn decode(v: &RawVariant, apartment: &Init) -> Variant {
    let t = v.vt;
    if t & vt::BYREF == 0 {
        match t {
            vt::EMPTY => return Variant::Empty,
            vt::NULL => return Variant::Null,
            _ => {}
        }
    }
    let base = t & !vt::BYREF;
    let src: *const u8 = if t & vt::BYREF != 0 {
        // SAFETY: every byte of the union is initialised (RawVariant::empty or COM).
        let p = unsafe { v.data.ptr };
        if p.is_null() {
            return Variant::Unsupported(t);
        }
        if base == vt::VARIANT {
            // SAFETY: VT_BYREF | VT_VARIANT points to a VARIANT its owner keeps valid.
            let inner = unsafe { &*p.cast::<RawVariant>() };
            // A reference to another reference is not valid automation data.
            if inner.vt & vt::BYREF != 0 {
                return Variant::Unsupported(t);
            }
            return decode(inner, apartment);
        }
        p.cast()
    } else {
        (&raw const v.data).cast()
    };
    match base {
        vt::BSTR | vt::DISPATCH | vt::UNKNOWN => {
            // SAFETY: src points to the union (at least pointer-sized) or, for VT_BYREF, to
            // a BSTR / interface pointer its owner keeps valid; read_unaligned has no
            // alignment requirement.
            let p = unsafe { ptr::read_unaligned(src.cast::<*mut c_void>()) };
            if base == vt::BSTR {
                Variant::String(bstr_to_string(p.cast()))
            } else {
                object_from(p, base, apartment)
            }
        }
        _ => {
            let Some(size) = scalar_size(base) else {
                return Variant::Unsupported(t);
            };
            let mut b = [0u8; 8];
            // SAFETY: src points to the 8-byte union or, for VT_BYREF, to a value of type
            // `base`, which is `size` (at most 8) bytes; b has room for 8.
            unsafe { ptr::copy_nonoverlapping(src, b.as_mut_ptr(), size) };
            decode_scalar(base, b).unwrap_or(Variant::Unsupported(t))
        }
    }
}

// ---------------------------------------------------------------------------------------
// Invoke
// ---------------------------------------------------------------------------------------

pub(crate) fn invoke(
    d: &Dispatch,
    id: i32,
    name: &str,
    kind: InvokeKind,
    args: &[Variant],
) -> Result<Variant, Error> {
    // IDispatch::Invoke takes the arguments last to first.
    let mut raw = Vec::with_capacity(args.len());
    for a in args.iter().rev() {
        raw.push(to_raw(a)?);
    }
    let arg_count =
        u32::try_from(raw.len()).map_err(|_| Error::InvalidInput("too many arguments"))?;
    let (flags, named) = match kind {
        InvokeKind::Get => (DISPATCH_PROPERTYGET, false),
        InvokeKind::Put => (DISPATCH_PROPERTYPUT, true),
        InvokeKind::PutRef => (DISPATCH_PROPERTYPUTREF, true),
        InvokeKind::Method => (DISPATCH_METHOD | DISPATCH_PROPERTYGET, false),
    };
    let mut put_id = DISPID_PROPERTYPUT;
    let mut params = DispParams {
        args: if raw.is_empty() {
            ptr::null_mut()
        } else {
            raw.as_mut_ptr().cast::<RawVariant>()
        },
        named_args: if named { &mut put_id } else { ptr::null_mut() },
        arg_count,
        named_count: u32::from(named),
    };
    let mut result = OwnedVariant(RawVariant::empty());
    let result_ptr: *mut RawVariant = if named {
        ptr::null_mut()
    } else {
        &mut result.0
    };
    let mut excep = Excep::new();
    let mut arg_err = u32::MAX;
    // SAFETY: d.ptr is a live IDispatch; params points to `arg_count` initialised VARIANTs
    // (OwnedVariant is repr(transparent)) and the named DISPID, all outliving the call;
    // result, excep and arg_err are valid places (result NULL for puts, as allowed).
    let hr = unsafe {
        (d.vtbl().invoke)(
            d.ptr.as_ptr(),
            id,
            &IID_NULL,
            LOCALE_USER_DEFAULT,
            flags,
            &mut params,
            result_ptr,
            &mut excep.0,
            &mut arg_err,
        )
    };
    // The callee may have changed the arguments in place; dropping `raw` clears whatever
    // they now hold.
    drop(raw);
    if hr < 0 {
        return Err(invoke_error(hr, name, &mut excep, arg_err, args.len()));
    }
    Ok(decode(&result.0, &d.apartment))
}

fn invoke_error(hr: i32, name: &str, excep: &mut Excep, arg_err: u32, count: usize) -> Error {
    if hr as u32 == hresult::DISP_E_EXCEPTION {
        if let Some(fill) = excep.0.deferred_fill_in {
            // SAFETY: the object supplied this callback to fill in this EXCEPINFO, as the
            // IDispatch::Invoke contract describes.
            unsafe { fill(&mut excep.0) };
        }
        let description = bstr_to_string(excep.0.description);
        let source = bstr_to_string(excep.0.source);
        let mut detail = format!("`{name}`");
        if !description.is_empty() {
            detail.push_str(": ");
            detail.push_str(description.trim_end());
        }
        if !source.is_empty() {
            detail.push_str(&format!(" (source: {source})"));
        }
        if excep.0.code != 0 {
            detail.push_str(&format!(" (code {})", excep.0.code));
        }
        let code = if excep.0.scode != 0 {
            excep.0.scode
        } else {
            hr
        };
        return com_err("IDispatch::Invoke", code, detail);
    }
    let positional = matches!(
        hr as u32,
        hresult::DISP_E_TYPEMISMATCH | hresult::DISP_E_PARAMNOTFOUND
    );
    let detail = match usize::try_from(arg_err) {
        // arg_err indexes the reversed array; report the 1-based position from the left.
        Ok(i) if positional && i < count => format!("`{name}`, argument {}", count - i),
        _ => format!("`{name}`"),
    };
    com_err("IDispatch::Invoke", hr, detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::{self, Object};
    use core::mem::size_of;

    #[test]
    fn layouts() {
        let p = size_of::<usize>();
        assert_eq!(size_of::<RawVariant>(), if p == 8 { 24 } else { 16 });
        assert_eq!(core::mem::align_of::<RawVariant>(), 8);
        assert_eq!(size_of::<OwnedVariant>(), size_of::<RawVariant>());
        assert_eq!(size_of::<DispParams>(), 2 * p + 8);
        assert_eq!(size_of::<ExcepInfo>(), if p == 8 { 64 } else { 32 });
        assert_eq!(size_of::<IDispatchVtbl>(), 7 * p);
    }

    fn scalar(t: u16, ll: i64) -> RawVariant {
        let mut r = RawVariant::empty();
        r.vt = t;
        r.data.ll = ll;
        r
    }

    #[test]
    fn decodes_raw_variants() {
        let apt = com::init().unwrap();
        let i = &apt_init(&apt);
        assert_eq!(decode(&RawVariant::empty(), i), Variant::Empty);
        assert_eq!(decode(&scalar(vt::NULL, 0), i), Variant::Null);
        assert_eq!(decode(&scalar(vt::I2, -5), i), Variant::I32(-5));
        assert_eq!(decode(&scalar(vt::UI1, 200), i), Variant::I32(200));
        assert_eq!(
            decode(&scalar(vt::UI4, 0xFFFF_FFFF), i),
            Variant::I64(4_294_967_295)
        );
        assert_eq!(
            decode(&scalar(vt::R4, i64::from(1.5f32.to_bits())), i),
            Variant::F64(1.5)
        );
        assert_eq!(decode(&scalar(vt::BOOL, 0xFFFF), i), Variant::Bool(true));
        assert_eq!(
            decode(&scalar(vt::DATE, 0), i),
            Variant::Unsupported(vt::DATE)
        );
        assert_eq!(
            decode(&scalar(vt::ARRAY | vt::I4, 0), i),
            Variant::Unsupported(vt::ARRAY | vt::I4)
        );
        assert_eq!(
            decode(&scalar(vt::BYREF | vt::EMPTY, 0), i),
            Variant::Unsupported(vt::BYREF | vt::EMPTY)
        );
        assert_eq!(
            decode(&scalar(vt::DISPATCH, 0), i),
            Variant::Null,
            "NULL object"
        );

        // VT_BYREF: read through the pointer.
        let mut n: i16 = -300;
        let mut r = RawVariant::empty();
        r.vt = vt::BYREF | vt::I2;
        r.data.ptr = (&raw mut n).cast();
        assert_eq!(decode(&r, i), Variant::I32(-300));
        r.data.ptr = ptr::null_mut();
        assert_eq!(decode(&r, i), Variant::Unsupported(vt::BYREF | vt::I2));

        let s = to_raw(&Variant::String("byref đ".into())).unwrap();
        let mut b: *mut c_void = {
            // SAFETY: s is a VT_BSTR built by to_raw; reading its pointer member.
            unsafe { s.0.data.ptr }
        };
        r.vt = vt::BYREF | vt::BSTR;
        r.data.ptr = (&raw mut b).cast();
        assert_eq!(decode(&r, i), Variant::String("byref đ".into()));

        let mut inner = scalar(vt::I4, 42);
        r.vt = vt::BYREF | vt::VARIANT;
        r.data.ptr = (&raw mut inner).cast();
        assert_eq!(decode(&r, i), Variant::I32(42));
        let mut nested = RawVariant::empty();
        nested.vt = vt::BYREF | vt::VARIANT;
        nested.data.ptr = (&raw mut inner).cast();
        r.data.ptr = (&raw mut nested).cast();
        assert_eq!(decode(&r, i), Variant::Unsupported(vt::BYREF | vt::VARIANT));
        drop(s);
    }

    fn apt_init(apt: &com::Apartment) -> Init {
        assert!(!apt.is_multithreaded());
        current().unwrap()
    }

    #[test]
    fn to_raw_round_trips() {
        let apt = com::init().unwrap();
        let i = &apt_init(&apt);
        for v in [
            Variant::Empty,
            Variant::Null,
            Variant::Bool(true),
            Variant::Bool(false),
            Variant::I32(-7),
            Variant::I64(i64::MIN),
            Variant::F64(f64::MAX),
            Variant::String(String::new()),
            Variant::String("Tiếng Việt\0🙂".into()),
        ] {
            let r = to_raw(&v).unwrap();
            assert_eq!(r.0.vt, v.vt());
            assert_eq!(decode(&r.0, i), v);
        }
        let r = to_raw(&Variant::Bool(true)).unwrap();
        // SAFETY: reading an initialised union member.
        let low = unsafe { r.0.data.ll } as i16;
        assert_eq!(low, -1, "VARIANT_TRUE");
    }

    fn ref_count(o: &Object) -> u32 {
        let p = o.raw().ptr;
        // SAFETY: o holds a live reference; the added reference is released right away.
        unsafe { add_ref(p) };
        // SAFETY: releases the reference added above.
        unsafe { release(p) }
    }

    #[test]
    fn reference_counts_balance() {
        let _apt = com::init().unwrap();
        let outer = Object::create("Scripting.Dictionary").unwrap();
        let inner = Object::create("Scripting.Dictionary").unwrap();
        let base = ref_count(&inner);
        let copy = inner.clone();
        assert_eq!(ref_count(&inner), base + 1);
        drop(copy);
        assert_eq!(ref_count(&inner), base);

        for k in 0..200 {
            outer
                .call("Add", &[k.into(), Variant::Object(inner.clone())])
                .unwrap();
        }
        // The dictionary holds one reference per entry; our temporary clones are gone.
        assert_eq!(ref_count(&inner), base + 200);
        for k in 0..200 {
            let got = outer.call("Item", &[k.into()]).unwrap();
            assert_eq!(got.as_object(), Some(&inner));
            assert_eq!(ref_count(&inner), base + 201);
        }
        assert_eq!(ref_count(&inner), base + 200);
        // set_ref and failed calls must not leak either.
        let _ = outer.set_ref("NoSuchMember", &inner);
        let _ = outer.call("Add", &[0.into(), Variant::Object(inner.clone())]);
        assert_eq!(ref_count(&inner), base + 200);
        outer.call("RemoveAll", &[]).unwrap();
        assert_eq!(ref_count(&inner), base);
    }

    #[test]
    fn many_strings_and_objects() {
        let _apt = com::init().unwrap();
        let dict = Object::create("Scripting.Dictionary").unwrap();
        let text = "x".repeat(10_000);
        for k in 0..2_000 {
            dict.call("Add", &[k.into(), text.as_str().into()]).unwrap();
            let got = dict.call("Item", &[k.into()]).unwrap();
            assert_eq!(got.as_str().map(str::len), Some(10_000));
            dict.call("Remove", &[k.into()]).unwrap();
        }
        assert_eq!(dict.get("Count").unwrap(), Variant::I32(0));
    }

    /// Runs `f` on a new thread that COM was initialised on with `coinit` beforehand.
    fn on_thread_with(coinit: u32, f: impl FnOnce() + Send + 'static) {
        std::thread::spawn(move || {
            // SAFETY: reserved is NULL; coinit is a valid COINIT value.
            let hr = unsafe { CoInitializeEx(ptr::null_mut(), coinit) };
            assert_eq!(hr, S_OK);
            f();
            // SAFETY: balances the CoInitializeEx above on the same thread.
            unsafe { CoUninitialize() };
        })
        .join()
        .unwrap();
    }

    #[test]
    fn already_multithreaded() {
        const COINIT_MULTITHREADED: u32 = 0;
        on_thread_with(COINIT_MULTITHREADED, || {
            let apt = com::init().unwrap();
            assert!(apt.is_multithreaded());
            let dict = Object::create("Scripting.Dictionary").unwrap();
            dict.call("Add", &["a".into(), 1.into()]).unwrap();
            assert_eq!(dict.get("Count").unwrap(), Variant::I32(1));
        });
    }

    #[test]
    fn already_single_threaded() {
        on_thread_with(COINIT_APARTMENTTHREADED, || {
            let apt = com::init().unwrap();
            assert!(!apt.is_multithreaded());
            let dict = Object::create("Scripting.Dictionary").unwrap();
            assert_eq!(dict.get("Count").unwrap(), Variant::I32(0));
            drop(dict);
            drop(apt);
            // Our S_FALSE initialisation is balanced; the outer one still holds.
            let mut clsid = IID_NULL;
            let w = crate::wide::to_wide("Scripting.Dictionary").unwrap();
            let mut out: *mut c_void = ptr::null_mut();
            // SAFETY: w is NUL-terminated; clsid is a valid place.
            let hr = unsafe { CLSIDFromProgID(w.as_ptr(), &mut clsid) };
            assert_eq!(hr, S_OK);
            // SAFETY: as in create(); the reference is released below.
            let hr = unsafe {
                CoCreateInstance(
                    &clsid,
                    ptr::null_mut(),
                    CLSCTX_INPROC_SERVER,
                    &IID_IDISPATCH,
                    &mut out,
                )
            };
            assert_eq!(hr, S_OK, "COM still initialised");
            // SAFETY: out is the live reference CoCreateInstance returned to us.
            unsafe { release(NonNull::new(out).unwrap()) };
        });
    }
}
