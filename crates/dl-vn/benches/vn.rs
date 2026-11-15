//! `cargo bench -p dl-vn`
use dl_bench::Bench;
use dl_vn::vietqr::{Initiation, VietQr};
use dl_vn::{phone, tax, text, words};

fn main() {
    let mut b = Bench::from_args();

    b.run("number_to_words u64::MAX", || {
        words::number_to_words(u64::MAX)
    });
    b.run("vnd_to_words 1_234_567", || words::vnd_to_words(1_234_567));
    b.run("amount_to_words 1234.56 USD", || {
        words::amount_to_words("1234.56", "đô la Mỹ", Some(("xu", 2)))
    });

    let title = "Khách sạn Đà Nẵng - Phòng 101! Tiếng Việt có dấu, Nguyễn Văn Đức";
    b.throughput("remove_diacritics", title.len() as u64, || {
        text::remove_diacritics(title)
    });
    b.throughput("slugify", title.len() as u64, || text::slugify(title));
    let names = [
        "Nguyễn Văn An",
        "Trần Thị Ánh",
        "Lê Văn Ân",
        "Phạm Thị Bình",
        "Hoàng Văn Dũng",
        "Đỗ Minh Đức",
        "Vũ Thị Hà",
        "Bùi Văn Hải",
        "Đặng Thị Yến",
        "Ngô Văn Xuân",
    ];
    b.run("sort 10 names with compare_vi", || {
        let mut v = names;
        v.sort_by(|x, y| text::compare_vi(x, y));
        v
    });

    b.run("TaxCode::parse branch", || {
        tax::TaxCode::parse("0100109106-475")
    });
    b.run("normalize_phone", || {
        phone::normalize_phone("(+84)912-345-678")
    });

    let mut qr = VietQr::new("970436", "1031933430");
    qr.initiation = Initiation::Dynamic;
    qr.amount = Some(1_250_000);
    qr.purpose = Some("Tien phong 101".into());
    let payload = qr.to_payload().unwrap();
    b.run("VietQr::to_payload", || qr.to_payload());
    b.run("VietQr::parse", || VietQr::parse(&payload));
}
