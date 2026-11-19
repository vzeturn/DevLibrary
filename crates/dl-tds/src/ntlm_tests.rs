//! NTLM against the MS-NLMP 4.2.4 test vectors, and a server-side verifier (what a domain
//! controller recomputes) used here and by the fake-server login test.

use super::*;

fn hex(s: &str) -> Vec<u8> {
    let s: String = s.split_whitespace().collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// MS-NLMP 4.2.4.3: the CHALLENGE_MESSAGE of the NTLMv2 example.
const SPEC_CHALLENGE: &str = "4e544c4d53535000 02000000 0c000c00 38000000 33828ae2 0123456789abcdef \
     0000000000000000 24002400 44000000 060070170000000f \
     530065007200760065007200 \
     02000c0044006f006d00610069006e00 01000c00530065007200760065007200 00000000";

fn spec_challenge() -> Vec<u8> {
    hex(SPEC_CHALLENGE)
}

const SERVER_CHALLENGE: [u8; 8] = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
const CLIENT_CHALLENGE: [u8; 8] = [0xaa; 8];

#[test]
fn ms_nlmp_4_2_4_ntlmv2_vectors() {
    // 4.2.4.1.1 NTOWFv2.
    let key = ntowfv2("Password", "User", "Domain");
    assert_eq!(key.to_vec(), hex("0c868a403bfd7a93a3001ef22ef02e3f"));
    let ch = spec_challenge();
    let c = parse_challenge(&ch).unwrap();
    assert_eq!(c.server_challenge, SERVER_CHALLENGE);
    assert_eq!(c.flags, 0xe28a_8233);
    // No timestamp: the target information goes back unchanged.
    let (ti, ts) = response_target_info(c.target_info).unwrap();
    assert_eq!(ts, None);
    assert_eq!(ti, c.target_info);
    let r = ntlmv2_response(&key, &SERVER_CHALLENGE, &CLIENT_CHALLENGE, 0, &ti);
    // 4.2.4.2.2 NTLMv2 response (NTProofStr) and 4.2.4.1.2 session base key.
    assert_eq!(
        r.nt_response[..16].to_vec(),
        hex("68cd0ab851e51c96aabc927bebef6a1c")
    );
    assert_eq!(
        r.session_base_key.to_vec(),
        hex("8de40ccadbc14a82f15cb0ad0de95ca3")
    );
    // 4.2.4.2.1 LMv2 response.
    assert_eq!(
        lmv2_response(&key, &SERVER_CHALLENGE, &CLIENT_CHALLENGE).to_vec(),
        hex("86c35097ac9cec102554764a57cccc19aaaaaaaaaaaaaaaa")
    );
}

#[test]
fn spec_example_authenticate_verifies() {
    let mut n = Ntlm::new(
        Credentials::from_login("Domain\\User", "Password").unwrap(),
        "COMPUTER",
    );
    let neg = n.initialize();
    let ch = spec_challenge();
    let auth = n.authenticate(&ch, CLIENT_CHALLENGE, 0).unwrap();
    let v = verify(&neg, &ch, &auth, "Password").unwrap();
    assert_eq!((v.domain.as_str(), v.user.as_str()), ("Domain", "User"));
    assert_eq!(v.workstation, "COMPUTER");
    assert!(!v.mic);
    // The NT response is exactly the spec's NTProofStr followed by temp.
    let nt = field(&auth, 20).unwrap();
    assert_eq!(nt[..16].to_vec(), hex("68cd0ab851e51c96aabc927bebef6a1c"));
    assert_eq!(
        field(&auth, 12).unwrap().to_vec(),
        hex("86c35097ac9cec102554764a57cccc19aaaaaaaaaaaaaaaa")
    );
    // Flags: what both sides support, no key exchange, signing or sealing.
    let flags = u32_at(&auth, 60).unwrap();
    assert_eq!(flags & 0x4000_0030, 0, "{flags:#x}");
    assert_ne!(flags & NEGOTIATE_UNICODE, 0);
    assert_eq!(
        &auth[MIC_AT..MIC_AT + 16],
        &[0u8; 16],
        "no MIC without a timestamp"
    );
    assert!(verify(&neg, &ch, &auth, "password").is_err());
}

#[test]
fn negotiate_message_layout() {
    let mut n = Ntlm::new(Credentials::from_login("D\\u", "p").unwrap(), "WS");
    let m = n.initialize();
    assert_eq!(m.len(), 40);
    assert_eq!(&m[..8], SIGNATURE);
    assert_eq!(u32_at(&m, 8), Some(NEGOTIATE_MESSAGE));
    assert_eq!(u32_at(&m, 12), Some(CLIENT_FLAGS));
    assert_eq!(field(&m, 16), Some(&[][..]));
    assert_eq!(field(&m, 24), Some(&[][..]));
    assert_eq!(&m[32..], &VERSION);
    assert!(!n.is_done());
}

#[test]
fn timestamp_means_zero_lm_mic_and_flags() {
    let mut n = Ntlm::new(
        Credentials::from_login("CORP\\svc_smile", "S3cret!").unwrap(),
        "POS-01",
    );
    let neg = n.initialize();
    let ch = challenge([7; 8], Some(0x01DB_0000_0000_0000), None);
    let auth = n.next(&ch).unwrap();
    assert!(n.is_done());
    let v = verify(&neg, &ch, &auth, "S3cret!").unwrap();
    assert!(v.mic);
    assert_eq!(field(&auth, 12).unwrap(), &[0u8; 24]);
    // The NT response carries the server's timestamp, not the local clock.
    let nt = field(&auth, 20).unwrap();
    assert_eq!(nt[24..32], 0x01DB_0000_0000_0000u64.to_le_bytes());
    // Any change to the messages breaks the MIC.
    let mut bad = auth.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    assert_eq!(verify(&neg, &ch, &bad, "S3cret!").unwrap_err(), "MIC");
    let mut neg2 = neg.clone();
    neg2[12] ^= 0x10;
    assert_eq!(verify(&neg2, &ch, &auth, "S3cret!").unwrap_err(), "MIC");
    assert_eq!(verify(&neg, &ch, &auth, "wrong").unwrap_err(), "NTProofStr");
    // A second CHALLENGE is refused.
    assert!(matches!(n.next(&ch), Err(Error::Ntlm(_))));
}

#[test]
fn existing_av_flags_keep_their_bits() {
    let ch = challenge([1; 8], Some(5), Some(0x1));
    let c = parse_challenge(&ch).unwrap();
    let (ti, ts) = response_target_info(c.target_info).unwrap();
    assert_eq!(ts, Some(5));
    let pairs = av_pairs(&ti).unwrap();
    let flags: Vec<_> = pairs.iter().filter(|(id, _)| *id == AV_FLAGS).collect();
    assert_eq!(flags.len(), 1);
    assert_eq!(u32_at(flags[0].1, 0), Some(0x3));
    assert_eq!(&ti[ti.len() - 4..], &[0, 0, 0, 0]);
}

#[test]
fn user_name_case_is_ignored_domain_case_is_not() {
    assert_eq!(ntowfv2("pw", "user", "D"), ntowfv2("pw", "USER", "D"));
    assert_ne!(ntowfv2("pw", "user", "D"), ntowfv2("pw", "user", "d"));
}

#[test]
fn windows_login_spellings() {
    assert!(Credentials::from_login("CORP\\smile", "x").is_some());
    let c = Credentials::from_login("MN-SERVER\\Administrator", "").unwrap();
    assert_eq!(
        (c.domain.as_str(), c.user.as_str()),
        ("MN-SERVER", "Administrator")
    );
    for s in ["sa", "", "\\user", "CORP\\", "a\\b\\c", "user@corp.example"] {
        assert!(Credentials::from_login(s, "x").is_none(), "{s}");
    }
}

#[test]
fn malformed_challenges_are_ntlm_errors() {
    let mut n = Ntlm::new(Credentials::from_login("D\\u", "p").unwrap(), "WS");
    n.initialize();
    let good = challenge([1; 8], None, None);
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (b"NEGOTIATE-ME".to_vec(), "not an NTLM message"),
        (
            {
                let mut c = good.clone();
                c[8] = 3;
                c
            },
            "not a CHALLENGE",
        ),
        (
            {
                let mut c = good.clone();
                c[40] = 0xFF; // target info longer than the message
                c
            },
            "truncated",
        ),
        (
            {
                let mut c = good.clone();
                c[20] &= !1; // no Unicode
                c
            },
            "Unicode",
        ),
        (
            {
                let mut c = good.clone();
                let ti = u32_at(&c, 44).unwrap() as usize;
                c[ti + 2] = 0xF0; // first AV pair overruns
                c
            },
            "malformed",
        ),
    ];
    for (c, want) in cases {
        let e = n.authenticate(&c, [0; 8], 0).unwrap_err();
        assert!(matches!(e, Error::Ntlm(_)), "{e:?}");
        assert!(e.to_string().contains(want), "{e} / {want}");
    }
}

