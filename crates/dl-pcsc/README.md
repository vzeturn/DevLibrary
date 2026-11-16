# dl-pcsc

PC/SC smart card access without dependencies: list readers, wait for cards and reader
hot-plug, connect, and exchange APDUs. The operating system's PC/SC library is loaded at
run time, so nothing is linked at build time and a program without it still starts:

| Platform | Library | Notes |
|----------|---------|-------|
| Windows (x64, ARM64, x86) | `winscard.dll` from System32 | wide-character (`W`) functions, service `SCardSvr` |
| Linux and other Unix | `libpcsclite.so.1` (`dlopen`) | needs the `pcscd` daemon; 64-bit `DWORD`/`LONG` on LP64 |
| macOS | `/System/Library/Frameworks/PCSC.framework/PCSC` | packed structs, 32-bit `DWORD` |
| WebAssembly, others | none | every call returns `Error::Unsupported` |

The library is built for the device agent that reads NFC/MIFARE card UIDs from
ACR122U-class contactless readers.

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| Go        | `github.com/ebfe/scard` |
| .NET      | `PCSC` (pcsc-sharp), hand-written `winscard.dll` P/Invoke |
| Rust      | `pcsc`, `pcsc-sys` |

## Read a card UID

```rust,no_run
use std::time::Duration;
use dl_pcsc::{Context, Protocols, Scope, Share};

let ctx = Context::establish(Scope::User)?;
let readers = ctx.list_readers()?; // empty when no reader is plugged in
let Some(reader) = readers.iter().find(|r| r.contains("ACR122")) else {
    return Ok(());
};
ctx.wait_for_card(reader, Some(Duration::from_secs(30)))?;
let card = ctx.connect(reader, Share::Shared, Protocols::ANY)?;
// Sends FF CA 00 00 00 (GET DATA UID) and checks the 90 00 status word.
println!("UID {}", card.uid()?); // e.g. "04A224B2C15E80"
# Ok::<(), dl_pcsc::Error>(())
```

`card.transmit(&apdu)` returns the raw response including `SW1 SW2`;
`apdu::split_status` and `apdu::uid_hex` take responses apart. Dropping a `Card`
disconnects with `Disposition::Leave`; `card.disconnect(Disposition::Reset)` chooses
another disposition.

## Watch readers and cards

```rust,no_run
use dl_pcsc::{Context, PNP_NOTIFICATION, ReaderWait, Scope, State};

let ctx = Context::establish(Scope::User)?;
let canceller = ctx.canceller(); // Send + Sync: call cancel() from a shutdown handler
let mut watch: Vec<ReaderWait> = ctx
    .list_readers()?
    .into_iter()
    .map(|r| ReaderWait::new(r, State::UNAWARE))
    .collect();
watch.push(ReaderWait::new(PNP_NOTIFICATION, State::UNAWARE));
loop {
    let states = match ctx.wait(&watch, None) {
        Ok(s) => s,
        Err(e) if e.is_cancelled() => break,
        // Windows stops the service when the last reader is removed.
        Err(e) if e.is_no_service() => break,
        Err(e) => return Err(e),
    };
    for s in &states {
        if s.name != PNP_NOTIFICATION && s.state.contains(State::CHANGED) {
            println!("{}: {:?}, ATR {:02X?}", s.name, s.state, s.atr);
        }
    }
    // Pass the new states back. When the PnP entry changed, a real program calls
    // list_readers again and adds or drops entries.
    watch = states.iter().map(|s| s.to_wait()).collect();
}
# Ok::<(), dl_pcsc::Error>(())
```

`Context` is `Send` but not `Sync`: PC/SC allows one thread per context, except for
`SCardCancel`, which `Canceller` wraps. A canceller outliving its context does nothing.

## Errors

`Error::Pcsc { op, code }` carries the PC/SC function and result code; `Display` names
the code (`SCardEstablishContext failed: SCARD_E_NO_SERVICE (0x8010001D)`) and
`codes::code_name` maps any code. Helpers: `is_no_service` (`SCARD_E_NO_SERVICE` or
`SCARD_E_SERVICE_STOPPED`), `is_timeout`, `is_removed` (`SCARD_W_REMOVED_CARD` or
`SCARD_E_NO_SMARTCARD`), `is_cancelled`. `list_readers` returns an empty list instead of
`SCARD_E_NO_READERS_AVAILABLE`. `Error::Unsupported` says why the library could not be
loaded.

On Windows 8 and later the Smart Card service starts when a reader is plugged in and
stops after the last one is removed: `Context::establish` then fails with
`SCARD_E_NO_SERVICE` and existing contexts with `SCARD_E_SERVICE_STOPPED`. Retry
`establish` periodically.

## Limits

- `SCardControl`, `SCardGetAttrib`/`SCardSetAttrib`, `SCardReconnect`, reader groups and
  the Windows card database functions are not exposed.
- Responses of short APDUs are read into a 258-byte buffer, of extended APDUs (detected
  from the command encoding) into 65538 bytes.
- The PnP pseudo reader needs pcsc-lite built with hot-plug support; macOS does not
  support it.
- All `unsafe` code is in `src/sys.rs`; struct layouts are checked by tests on each
  target.
