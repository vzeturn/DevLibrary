//! Portable NTLM authentication (MS-NLMP, NTLMv2 only) with explicit Windows credentials:
//! `User ID=DOMAIN\user;Password=...` logs in to SQL Server with NTLM on any OS, as
//! go-mssqldb's `integratedauth/ntlm` and Microsoft.Data.SqlClient on Linux do.
//!
//! The flow is the one of [`crate::sspi`]: the NEGOTIATE message goes into LOGIN7, the
//! server answers with an SSPI token carrying the CHALLENGE, and the AUTHENTICATE message
//! goes back as an SSPI message. SQL Server talks raw NTLM (no SPNEGO wrapper) to a client
//! that starts with raw NTLM.
//!
//! Only NTLMv2 is implemented (LM and NTLMv1 are refused by every supported Windows
//! version). The server's target information is echoed; when it carries a timestamp
//! (`MsvAvTimestamp`, every Windows since Vista/2008) the LM response is zero, `MsvAvFlags`
//! announces a MIC and the MIC is computed as MS-NLMP 3.1.5.1.2 says. No key exchange,
//! signing or sealing: TDS protects nothing with the NTLM session key. Channel bindings
//! (Extended Protection = Required) are not sent.

use std::time::{SystemTime, UNIX_EPOCH};

use dl_hash::{Digest, Hmac, Md4, Md5};

use crate::Error;

const SIGNATURE: &[u8; 8] = b"NTLMSSP\0";
const NEGOTIATE_MESSAGE: u32 = 1;
const CHALLENGE_MESSAGE: u32 = 2;
const AUTHENTICATE_MESSAGE: u32 = 3;

const NEGOTIATE_UNICODE: u32 = 0x0000_0001;
const REQUEST_TARGET: u32 = 0x0000_0004;
const NEGOTIATE_NTLM: u32 = 0x0000_0200;
const NEGOTIATE_ALWAYS_SIGN: u32 = 0x0000_8000;
const NEGOTIATE_EXTENDED_SESSIONSECURITY: u32 = 0x0008_0000;
const NEGOTIATE_TARGET_INFO: u32 = 0x0080_0000;
const NEGOTIATE_VERSION: u32 = 0x0200_0000;
const NEGOTIATE_128: u32 = 0x2000_0000;
const NEGOTIATE_56: u32 = 0x8000_0000;

/// What the client asks for; the AUTHENTICATE message carries the subset the server kept.
const CLIENT_FLAGS: u32 = NEGOTIATE_UNICODE
    | REQUEST_TARGET
    | NEGOTIATE_NTLM
    | NEGOTIATE_ALWAYS_SIGN
    | NEGOTIATE_EXTENDED_SESSIONSECURITY
    | NEGOTIATE_TARGET_INFO
    | NEGOTIATE_VERSION
    | NEGOTIATE_128
    | NEGOTIATE_56;

/// The VERSION field (debugging only): Windows 6.1 build 7601, NTLMSSP revision 15.
const VERSION: [u8; 8] = [6, 1, 0xB1, 0x1D, 0, 0, 0, 0x0F];

const AV_EOL: u16 = 0;
const AV_FLAGS: u16 = 6;
const AV_TIMESTAMP: u16 = 7;
/// `MsvAvFlags` bit: the AUTHENTICATE message carries a MIC.
const AV_FLAG_MIC: u32 = 0x0000_0002;

/// AUTHENTICATE header: fixed fields (64), VERSION (8), MIC (16).
const AUTH_HEADER: usize = 88;
const MIC_AT: usize = 72;

/// Seconds between 1601-01-01 (FILETIME epoch) and 1970-01-01.
const FILETIME_UNIX_OFFSET: u64 = 11_644_473_600;

/// Windows credentials given in the connection string.
pub(crate) struct Credentials {
    domain: String,
    user: String,
    password: String,
}