/// A CHALLENGE as a Windows server sends it: target name `CORP`, NetBIOS and DNS names,
/// optionally `MsvAvFlags` and `MsvAvTimestamp`.
pub(crate) fn challenge(
    server_challenge: [u8; 8],
    timestamp: Option<u64>,
    flags: Option<u32>,
) -> Vec<u8> {
    let mut info = Vec::new();
    let mut av = |id: u16, v: &[u8]| {
        info.extend_from_slice(&id.to_le_bytes());
        info.extend_from_slice(&(v.len() as u16).to_le_bytes());
        info.extend_from_slice(v);
    };
    av(2, &utf16("CORP"));
    av(1, &utf16("SRV"));
    av(4, &utf16("corp.test"));
    av(3, &utf16("srv.corp.test"));
    if let Some(f) = flags {
        av(AV_FLAGS, &f.to_le_bytes());
    }
    if let Some(t) = timestamp {
        av(AV_TIMESTAMP, &t.to_le_bytes());
    }
    av(AV_EOL, &[]);
    let name = utf16("CORP");
    let flags = CLIENT_FLAGS | 0x0001_0000; // TARGET_TYPE_DOMAIN
    let mut m = Vec::new();
    m.extend_from_slice(SIGNATURE);
    m.extend_from_slice(&CHALLENGE_MESSAGE.to_le_bytes());
    m.extend_from_slice(&(name.len() as u16).to_le_bytes());
    m.extend_from_slice(&(name.len() as u16).to_le_bytes());
    m.extend_from_slice(&56u32.to_le_bytes());
    m.extend_from_slice(&flags.to_le_bytes());
    m.extend_from_slice(&server_challenge);
    m.extend_from_slice(&[0; 8]);
    m.extend_from_slice(&(info.len() as u16).to_le_bytes());
    m.extend_from_slice(&(info.len() as u16).to_le_bytes());
    m.extend_from_slice(&((56 + name.len()) as u32).to_le_bytes());
    m.extend_from_slice(&[10, 0, 0x63, 0x45, 0, 0, 0, 0x0F]);
    m.extend_from_slice(&name);
    m.extend_from_slice(&info);
    m
}

