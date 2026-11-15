//! `cargo bench -p dl-qr`
use dl_bench::Bench;
use dl_qr::{Ecc, QrCode, SvgOptions};

fn main() {
    let mut b = Bench::from_args();

    let mut url = String::from("https://example.com/invoice/lookup?id=");
    while url.len() < 100 {
        url.push_str("a1B2c3");
    }
    url.truncate(100);
    b.run("encode_text 100-char URL, M", || {
        QrCode::encode_text(&url, Ecc::Medium).unwrap()
    });

    let payload: Vec<u8> = (0..1000u32).map(|i| (i * 31 + 7) as u8).collect();
    b.throughput("encode_bytes 1000 B, L", payload.len() as u64, || {
        QrCode::encode_bytes(&payload, Ecc::Low).unwrap()
    });

    b.run("encode_text 100-char URL, M, fixed mask", || {
        QrCode::builder()
            .ecc(Ecc::Medium)
            .mask(Some(0))
            .encode_text(&url)
            .unwrap()
    });

    let qr = QrCode::encode_text(&url, Ecc::Medium).unwrap();
    assert_eq!(qr.version(), 6, "README performance table");
    b.run("to_svg 100-char URL code", || {
        qr.to_svg(&SvgOptions::default())
    });
    let big = QrCode::encode_bytes(&payload, Ecc::Low).unwrap();
    assert_eq!(big.version(), 22, "README performance table");
    b.run("to_svg 1000 B code", || big.to_svg(&SvgOptions::default()));
    b.run("to_text 100-char URL code", || qr.to_text(4));
}
