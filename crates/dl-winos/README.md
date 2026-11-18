# dl-winos

Windows system access without dependencies: read-only registry access (keys, typed values,
enumeration, WOW64 views), RAW print jobs through the print spooler (printer list,
default printer, `WritePrinter`), late-bound COM automation through `IDispatch` (SAPI
text-to-speech, OPOS printers, cash drawers, line displays and other automation objects)
and dynamic loading of vendor SDK DLLs (typed exports, clear errors for missing DLLs,
missing symbols and 32/64-bit mismatches, ANSI string buffers), plus console echo control
for password prompts. On Linux, macOS and
WebAssembly the same API compiles and every call returns `Error::Unsupported`, so one
code base can choose at run time; only the PE header reader `Library::machine` works
everywhere.

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| Go        | `golang.org/x/sys/windows/registry`, hand-written `winspool.drv` calls (`OpenPrinterW`, `WritePrinter`), `github.com/go-ole/go-ole` + `oleutil`, `syscall.LoadDLL` / `syscall.NewLazyDLL` / `windows.LoadDLL` (`FindProc`, `Call`), `debug/pe` (for the machine type) |
| .NET      | `Microsoft.Win32.Registry`, `RawPrinterHelper` P/Invoke samples, `System.Drawing.Printing.PrinterSettings.InstalledPrinters`, `Type.GetTypeFromProgID` + `Activator.CreateInstance` + `dynamic`, `Interop.SpeechLib` / `System.Speech`, OPOS interop assemblies (`Interop.OposPOSPrinter_CCO` and friends), `[DllImport]` / `[LibraryImport]` with `CharSet.Ansi` and `ThrowOnUnmappableChar`, `System.Runtime.InteropServices.NativeLibrary` (`TryLoad`, `GetExport`) + `Marshal.GetDelegateForFunctionPointer`, `PEReader` / `PEHeaders.CoffHeader.Machine` |
| Python    | `pywin32` (`win32com.client.Dispatch`), `comtypes.client.CreateObject`, `pyttsx3` (SAPI driver), `ctypes.WinDLL` / `ctypes.windll` |
| Rust      | `winreg`, `windows-sys` (`Win32_System_Registry`, `Win32_Graphics_Printing`, `Win32_System_LibraryLoader`), `printers`, `windows` (`Win32_System_Com`, `Win32_System_Ole`, `Win32_System_Variant`), `winapi` (`oaidl`, `combaseapi`), `libloading`, `goblin` / `object` (for the machine type) |

## Registry

```rust,no_run
use dl_winos::registry::{self, Hive, Key, Value, View};
use dl_winos::codes;

let k = Key::open(
    Hive::LocalMachine,
    r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
    View::Default,
)?;
let product = k.string("ProductName")?.unwrap_or_default();
let build = k.string("CurrentBuildNumber")?.unwrap_or_default();
println!("{product} build {build}");

// A missing key is error code 2; a missing value is Ok(None).
match Key::open(Hive::LocalMachine, r"SOFTWARE\My Vendor\My App", View::Wow64_32) {
    Ok(app) => {
        if let Some(Value::ExpandString(raw)) = app.value("DataDir")? {
            println!("data in {}", registry::expand(&raw)?);
        }
        for (name, value) in app.values()? {
            println!("{name} = {value:?}");
        }
    }
    Err(e) if e.code() == Some(codes::FILE_NOT_FOUND) => println!("not installed"),
    Err(e) => return Err(e),
}

let printers = Key::open(
    Hive::LocalMachine,
    r"SYSTEM\CurrentControlSet\Control\Print\Printers",
    View::Default,
)?;
for name in printers.subkeys()? {
    let port = printers.open_subkey(&name)?.string("Port")?;
    println!("{name}: {port:?}");
}
# Ok::<(), dl_winos::Error>(())
```

Keys are opened with `KEY_READ` only. `Value` covers `REG_SZ`, `REG_EXPAND_SZ` (as stored;
`registry::expand` resolves `%VAR%` with `ExpandEnvironmentStringsW`), `REG_MULTI_SZ`,
`REG_DWORD`, `REG_QWORD` and `REG_BINARY`; every other type comes back as
`Value::Other { kind, data }`. `View::Wow64_64` / `View::Wow64_32` pick the 64- or 32-bit
`SOFTWARE` view regardless of the process bitness.

