//! `AcrNfcAdapter.cs`: ACR122U-class NFC reader over PC/SC (POS-HW-04, member and staff tap
//! cards). The UID is read with the PC/SC pseudo-APDU GET DATA (FF CA 00 00 00) and answered
//! as [`CardIdentity`] of its uppercase hex.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_json::Value;
use dl_pcsc::{Context, Protocols, ReaderWait, Scope, Share, State, codes as sc};

use super::card::CardIdentity;
use super::{contains_ignore_case, failures, seconds_0_1, trimmed_endpoint};
use crate::adapters::Cx;
use crate::contracts::{AdapterError, PosReadRequest, codes, transports};

/// Reads the UID of the next contactless card on a PC/SC reader. Faked in tests.
pub trait PcscCardSource: Send + Sync {
    /// The card UID bytes (GET DATA). Refuses with DEVICE_NO_CARD (no card within `wait`),
    /// DEVICE_UNAVAILABLE (no such reader) or DEVICE_ERROR (a PC/SC error).
    fn read_uid(&self, reader_name: &str, wait: Duration, cx: &Cx)
    -> Result<Vec<u8>, AdapterError>;
}

/// The NFC card reader. Without a card source (C#: not Windows) every read refuses with
/// DEVICE_PLATFORM_UNSUPPORTED.
pub struct AcrNfcAdapter {
    source: Option<Arc<dyn PcscCardSource>>,
}

impl AcrNfcAdapter {
    /// A reader over `source`; `None` is a host without PC/SC.
    pub fn new(source: Option<Arc<dyn PcscCardSource>>) -> AcrNfcAdapter {
        AcrNfcAdapter { source }
    }

    /// winscard.dll on Windows (as C#); on other hosts pcsc-lite when it loads (coverage
    /// beyond C#, which refuses there), otherwise none.
    pub fn create() -> AcrNfcAdapter {
        let source: Option<Arc<dyn PcscCardSource>> =
            if cfg!(windows) || dl_pcsc::load_library().is_ok() {
                Some(Arc::new(WinScardCardSource))
            } else {
                None
            };
        AcrNfcAdapter::new(source)
    }

    /// pos.read with device `card` over pcsc.
    pub fn read(&self, request: &PosReadRequest, cx: &Cx) -> Result<Value, AdapterError> {
        let Some(source) = &self.source else {
            return Err(failures::platform_unsupported("the PC/SC (NFC) reader"));
        };
        let target = super::target_of(&request.target)?;
        let reader = trimmed_endpoint(target)?;
        if reader.is_empty() {
            return Err(failures::bad_payload(
                "pcsc endpoint (reader name) is empty",
            ));
        }
        let wait = Duration::from_millis(u64::try_from(request.timeout_ms).unwrap_or(0));
        // Task.Run(..., ct): a cancelled call does not start the read.
        cx.check()?;
        let uid = source.read_uid(reader, wait, cx)?;
        if uid.is_empty() {
            return Err(AdapterError::new(
                codes::NO_CARD,
                "the reader answered an empty UID",
            ));
        }
        CardIdentity::of(&dl_encoding::hex::encode_upper(&uid), transports::PCSC)
    }
}

/// The system PC/SC library through dl-pcsc (winscard.dll, pcsc-lite): establish a context,
/// wait for a card on the named reader, connect, GET DATA, release.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinScardCardSource;

const GET_UID: [u8; 5] = [0xFF, 0xCA, 0x00, 0x00, 0x00];
const POLL: Duration = Duration::from_millis(250);

impl WinScardCardSource {
    fn find_reader(ctx: &Context, wanted: &str) -> Result<String, AdapterError> {
        // dl-pcsc answers an empty list for SCARD_E_NO_READERS_AVAILABLE.
        let names = ctx
            .list_readers()
            .map_err(|e| check(&e, "SCardListReaders"))?;
        if names.is_empty() {
            return Err(AdapterError::new(
                codes::UNAVAILABLE,
                "no PC/SC reader is connected",
            ));
        }
        let count = names.len();
        names
            .into_iter()
            .find(|n| contains_ignore_case(n, wanted))
            .ok_or_else(|| {
                AdapterError::new(
                    codes::UNAVAILABLE,
                    format!("no PC/SC reader matches '{wanted}' ({count} connected)"),
                )
            })
    }

    /// Polls in short slices so a cancelled command stops waiting.
    fn wait_for_card(
        ctx: &Context,
        reader: &str,
        wait: Duration,
        cx: &Cx,
    ) -> Result<(), AdapterError> {
        let until = Instant::now() + wait;
        let mut current = State::UNAWARE;
        loop {
            cx.check()?;
            match ctx.wait(&[ReaderWait::new(reader, current)], Some(POLL)) {
                Ok(states) => {
                    if let Some(s) = states.first() {
                        if s.state.contains(State::PRESENT) {
                            return Ok(());
                        }
                        current = s.state;
                    }
                }
                Err(e) if e.is_timeout() => {}
                Err(e) => return Err(check(&e, "SCardGetStatusChange")),
            }
            if Instant::now() >= until {
                let ms = i64::try_from(wait.as_millis()).unwrap_or(i64::MAX);
                return Err(AdapterError::new(
                    codes::NO_CARD,
                    format!("no card on the reader within {} s", seconds_0_1(ms)),
                ));
            }
        }
    }
}

impl PcscCardSource for WinScardCardSource {
    fn read_uid(
        &self,
        reader_name: &str,
        wait: Duration,
        cx: &Cx,
    ) -> Result<Vec<u8>, AdapterError> {
        let ctx =
            Context::establish(Scope::User).map_err(|e| check(&e, "SCardEstablishContext"))?;
        let reader = Self::find_reader(&ctx, reader_name)?;
        Self::wait_for_card(&ctx, &reader, wait, cx)?;
        let card = ctx
            .connect(&reader, Share::Shared, Protocols::ANY)
            .map_err(|e| check(&e, "SCardConnect"))?;
        let response = card
            .transmit(&GET_UID)
            .map_err(|e| check(&e, "SCardTransmit"))?;
        match response.as_slice() {
            [uid @ .., 0x90, 0x00] => Ok(uid.to_vec()),
            _ => Err(AdapterError::new(
                codes::ERROR,
                "the reader refused GET DATA (the card has no readable UID)",
            )),
        }
    }
}

/// `Check(rc, call)`: an unknown or unavailable reader (or none) is DEVICE_UNAVAILABLE, any
/// other result code DEVICE_ERROR `"{call} failed: 0x{rc:X8}"`.
fn check(e: &dl_pcsc::Error, call: &str) -> AdapterError {
    match e {
        dl_pcsc::Error::Pcsc { code, .. } => {
            let device_code = match *code {
                sc::SCARD_E_UNKNOWN_READER
                | sc::SCARD_E_READER_UNAVAILABLE
                | sc::SCARD_E_NO_READERS_AVAILABLE => codes::UNAVAILABLE,
                _ => codes::ERROR,
            };
            AdapterError::new(device_code, format!("{call} failed: 0x{code:08X}"))
        }
        dl_pcsc::Error::Unsupported(_) => failures::platform_unsupported("the PC/SC (NFC) reader"),
        other => AdapterError::new(codes::ERROR, format!("{call} failed: {other}")),
    }
}
