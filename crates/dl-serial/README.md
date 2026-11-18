# dl-serial

Serial ports without dependencies: open a COM port (Windows) or a tty (Linux, macOS),
set the baud rate, framing, flow control and timeouts, read and write through
`std::io::Read` / `Write`, control DTR and RTS, and list the ports of the machine with
their USB ids. Written for ESC/POS receipt printers, serial card readers, customer pole
displays and the virtual COM ports of Bluetooth printers. On WebAssembly and other
platforms the API compiles and `open` / `available_ports` return `Error::Unsupported`.

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| Go        | `go.bug.st/serial`, `github.com/tarm/serial` |
| .NET      | `System.IO.Ports.SerialPort` |
| Rust      | `serialport`, `serial2` |

## Print a receipt

```rust,no_run
use std::io::Write;
use dl_serial::{SerialPort, Settings};

// 9600 8N1, no flow control, 3 s read and 5 s write timeouts (the defaults).
let mut printer = SerialPort::open("COM3", &Settings::default())?;
printer.write_all(b"\x1b@Hello\n\n\n\x1dV\x00")?; // ESC @, text, GS V 0 (cut)
printer.flush()?; // wait until the bytes are on the wire
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Read a card reader with a timeout

```rust,no_run
use std::io::{ErrorKind, Read};
use std::time::Duration;
use dl_serial::{Flow, SerialPort, Settings};

let settings = Settings {
    flow: Flow::RtsCts,
    read_timeout: Some(Duration::from_millis(500)),
    ..Settings::new(115_200)
};
let mut reader = SerialPort::open("/dev/ttyUSB0", &settings)?;
let mut buf = [0u8; 256];
match reader.read(&mut buf) {
    // Returns as soon as data arrives, with everything received so far.
    Ok(n) => println!("{:02x?}", &buf[..n]),
    Err(e) if e.kind() == ErrorKind::TimedOut => println!("no card"),
    Err(e) => return Err(e.into()),
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Find the port of a device

```rust,no_run
use dl_serial::{PortKind, available_ports};

for p in available_ports()? {
    match p.kind {
        PortKind::Usb { vid, pid, ref product, .. } => {
            println!("{} USB {vid:04x}:{pid:04x} {product:?}", p.name)
        }
        PortKind::Bluetooth => println!("{} Bluetooth", p.name),
        _ => println!("{}", p.name),
    }
}
# Ok::<(), dl_serial::Error>(())
```

## Errors

`open` tells the common failures apart: `Error::NotFound` (no such port),
`Error::Busy` (open in another program), `Error::PermissionDenied` (Unix: user not in
`dialout`), `Error::NotASerialPort`, `Error::InvalidName`, `Error::InvalidSettings` and
`Error::UnsupportedBaudRate`. Anything else is `Error::Os` with the operation and the OS
error code. Reads and writes return `std::io::Error`; `Error` converts into one.

## Behaviour

- **Windows**: opens `\\.\COM3` (so COM10 and above work) without sharing, with
  overlapped I/O, so a clone can write while another thread waits in a read. The DCB is
  built with `fBinary`, no DSR handshake, no NUL stripping and no abort-on-error;
  `SetCommTimeouts` uses `MAXDWORD / MAXDWORD / timeout`, which makes `ReadFile` return
  as soon as a byte arrives. `flush` is `FlushFileBuffers`, `clear` is `PurgeComm`,
  `bytes_available` is `ClearCommError`.
- **Linux** (x86_64, aarch64; glibc and musl): `open(O_RDWR | O_NOCTTY | O_NONBLOCK)`,
  `TIOCEXCL` and an advisory `flock`, then raw mode through the kernel `termios2`
  (`TCGETS2` / `TCSETS2`), so any baud rate the driver supports works through `BOTHER`.
  A custom rate the driver rounds by more than 2 % fails with `UnsupportedBaudRate`.
- **macOS**: the same open sequence with `tcsetattr`; rates without a `B` constant are
  set with `IOSSIOSPEED`. Mark and space parity are refused (no `CMSPAR`).
- On Unix the descriptor stays non-blocking and `poll()` implements both timeouts, so
  a write to a printer that holds CTS low fails with `TimedOut` instead of hanging.
  `flush` waits for the output queue within the write timeout, then calls `tcdrain`.

## Limits

- No events (`WaitCommEvent`), break signal, modem status reads (CTS, DSR, RI, CD) or
  per-port buffer size settings.
- `available_ports` on Windows reads the registry only (no SetupAPI): USB ids, serial
  numbers and names come from the `Enum\USB` and `Enum\FTDIBUS` device nodes, and the
  serial is `None` for composite devices. On macOS every port is `Unknown` except
  Bluetooth ones (no IOKit).
- Linux on other architectures, and the BSDs, use the stub (`Unsupported`).
- 1.5 stop bits need 5 data bits; 2 stop bits cannot be used with 5 data bits.
- All `unsafe` code is in `src/sys_windows.rs` and `src/sys_unix.rs`.