Registry data is decoded defensively: strings without a NUL terminator, odd byte lengths
(the trailing byte is ignored) and unpaired surrogates (U+FFFD) are accepted, a
`REG_DWORD` / `REG_QWORD` whose data is not exactly 4 / 8 bytes is returned as
`Value::Other`, and `Value::from_raw` exposes the same decoder for data from other sources.

## RAW printing

```rust,no_run
use dl_winos::{codes, spooler};

for p in spooler::printers()? {
    println!("{} on {} ({}){}", p.name, p.port, p.driver, if p.is_default { " [default]" } else { "" });
}

// ESC/POS: initialise, text, feed, cut.
let receipt = b"\x1b@Table 12\nPho bo        65.000\n\n\n\x1dV\x00";
match spooler::write_raw("POS-80", "receipt 1042", receipt) {
    Ok(job) => println!("spooled as job {job}"),
    Err(e) if e.code() == Some(codes::INVALID_PRINTER_NAME) => println!("no printer POS-80"),
    Err(e) => return Err(e),
}
# Ok::<(), dl_winos::Error>(())
```

`write_raw` runs `OpenPrinterW`, `StartDocPrinterW` (level 1, datatype `RAW`),
`StartPagePrinter`, `WritePrinter` until every byte is written, `EndPagePrinter`,
`EndDocPrinter` and `ClosePrinter`, and returns the job id. The bytes reach the port
unchanged, so this is the call for ESC/POS receipt printers, ZPL/EPL label printers, PCL
and PostScript. If a step after `StartDocPrinterW` fails the job is cancelled with
`AbortPrinter`; errors carry the failing function and its `GetLastError` code.

`printers()` lists local printers and the current user's connections
(`EnumPrintersW` level 2); `is_default` compares with `GetDefaultPrinterW`, which
`default_printer()` also returns (`Ok(None)` when no default is set).

## COM automation

`com` drives automation objects late-bound through `IDispatch`, like VBScript or C#
`dynamic`: create an object by ProgID, then `get` and `set` properties and `call` methods
by name.

```rust,no_run
use dl_winos::com::{self, Object, Variant};

let _apartment = com::init()?; // once per thread, before the first object
let voice = Object::create("SAPI.SpVoice")?;
voice.set("Rate", 2)?;
voice.set("Volume", 80)?;

// Pick a voice: GetVoices() returns a collection, Item(i) a voice token.
let voices: Object = voice.call("GetVoices", &[])?.try_into()?;
let count: i32 = voices.get("Count")?.try_into()?;
for i in 0..count {
    let token: Object = voices.call("Item", &[i.into()])?.try_into()?;
    if token.call("GetDescription", &[])?.as_str().is_some_and(|d| d.contains("Zira")) {
        voice.set_ref("Voice", &token)?; // VB: Set voice.Voice = token
    }
}
// Flags 0 = synchronous; 1 (SVSFlagsAsync) returns at once.
voice.call("Speak", &["Order 1042 is ready".into(), Variant::I32(0)])?;
# Ok::<(), dl_winos::Error>(())
```

OPOS control objects (`OPOS.POSPrinter`, `OPOS.CashDrawer`, `OPOS.LineDisplay`,
`OPOS.MSR`, `OPOS.ToneIndicator`, ...) work the same way; their methods return an OPOS
`ResultCode`:

