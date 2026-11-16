//! Printer enumeration and RAW print jobs through the Windows print spooler.
//!
//! ```no_run
//! use dl_winos::spooler;
//!
//! for p in spooler::printers()? {
//!     println!("{} on {} ({}){}", p.name, p.port, p.driver, if p.is_default { " *" } else { "" });
//! }
//! if let Some(name) = spooler::default_printer()? {
//!     // ESC/POS: initialise, text, feed and cut.
//!     let job = spooler::write_raw(&name, "receipt", b"\x1b@Hello\n\n\n\x1dV\x00")?;
//!     println!("job {job}");
//! }
//! # Ok::<(), dl_winos::Error>(())
//! ```

use crate::wide::to_wide;
use crate::{Error, sys};

/// `PRINTER_ATTRIBUTE_*` flags found in [`PrinterInfo::attributes`].
pub mod attributes {
    /// `PRINTER_ATTRIBUTE_QUEUED`: print after the last page is spooled.
    pub const QUEUED: u32 = 0x0000_0001;
    /// `PRINTER_ATTRIBUTE_DIRECT`: jobs go to the printer without spooling.
    pub const DIRECT: u32 = 0x0000_0002;
    /// `PRINTER_ATTRIBUTE_DEFAULT`: legacy default-printer flag; Windows rarely sets it, so
    /// use [`PrinterInfo::is_default`](super::PrinterInfo::is_default) instead.
    pub const DEFAULT: u32 = 0x0000_0004;
    /// `PRINTER_ATTRIBUTE_SHARED`: shared on the network.
    pub const SHARED: u32 = 0x0000_0008;
    /// `PRINTER_ATTRIBUTE_NETWORK`: a connection to a printer on another computer.
    pub const NETWORK: u32 = 0x0000_0010;
    /// `PRINTER_ATTRIBUTE_HIDDEN`.
    pub const HIDDEN: u32 = 0x0000_0020;
    /// `PRINTER_ATTRIBUTE_LOCAL`: a printer installed on this computer.
    pub const LOCAL: u32 = 0x0000_0040;
    /// `PRINTER_ATTRIBUTE_WORK_OFFLINE`: the user set the printer offline.
    pub const WORK_OFFLINE: u32 = 0x0000_0400;
    /// `PRINTER_ATTRIBUTE_RAW_ONLY`: only RAW jobs can be spooled.
    pub const RAW_ONLY: u32 = 0x0000_1000;
}

/// One printer from [`printers`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrinterInfo {
    /// The printer name to pass to [`write_raw`] (`\\server\printer` for connections).
    pub name: String,
    /// The port, for example `USB001`, `COM3:`, `LPT1:` or a TCP/IP port name. Several
    /// ports are separated by commas.
    pub port: String,
    /// The driver name.
    pub driver: String,
    /// Whether this is the current user's default printer.
    pub is_default: bool,
    /// `PRINTER_ATTRIBUTE_*` flags (see [`attributes`]).
    pub attributes: u32,
}

/// The local printers and the current user's printer connections (`EnumPrintersW` level 2
/// with `PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS`).
pub fn printers() -> Result<Vec<PrinterInfo>, Error> {
    let raw = sys::enum_printers()?;
    // A missing or unreadable default printer only means no entry is marked.
    let default = default_printer().ok().flatten();
    Ok(raw
        .into_iter()
        .map(|(name, port, driver, attributes)| PrinterInfo {
            is_default: default.as_deref().is_some_and(|d| same_name(d, &name)),
            name,
            port,
            driver,
            attributes,
        })
        .collect())
}