impl Credentials {
    /// `DOMAIN\user` with both parts non-empty, as go-mssqldb recognises a Windows login;
    /// `None` for a SQL login.
    pub(crate) fn from_login(login: &str, password: &str) -> Option<Credentials> {
        let (domain, user) = login.split_once('\\')?;
        if domain.is_empty() || user.is_empty() || user.contains('\\') {
            return None;
        }
        Some(Credentials {
            domain: domain.to_string(),
            user: user.to_string(),
            password: password.to_string(),
        })
    }
}

/// An NTLM client context: NEGOTIATE first, then one AUTHENTICATE for the CHALLENGE.
pub(crate) struct Ntlm {
    creds: Credentials,
    workstation: String,
    negotiate: Vec<u8>,
    done: bool,
}

impl Ntlm {
    pub(crate) fn new(creds: Credentials, workstation: &str) -> Ntlm {
        Ntlm {
            creds,
            workstation: workstation.to_string(),
            negotiate: Vec::new(),
            done: false,
        }
    }

    /// The NEGOTIATE message (no domain or workstation: they go into AUTHENTICATE).
    pub(crate) fn initialize(&mut self) -> Vec<u8> {
        let mut m = Vec::with_capacity(40);
        m.extend_from_slice(SIGNATURE);
        m.extend_from_slice(&NEGOTIATE_MESSAGE.to_le_bytes());
        m.extend_from_slice(&CLIENT_FLAGS.to_le_bytes());
        // Empty DomainNameFields and WorkstationFields pointing at the payload.
        for _ in 0..2 {
            m.extend_from_slice(&[0, 0, 0, 0]);
            m.extend_from_slice(&40u32.to_le_bytes());
        }
        m.extend_from_slice(&VERSION);
        self.negotiate = m.clone();
        m
    }

    /// The AUTHENTICATE message answering the server's CHALLENGE.
    pub(crate) fn next(&mut self, challenge: &[u8]) -> Result<Vec<u8>, Error> {
        if self.done {
            return Err(Error::Ntlm("the server sent a second CHALLENGE"));
        }
        let client_challenge = dl_rand::secure_bytes::<8>()
            .map_err(|_| Error::Ntlm("no random source for the client challenge"))?;
        let out = self.authenticate(challenge, client_challenge, filetime_now())?;
        self.done = true;
        Ok(out)
    }

    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// AUTHENTICATE with a given client challenge and fallback time (deterministic for
    /// tests; the server's timestamp wins when it sent one).
    fn authenticate(
        &self,
        challenge: &[u8],
        client_challenge: [u8; 8],
        now: u64,
    ) -> Result<Vec<u8>, Error> {
        let ch = parse_challenge(challenge)?;
        if ch.flags & NEGOTIATE_UNICODE == 0 {
            return Err(Error::Ntlm(
                "the server does not accept Unicode NTLM messages",
            ));
        }
        let (target_info, timestamp) = response_target_info(ch.target_info)?;
        let key = ntowfv2(&self.creds.password, &self.creds.user, &self.creds.domain);
        let time = timestamp.unwrap_or(now);
        let r = ntlmv2_response(
            &key,
            &ch.server_challenge,
            &client_challenge,
            time,
            &target_info,
        );
        // With a server timestamp the LM response is Z(24) (MS-NLMP 3.1.5.1.2).
        let lm = if timestamp.is_some() {
            [0u8; 24]
        } else {
            lmv2_response(&key, &ch.server_challenge, &client_challenge)
        };
        let flags = CLIENT_FLAGS & ch.flags;
        let fields: [Vec<u8>; 6] = [
            lm.to_vec(),
            r.nt_response,
            utf16(&self.creds.domain),
            utf16(&self.creds.user),
            utf16(&self.workstation),
            Vec::new(), // EncryptedRandomSessionKey: no key exchange.
        ];
        let mut m = Vec::with_capacity(AUTH_HEADER + fields.iter().map(Vec::len).sum::<usize>());
        m.extend_from_slice(SIGNATURE);
        m.extend_from_slice(&AUTHENTICATE_MESSAGE.to_le_bytes());
        let mut offset = AUTH_HEADER;
        for f in &fields {
            let len = u16::try_from(f.len())
                .map_err(|_| Error::Ntlm("a name or the target information is too long"))?;
            m.extend_from_slice(&len.to_le_bytes());
            m.extend_from_slice(&len.to_le_bytes());
            m.extend_from_slice(&(offset as u32).to_le_bytes());
            offset += f.len();
        }
        m.extend_from_slice(&flags.to_le_bytes());
        m.extend_from_slice(&VERSION);
        m.extend_from_slice(&[0u8; 16]); // MIC, filled in below.
        for f in &fields {
            m.extend_from_slice(f);
        }
        if timestamp.is_some() {
            // No key exchange: ExportedSessionKey = KeyExchangeKey = SessionBaseKey.
            let mic = Hmac::<Md5>::new(&r.session_base_key)
                .chain(&self.negotiate)
                .chain(challenge)
                .chain(&m)
                .finalize();
            m[MIC_AT..MIC_AT + 16].copy_from_slice(&mic);
        }
        Ok(m)
    }
}