```rust,no_run
use dl_winos::com::{self, Object, Variant};

const OPOS_SUCCESS: i32 = 0;
const PTR_S_RECEIPT: i32 = 2;

fn opos(obj: &Object, method: &str, args: &[Variant]) -> Result<(), Box<dyn std::error::Error>> {
    let rc: i32 = obj.call(method, args)?.try_into()?;
    if rc != OPOS_SUCCESS {
        let extended = obj.get("ResultCodeExtended")?;
        return Err(format!("{method} returned {rc} (extended {extended:?})").into());
    }
    Ok(())
}

let _apartment = com::init()?;
let printer = Object::create("OPOS.POSPrinter")?;
opos(&printer, "Open", &["POSPrinter1".into()])?; // logical device name from the OPOS setup
opos(&printer, "ClaimDevice", &[1000.into()])?;
printer.set("DeviceEnabled", true)?;
opos(&printer, "PrintNormal", &[PTR_S_RECEIPT.into(), "Table 12\nPho bo    65.000\n".into()])?;
opos(&printer, "CutPaper", &[90.into()])?;
printer.set("DeviceEnabled", false)?;
opos(&printer, "ReleaseDevice", &[])?;
opos(&printer, "Close", &[])?;

let drawer = Object::create("OPOS.CashDrawer")?;
opos(&drawer, "Open", &["CashDrawer1".into()])?;
opos(&drawer, "ClaimDevice", &[1000.into()])?;
drawer.set("DeviceEnabled", true)?;
opos(&drawer, "OpenDrawer", &[])?;
let open: bool = drawer.get("DrawerOpened")?.try_into()?;
println!("drawer open: {open}");
# Ok::<(), Box<dyn std::error::Error>>(())
```

The line display takes `DisplayText(text, 0)`, the tone indicator `Sound(cycles, wait_ms)`.

**Threads.** `com::init()` joins the calling thread to a single-threaded apartment
(`CoInitializeEx(COINIT_APARTMENTTHREADED)`) and returns a guard. `Apartment`, `Object`
and `Variant` are not `Send`: objects belong to the thread that created them. Run COM on
one dedicated thread and send it work:

```rust,no_run
use std::sync::mpsc;
use std::thread;
use dl_winos::com::{self, Object};

let (tx, rx) = mpsc::channel::<String>();
let speaker = thread::spawn(move || -> Result<(), dl_winos::Error> {
    let _apartment = com::init()?;
    let voice = Object::create("SAPI.SpVoice")?;
    for text in rx {
        voice.call("Speak", &[text.into(), 0.into()])?;
    }
    Ok(())
});
tx.send("Order 1042 is ready".into()).expect("speaker thread is running");
drop(tx);
speaker.join().expect("speaker thread panicked")?;
# Ok::<(), dl_winos::Error>(())
```

Each object keeps the apartment initialised, so `CoUninitialize` runs only after the
guard and the last object are dropped. A thread that is already initialised keeps its
mode: after its own STA init `com::init()` balances with `CoUninitialize`; in the
multithreaded apartment (`RPC_E_CHANGED_MODE`) the guard does nothing and
`Apartment::is_multithreaded()` is true. `Object::create` before `com::init()` fails with
`CO_E_NOTINITIALIZED`.

**Calls.** `Object::create` runs `CLSIDFromProgID` and `CoCreateInstance`
(`CLSCTX_INPROC_SERVER | CLSCTX_LOCAL_SERVER`, `IID_IDispatch`). Member names are
resolved with `GetIDsOfNames` and cached per object. `get` uses `DISPATCH_PROPERTYGET`,
`set` / `set_ref` use `DISPATCH_PROPERTYPUT` / `DISPATCH_PROPERTYPUTREF` with the named
argument `DISPID_PROPERTYPUT`, and `call` uses `DISPATCH_METHOD | DISPATCH_PROPERTYGET`
(so parameterised properties such as `Item(i)` work); arguments are given in their
declared order and passed to `Invoke` in reverse, as it requires.

**Values.** `Variant` is `Empty`, `Null`, `Bool`, `I32`, `I64`, `F64`, `String` (a
`BSTR`) or `Object` (`VT_DISPATCH`). Results of the other integer and float types,
`VT_UNKNOWN` objects that support `IDispatch` and `VT_BYREF` values are converted to
these; anything else is `Variant::Unsupported(vt)`. `TryFrom<Variant>` converts to
`bool`, `i32`, `i64`, `f64`, `String` and `Object` (`Error::UnexpectedVariant`
otherwise). Every `VARIANT` is cleared with `VariantClear`; objects are counted with
`AddRef` / `Release`, and the tests check the counts return to their start values.

**Errors** are `Error::Com { op, hresult, detail }`; `com::hresult` has the common
codes (`CO_E_CLASSSTRING` for an unknown ProgID, `DISP_E_UNKNOWNNAME` for an unknown
member). When the object raises an exception the `hresult` is its `scode` and `detail`
holds the member, description and source from `EXCEPINFO`.

