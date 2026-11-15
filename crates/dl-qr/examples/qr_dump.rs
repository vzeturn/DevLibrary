//! Writes sample QR codes as PBM images (4-module quiet zone, 1 pixel per module) plus a
//! `manifest.txt` with the expected payloads, for `tests/decode_check.py`.
//!
//! `cargo run -p dl-qr --example qr_dump -- <output dir>`

use dl_qr::{Ecc, QrCode, Segment};
use std::fmt::Write as _;
use std::path::PathBuf;

/// A VietQR (NAPAS / EMVCo) bank transfer payload with a valid CRC, from `gen_vectors.py`.
const VIETQR: &str = "00020101021238570010A00000072701270006970436011300110123456780208QRIBFTTA\
                      5303704540715000005802VN62350831Thanh toan hoa don HD2024000123630446AF";

fn pbm(qr: &QrCode, border: usize) -> String {
    let full = qr.size() + 2 * border;
    let mut s = format!("P1\n{full} {full}\n");
    for y in 0..full {
        for x in 0..full {
            let dark = x >= border && y >= border && qr.get(x - border, y - border);
            s.push(if dark { '1' } else { '0' });
        }
        s.push('\n');
    }
    s
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "target/qr-decode".into()),
    );
    std::fs::create_dir_all(&dir)?;

    let long_text: String = (0..2953).map(|i| (b'a' + (i % 26) as u8) as char).collect();
    let digits: String = (0..7089)
        .map(|i| char::from(b'0' + (i % 10) as u8))
        .collect();
    let alnum: String = "HTTPS://VIETQR.VN/PAY/ID-"
        .repeat(200)
        .chars()
        .take(4296)
        .collect();
    let vn = "Thanh toán hóa đơn 123";
    let mixed = "INVOICE 2024 000123456789 ok";
    let url = "https://example.com/checkin?event=42&seat=A7";

    let mut cases: Vec<(String, QrCode, String)> = vec![
        (
            "vietqr-m".into(),
            QrCode::encode_text(VIETQR, Ecc::Medium)?,
            VIETQR.into(),
        ),
        (
            "vietqr-h-boost".into(),
            QrCode::builder()
                .ecc(Ecc::Low)
                .boost_ecc(true)
                .encode_text(VIETQR)?,
            VIETQR.into(),
        ),
        (
            "max-v40-l-bytes".into(),
            QrCode::encode_text(&long_text, Ecc::Low)?,
            long_text.clone(),
        ),
        (
            "max-v40-l-numeric".into(),
            QrCode::encode_text(&digits, Ecc::Low)?,
            digits.clone(),
        ),
        (
            "max-v40-l-alnum".into(),
            QrCode::encode_text(&alnum, Ecc::Low)?,
            alnum.clone(),
        ),
        (
            "max-v40-h-numeric".into(),
            QrCode::encode_text(&digits[..3057], Ecc::High)?,
            digits[..3057].into(),
        ),
        (
            "vn-utf8".into(),
            QrCode::encode_text(vn, Ecc::Quartile)?,
            vn.into(),
        ),
        (
            "vn-utf8-eci".into(),
            QrCode::builder().eci_utf8(true).encode_text(vn)?,
            vn.into(),
        ),
        (
            "mixed".into(),
            QrCode::encode_text(mixed, Ecc::Medium)?,
            mixed.into(),
        ),
        (
            "segments".into(),
            QrCode::builder().ecc(Ecc::High).encode_segments(&[
                Segment::eci(26)?,
                Segment::bytes("Mã đơn: ".as_bytes()),
                Segment::numeric("20240512000001")?,
            ])?,
            "Mã đơn: 20240512000001".into(),
        ),
        (
            "empty".into(),
            QrCode::encode_text("", Ecc::Low)?,
            String::new(),
        ),
    ];
    for m in 0..8 {
        let qr = QrCode::builder()
            .ecc(Ecc::High)
            .mask(Some(m))
            .encode_text(url)?;
        cases.push((format!("url-h-mask{m}"), qr, url.into()));
    }
    for v in [7u8, 10, 20, 27, 33] {
        let qr = QrCode::builder()
            .ecc(Ecc::Quartile)
            .version(v)
            .encode_text(url)?;
        cases.push((format!("url-q-v{v}"), qr, url.into()));
    }

    let mut manifest = String::new();
    for (name, qr, expected) in &cases {
        std::fs::write(dir.join(format!("{name}.pbm")), pbm(qr, 4))?;
        let hex: String = expected.bytes().map(|b| format!("{b:02x}")).collect();
        writeln!(
            manifest,
            "{name} {} {:?} {} {}",
            qr.version(),
            qr.ecc(),
            qr.mask(),
            if hex.is_empty() { "-" } else { &hex }
        )?;
    }
    std::fs::write(dir.join("manifest.txt"), manifest)?;
    println!("wrote {} codes to {}", cases.len(), dir.display());
    Ok(())
}
