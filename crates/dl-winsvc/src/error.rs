use core::fmt;

/// Win32 error codes callers commonly check, as returned by [`Error::code`].
pub mod codes {
    /// `ERROR_ACCESS_DENIED`: the caller lacks the rights (often: not elevated).
    pub const ACCESS_DENIED: u32 = 5;
    /// `ERROR_SERVICE_DOES_NOT_EXIST`.
    pub const SERVICE_DOES_NOT_EXIST: u32 = 1060;
    /// `ERROR_SERVICE_NOT_ACTIVE`: stopping a service that is not running.
    pub const SERVICE_NOT_ACTIVE: u32 = 1062;
    /// `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT`: [`run_service`](crate::run_service) was
    /// called from a process the SCM did not start. Fall back to running as a console app.
    pub const FAILED_SERVICE_CONTROLLER_CONNECT: u32 = 1063;
    /// `ERROR_SERVICE_ALREADY_RUNNING`.
    pub const SERVICE_ALREADY_RUNNING: u32 = 1056;
    /// `ERROR_SERVICE_EXISTS`.
    pub const SERVICE_EXISTS: u32 = 1073;
    /// `ERROR_SERVICE_MARKED_FOR_DELETE`.
    pub const SERVICE_MARKED_FOR_DELETE: u32 = 1072;
}

/// Errors from service, SCM and DPAPI calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The platform has no Windows services or DPAPI.
    Unsupported,
    /// A Win32 call failed.
    Win32 {
        /// The Win32 function that failed.
        op: &'static str,
        /// The error code (`GetLastError`, or an `NTSTATUS` for `Nt*` functions).
        code: u32,
    },
    /// An argument cannot be passed to Windows (for example a string with a NUL character).
    InvalidInput(&'static str),
    /// A wait ended before the service reached the requested state.
    Timeout {
        /// What was being waited for.
        op: &'static str,
    },
    /// [`run_service`](crate::run_service) was already called in this process.
    AlreadyRunning,
    /// The service work closure panicked; the service reported a service-specific error.
    WorkPanicked,
}

impl Error {
    /// The Win32 error code, when the error came from Windows.
    pub fn code(&self) -> Option<u32> {
        match self {
            Error::Win32 { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// The Win32 function or operation that failed, when known.
    pub fn operation(&self) -> Option<&'static str> {
        match self {
            Error::Win32 { op, .. } | Error::Timeout { op } => Some(op),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported => {
                f.write_str("Windows services are not available on this platform")
            }
            Error::Win32 { op, code } => {
                if *code > 0xFFFF {
                    write!(f, "{op} failed with status {code:#010x}")
                } else {
                    let os = std::io::Error::from_raw_os_error(*code as i32);
                    write!(f, "{op} failed: {os}")
                }
            }
            Error::InvalidInput(what) => write!(f, "invalid input: {what}"),
            Error::Timeout { op } => write!(f, "timed out waiting for {op}"),
            Error::AlreadyRunning => f.write_str("the service dispatcher is already running"),
            Error::WorkPanicked => f.write_str("the service work panicked"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_accessors() {
        let e = Error::Win32 {
            op: "OpenServiceW",
            code: codes::SERVICE_DOES_NOT_EXIST,
        };
        assert_eq!(e.code(), Some(1060));
        assert_eq!(e.operation(), Some("OpenServiceW"));
        assert!(e.to_string().starts_with("OpenServiceW failed: "), "{e}");
        let nt = Error::Win32 {
            op: "NtQuerySystemInformation",
            code: 0xC000_0022,
        };
        assert_eq!(
            nt.to_string(),
            "NtQuerySystemInformation failed with status 0xc0000022"
        );
        assert_eq!(
            Error::Timeout { op: "Running" }.operation(),
            Some("Running")
        );
        assert_eq!(Error::Unsupported.code(), None);
        assert_eq!(Error::InvalidInput("NUL").to_string(), "invalid input: NUL");
    }
}