## Dynamic libraries

`dll` loads vendor SDK DLLs that may or may not be installed, such as a hotel lock
encoder's 32-bit `__stdcall` `LockSDK.dll` with ANSI strings, and reports a missing DLL,
a missing export or the wrong bitness as an error instead of crashing.

```rust,no_run
use dl_winos::codes;
use dl_winos::dll::{Library, Machine, Search, ansi};

// int __stdcall EncodeCard(const char* room, const char* checkout, char* card_out, int len);
type EncodeCard = unsafe extern "system" fn(*const u8, *const u8, *mut u8, i32) -> i32;

let path = r"C:\Program Files (x86)\Lock Vendor\SDK\LockSDK.dll";
// Check the bitness first: a 32-bit SDK needs an i686-pc-windows-msvc build.
match Library::machine(path) {
    Ok(m) if m != Machine::current() => {
        println!("{path} is {m}, this agent is {}", Machine::current());
        return Ok(());
    }
    Ok(_) => {}
    Err(e) => {
        println!("lock SDK not installed: {e}");
        return Ok(());
    }
}
// DllDir: helper DLLs next to LockSDK.dll are found too.
let sdk = match Library::open(path, Search::DllDir) {
    Ok(sdk) => sdk,
    Err(e) if e.code() == Some(codes::MOD_NOT_FOUND) => {
        println!("a DLL the SDK needs is missing: {e}");
        return Ok(());
    }
    Err(e) => return Err(e.into()), // BAD_EXE_FORMAT explains the 32/64-bit mismatch
};
println!("using {}", sdk.path()?.display());
// SAFETY: the vendor header declares exactly this signature with __stdcall.
let encode: EncodeCard = unsafe { sdk.symbol("EncodeCard")? };
let room = ansi::encode("Phòng 101")?; // fails if the ANSI code page cannot write "ò"
let checkout = ansi::encode("202501151200")?;
let mut card = [0u8; 64];
// SAFETY: NUL-terminated inputs and a writable buffer of the stated length; `sdk` is
// still loaded.
let rc = unsafe { encode(room.as_ptr(), checkout.as_ptr(), card.as_mut_ptr(), 64) };
println!("EncodeCard returned {rc}: {}", ansi::decode(&card)?);
# Ok::<(), Box<dyn std::error::Error>>(())
```

**Loading.** `Library::open` calls `LoadLibraryExW` with the flags of the chosen `Search`:

| `Search` | `LoadLibraryExW` flags | Searches |
|----------|------------------------|----------|
| `Default` | 0 | the standard order (application directory, System32, Windows directory, current directory, `PATH`) |
| `ApplicationDir` | `LOAD_LIBRARY_SEARCH_APPLICATION_DIR \| LOAD_LIBRARY_SEARCH_SYSTEM32` | the executable's directory, then System32 |
| `DllDir` | `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR \| LOAD_LIBRARY_SEARCH_SYSTEM32` | absolute paths only; the DLL's own directory for its dependencies, then System32 |
| `System32` | `LOAD_LIBRARY_SEARCH_SYSTEM32` | System32 |

Error dialogs are suppressed during the call (`SetThreadErrorMode`), so an agent without
a desktop never blocks on a message box. Failures are `Error::Library { op, code, detail }`
with the `GetLastError` code: `codes::MOD_NOT_FOUND` (126) for a missing DLL or
dependency, `codes::PROC_NOT_FOUND` (127) for a missing export, and
`codes::BAD_EXE_FORMAT` (193) for a DLL built for another CPU architecture. For 193 the
message says so in words, names this process's architecture and, for an absolute path,
the DLL's (for example ``LoadLibraryExW failed for `C:\...\LockSDK.dll` (a 32-bit x86
DLL): the DLL (or a DLL it depends on) is built for a different CPU architecture than
this 64-bit x64 process; ...``).