/// The parts of a CHALLENGE message the client uses.
pub(crate) struct Challenge<'a> {
    pub(crate) flags: u32,
    pub(crate) server_challenge: [u8; 8],
    pub(crate) target_info: &'a [u8],
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The payload a `(len u16, maxlen u16, offset u32)` field at `at` points to.
pub(crate) fn field(msg: &[u8], at: usize) -> Option<&[u8]> {
    let len = usize::from(u16_at(msg, at)?);
    let off = u32_at(msg, at + 4)? as usize;
    msg.get(off..off.checked_add(len)?)
}

pub(crate) fn parse_challenge(msg: &[u8]) -> Result<Challenge<'_>, Error> {
    if msg.len() < 48 || &msg[..8] != SIGNATURE {
        return Err(Error::Ntlm(
            "the server's SSPI token is not an NTLM message",
        ));
    }
    if u32_at(msg, 8) != Some(CHALLENGE_MESSAGE) {
        return Err(Error::Ntlm("the server's NTLM message is not a CHALLENGE"));
    }
    let flags = u32_at(msg, 20).unwrap_or(0);
    let mut server_challenge = [0u8; 8];
    server_challenge.copy_from_slice(&msg[24..32]);
    let target_info = if flags & NEGOTIATE_TARGET_INFO != 0 {
        field(msg, 40).ok_or(Error::Ntlm("the CHALLENGE target information is truncated"))?
    } else {
        &[]
    };
    Ok(Challenge {
        flags,
        server_challenge,
        target_info,
    })
}

/// The AV pairs of `info` without the terminating `MsvAvEOL`.
pub(crate) fn av_pairs(info: &[u8]) -> Result<Vec<(u16, &[u8])>, Error> {
    let bad = || Error::Ntlm("the CHALLENGE target information is malformed");
    let mut out = Vec::new();
    let mut at = 0;
    while at < info.len() {
        let (Some(id), Some(len)) = (u16_at(info, at), u16_at(info, at + 2)) else {
            return Err(bad());
        };
        let value = info
            .get(at + 4..at + 4 + usize::from(len))
            .ok_or_else(bad)?;
        if id == AV_EOL {
            return Ok(out);
        }
        out.push((id, value));
        at += 4 + usize::from(len);
    }
    // A missing terminator is tolerated (empty target information has none).
    Ok(out)
}