/// Who an AUTHENTICATE message says the client is.
#[derive(Debug)]
pub(crate) struct Verified {
    pub(crate) domain: String,
    pub(crate) user: String,
    pub(crate) workstation: String,
    pub(crate) mic: bool,
}

fn from_utf16(b: &[u8]) -> String {
    let u: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    String::from_utf16_lossy(&u)
}

/// What the server side recomputes for NTLMv2 with `password`: `NTProofStr`, the echoed
/// target information and, when announced, the MIC. Errors name the check that failed.
pub(crate) fn verify(
    negotiate: &[u8],
    challenge: &[u8],
    auth: &[u8],
    password: &str,
) -> Result<Verified, &'static str> {
    if auth.get(..8) != Some(&SIGNATURE[..]) || u32_at(auth, 8) != Some(AUTHENTICATE_MESSAGE) {
        return Err("not an AUTHENTICATE message");
    }
    let f = |at| field(auth, at).ok_or("field");
    let (nt, domain, user, ws) = (f(20)?, f(28)?, f(36)?, f(44)?);
    let v = Verified {
        domain: from_utf16(domain),
        user: from_utf16(user),
        workstation: from_utf16(ws),
        mic: false,
    };
    let ch = parse_challenge(challenge).map_err(|_| "challenge")?;
    if nt.len() < 16 + 28 + 4 || nt[16..18] != [1, 1] {
        return Err("NTLMv2 response");
    }
    let key = ntowfv2(password, &v.user, &v.domain);
    let temp = nt[16..].to_vec();
    let r = proof_for(&key, &ch.server_challenge, temp.clone());
    if !dl_hash::ct_eq(&r.nt_response[..16], &nt[..16]) {
        return Err("NTProofStr");
    }
    let ti = &temp[28..temp.len() - 4];
    let echoed = av_pairs(ti).map_err(|_| "target info")?;
    for pair in av_pairs(ch.target_info).map_err(|_| "target info")? {
        if pair.0 != AV_FLAGS && !echoed.contains(&pair) {
            return Err("target info not echoed");
        }
    }
    let mic = echoed
        .iter()
        .any(|(id, v)| *id == AV_FLAGS && u32_at(v, 0).unwrap_or(0) & AV_FLAG_MIC != 0);
    if mic {
        let mut zeroed = auth.to_vec();
        zeroed[MIC_AT..MIC_AT + 16].fill(0);
        let want = Hmac::<Md5>::new(&r.session_base_key)
            .chain(negotiate)
            .chain(challenge)
            .chain(&zeroed)
            .finalize();
        if !dl_hash::ct_eq(&want, &auth[MIC_AT..MIC_AT + 16]) {
            return Err("MIC");
        }
    }
    Ok(Verified { mic, ..v })
}