**Symbols.** `unsafe fn symbol<F: Copy>(&self, name) -> Result<F>` looks up an export
with `GetProcAddress` and returns it as the function pointer type you state: use
`unsafe extern "system" fn` for `WINAPI` / `__stdcall` exports (on 64-bit Windows this is
the only convention, so one type works for both builds) and `unsafe extern "C" fn` for
`__cdecl` ones. A type that is not pointer-sized is rejected at run time; a wrong
signature or calling convention is undefined behaviour, which is why the call is
`unsafe`. The returned pointer does not borrow the `Library`: dropping the `Library`
calls `FreeLibrary`, after which the pointer must not be called. `Library::leak` keeps
the DLL loaded for the rest of the process. `Library::path` returns the file actually
loaded (`GetModuleFileNameW`).

**Bitness.** `Library::machine(path)` reads the MZ and PE headers of a file without
loading it and returns `Machine::X86`, `X64`, `Arm64` or `Other(u16)`; compare with
`Machine::current()`. It is plain Rust and works on every platform. Malformed files give
`Error::BadImage { offset, reason }` and unreadable ones `Error::Io`. In a 32-bit process
on 64-bit Windows, `System32` paths are redirected to `SysWOW64`; use `Sysnative` to reach
the 64-bit files.

**ANSI strings.** `ansi::encode(&str)` returns the text in the system ANSI code page
(`WideCharToMultiByte(CP_ACP, WC_NO_BEST_FIT_CHARS)`) with a terminating NUL, ready to
pass as `const char*`. A character without an exact mapping is
`Error::Unmappable { code_page, index, ch }` instead of a silent `?`, like .NET's
`ThrowOnUnmappableChar`; whether Vietnamese text maps depends on the machine (code page
1258 has it, 1252 only some letters, UTF-8 everything), and `ansi::code_page()` tells
which. `ansi::decode(&[u8])` reads up to the first NUL with `MultiByteToWideChar(CP_ACP)`.
On other platforms both return `Error::Unsupported`.

## Console

`console::hide_input()` turns echo off on the console behind standard input until the
returned guard is dropped (or `restore()`d), for reading a password with `read_line`. It
fails with `GetConsoleMode` when standard input is redirected and with `Unsupported` off
Windows; callers usually go on without hiding then.

```rust,no_run
use std::io::BufRead;

eprint!("Mat khau SQL: ");
let echo = dl_winos::console::hide_input().ok();
let mut password = String::new();
std::io::stdin().lock().read_line(&mut password)?;
drop(echo);
eprintln!();
# Ok::<(), std::io::Error>(())
```

## Limits

- COM: late binding only (no type libraries, vtable interfaces or custom interfaces); no
  event sinks (connection points), so OPOS `DataEvent`, `StatusUpdateEvent` and
  `ErrorEvent` are not delivered (reading MSR swipes needs `DataEvent`); no `ByRef`
  output arguments (OPOS `DirectIO` data comes back unchanged), no optional-argument
  omission, no arrays, dates, currency or decimals. Most OPOS control objects are 32-bit
  in-process servers: a 64-bit process gets `REGDB_E_CLASSNOTREG`; build for
  `i686-pc-windows-msvc` to load them.
- Registry access is read-only: no create, write, delete, notifications or remote
  registries. Value names that are not valid UTF-16 are returned lossily (U+FFFD) and can
  then not be read back by name.
- `REG_MULTI_SZ` lists end at the first empty string, as the format defines; empty
  strings in the middle of malformed data cut the list there.
- Spooler: RAW jobs only (no GDI/XPS rendering, no job status or control, no printer
  management). The spooler accepts a job once it is spooled; whether the device printed it
  is not reported.
- DLLs: exports by name only (no ordinals), no delay-load or `AddDllDirectory` /
  `SetDefaultDllDirectories` management, no resources or version info. Function pointer
  signatures are not checked (Windows exports carry none). A 32-bit DLL cannot be loaded
  into a 64-bit process at all; out-of-process bridging (a 32-bit helper process) is up to
  the application. `Library::machine` reads the COFF machine only: an ARM64X or ARM64EC
  image reports `Arm64` or `Other(0xA641)`, and a .NET "AnyCPU" assembly reports `X86`.
  ANSI helpers use the system code page (`CP_ACP`), not the thread or console code page.
- All `unsafe` code is in `src/sys_windows.rs`, `src/com_windows.rs`,
  `src/dll_windows.rs` and `Library::symbol`; buffers returned by Win32 are read with
  bounds checks.