/// The target information the client sends back (the server's pairs, `MsvAvFlags` with
/// the MIC bit when the server sent a timestamp, `MsvAvEOL`) and that timestamp.
fn response_target_info(info: &[u8]) -> Result<(Vec<u8>, Option<u64>), Error> {
    let pairs = av_pairs(info)?;
    let timestamp = pairs
        .iter()
        .find(|(id, _)| *id == AV_TIMESTAMP)
        .map(|(_, v)| {
            <[u8; 8]>::try_from(*v)
                .map(u64::from_le_bytes)
                .map_err(|_| Error::Ntlm("the CHALLENGE timestamp is malformed"))
        })
        .transpose()?;
    let mut out = Vec::with_capacity(info.len() + 12);
    let mut flags_written = false;
    for (id, v) in &pairs {
        out.extend_from_slice(&id.to_le_bytes());
        if *id == AV_FLAGS && timestamp.is_some() {
            let f = u32_at(v, 0).unwrap_or(0) | AV_FLAG_MIC;
            out.extend_from_slice(&4u16.to_le_bytes());
            out.extend_from_slice(&f.to_le_bytes());
            flags_written = true;
        } else {
            out.extend_from_slice(&(v.len() as u16).to_le_bytes());
            out.extend_from_slice(v);
        }
    }
    if timestamp.is_some() && !flags_written {
        out.extend_from_slice(&AV_FLAGS.to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&AV_FLAG_MIC.to_le_bytes());
    }
    out.extend_from_slice(&[0, 0, 0, 0]); // MsvAvEOL
    Ok((out, timestamp))
}

pub(crate) fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn filetime_now() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() + FILETIME_UNIX_OFFSET) * 10_000_000 + u64::from(d.subsec_nanos() / 100)
}

/// `NTOWFv2` (MS-NLMP 3.3.2): HMAC-MD5 keyed with MD4(UTF-16 password) over the
/// upper-cased user name followed by the domain.
pub(crate) fn ntowfv2(password: &str, user: &str, domain: &str) -> [u8; 16] {
    let nt_hash = Md4::digest(utf16(password));
    Hmac::<Md5>::new(&nt_hash)
        .chain(utf16(&user.to_uppercase()))
        .chain(utf16(domain))
        .finalize()
}

pub(crate) struct V2Response {
    /// `NTProofStr || temp` (`NTProofStr` is the first 16 bytes).
    pub(crate) nt_response: Vec<u8>,
    pub(crate) session_base_key: [u8; 16],
}

/// The NTLMv2 response (MS-NLMP 3.3.2) for `target_info` (AV pairs ending in `MsvAvEOL`).
pub(crate) fn ntlmv2_response(
    key: &[u8; 16],
    server_challenge: &[u8; 8],
    client_challenge: &[u8; 8],
    time: u64,
    target_info: &[u8],
) -> V2Response {
    let mut temp = Vec::with_capacity(28 + target_info.len() + 4);
    temp.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0, 0]);
    temp.extend_from_slice(&time.to_le_bytes());
    temp.extend_from_slice(client_challenge);
    temp.extend_from_slice(&[0, 0, 0, 0]);
    temp.extend_from_slice(target_info);
    temp.extend_from_slice(&[0, 0, 0, 0]);
    proof_for(key, server_challenge, temp)
}

/// `NTProofStr` and `SessionBaseKey` for a `temp` blob (also what a server recomputes).
pub(crate) fn proof_for(key: &[u8; 16], server_challenge: &[u8; 8], temp: Vec<u8>) -> V2Response {
    let nt_proof = Hmac::<Md5>::new(key)
        .chain(server_challenge)
        .chain(&temp)
        .finalize();
    let session_base_key = Hmac::<Md5>::mac(key, nt_proof);
    let mut nt_response = nt_proof.to_vec();
    nt_response.extend_from_slice(&temp);
    V2Response {
        nt_response,
        session_base_key,
    }
}

/// The LMv2 response: HMAC-MD5(NTOWFv2, server || client challenge) || client challenge.
pub(crate) fn lmv2_response(
    key: &[u8; 16],
    server_challenge: &[u8; 8],
    client_challenge: &[u8; 8],
) -> [u8; 24] {
    let mac = Hmac::<Md5>::new(key)
        .chain(server_challenge)
        .chain(client_challenge)
        .finalize();
    let mut out = [0u8; 24];
    out[..16].copy_from_slice(&mac);
    out[16..].copy_from_slice(client_challenge);
    out
}

#[cfg(test)]
#[path = "ntlm_tests.rs"]
pub(crate) mod tests;