/// Printer names are case-insensitive.
fn same_name(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

/// The current user's default printer name, or `Ok(None)` when none is set.
pub fn default_printer() -> Result<Option<String>, Error> {
    sys::default_printer()
}

/// Sends `bytes` unchanged to `printer` as one RAW job named `document_name` and returns
/// the spooler job id.
///
/// Use this for printer languages the application produces itself: ESC/POS receipts, ZPL
/// or EPL labels, PCL, PostScript. An unknown printer fails with [`Error::Win32`] code
/// [`codes::INVALID_PRINTER_NAME`](crate::codes::INVALID_PRINTER_NAME) (1801). When a call
/// after `StartDocPrinterW` fails, the job is cancelled with `AbortPrinter` and the first
/// error is returned.
pub fn write_raw(printer: &str, document_name: &str, bytes: &[u8]) -> Result<u32, Error> {
    if printer.is_empty() {
        return Err(Error::InvalidInput("empty printer name"));
    }
    sys::write_raw(&to_wide(printer)?, &to_wide(document_name)?, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_compare_case_insensitively() {
        assert!(same_name(
            "Microsoft Print to PDF",
            "microsoft print TO pdf"
        ));
        assert!(same_name("Máy in Đ", "máy in đ"));
        assert!(!same_name("a", "b"));
    }

    #[test]
    fn bad_arguments() {
        assert!(matches!(
            write_raw("", "doc", b"x"),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            write_raw("p\0", "doc", b"x"),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            write_raw("p", "d\0", b"x"),
            Err(Error::InvalidInput(_))
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_elsewhere() {
        assert_eq!(printers().unwrap_err(), Error::Unsupported);
        assert_eq!(default_printer().unwrap_err(), Error::Unsupported);
        assert_eq!(write_raw("p", "d", b"x").unwrap_err(), Error::Unsupported);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use crate::codes;
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[test]
    fn lists_print_to_pdf() {
        let list = printers().unwrap();
        let pdf = list
            .iter()
            .find(|p| p.name == "Microsoft Print to PDF")
            .unwrap_or_else(|| panic!("{list:?}"));
        assert_eq!(pdf.port, "PORTPROMPT:");
        assert_eq!(pdf.driver, "Microsoft Print To PDF");
        assert_ne!(pdf.attributes & attributes::LOCAL, 0);
        match default_printer().unwrap() {
            Some(d) => {
                let marked: Vec<_> = list.iter().filter(|p| p.is_default).collect();
                assert!(marked.len() <= 1, "{marked:?}");
                assert!(marked.iter().all(|p| same_name(&p.name, &d)));
            }
            None => assert!(list.iter().all(|p| !p.is_default)),
        }
    }

    #[test]
    fn unknown_printer() {
        let e = write_raw("dl-winos no such printer", "doc", b"x").unwrap_err();
        assert_eq!(e.code(), Some(codes::INVALID_PRINTER_NAME), "{e}");
        assert_eq!(e.operation(), Some("OpenPrinterW"));
    }

    fn powershell(script: &str) -> std::process::Output {
        Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .output()
            .unwrap()
    }

    /// Removes the temporary printer and port even when an assertion fails.
    struct TempPrinter {
        name: String,
        port: String,
    }

    impl Drop for TempPrinter {
        fn drop(&mut self) {
            let _ = powershell(&format!(
                "Remove-Printer -Name '{}' -ErrorAction SilentlyContinue; \
                 Start-Sleep -Milliseconds 500; \
                 Remove-PrinterPort -Name '{}' -ErrorAction SilentlyContinue",
                self.name.replace('\'', "''"),
                self.port.replace('\'', "''"),
            ));
            let _ = std::fs::remove_file(&self.port);
        }
    }

    /// Needs an elevated process: installs the inbox "Generic / Text Only" driver (a v3
    /// driver, so RAW data passes through unchanged; v4 drivers such as Microsoft Print To
    /// PDF swallow it), creates a local printer whose port is a file, prints RAW bytes to it
    /// and checks the file holds exactly those bytes. The driver stays installed.
    #[test]
    #[ignore = "creates a printer; needs elevation (cargo test -- --ignored)"]
    fn raw_job_reaches_file_port() {
        let dir = std::env::temp_dir();
        let port = dir
            .join(format!("dl-winos-{}.prn", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let name = format!("dl-winos test {}", std::process::id());
        let _ = std::fs::remove_file(&port);
        let tmp = TempPrinter {
            name: name.clone(),
            port: port.clone(),
        };
        let out = powershell(&format!(
            "$ErrorActionPreference = 'Stop'; \
             Add-PrinterDriver -Name 'Generic / Text Only'; \
             Add-PrinterPort -Name '{p}'; \
             Add-Printer -Name '{n}' -DriverName 'Generic / Text Only' -PortName '{p}'",
            p = port.replace('\'', "''"),
            n = name.replace('\'', "''"),
        ));
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            printers()
                .unwrap()
                .iter()
                .any(|p| p.name == name && p.port == port)
        );

        // Every byte value, plus enough data for several WritePrinter calls in the spooler.
        let mut data: Vec<u8> = (0..=255u8).collect();
        data.extend((0..300_000u32).map(|i| (i * 7 % 251) as u8));
        let job = write_raw(&name, "dl-winos raw test", &data).unwrap();
        assert_ne!(job, 0);

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(got) = std::fs::read(&port)
                && got.len() >= data.len()
            {
                assert!(
                    got == data,
                    "file has {} bytes, sent {}",
                    got.len(),
                    data.len()
                );
                break;
            }
            if Instant::now() >= deadline {
                let jobs = powershell(&format!(
                    "Get-PrintJob -PrinterName '{}' | Format-List *; Get-Item '{}' | Format-List *",
                    name.replace('\'', "''"),
                    port.replace('\'', "''"),
                ));
                panic!(
                    "job {job} never reached {port}: {}{}",
                    String::from_utf8_lossy(&jobs.stdout),
                    String::from_utf8_lossy(&jobs.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        drop(tmp);
    }
}
