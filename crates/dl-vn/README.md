# dl-vn

Vietnam-specific utilities for business software: amounts in words for e-invoices,
Vietnamese diacritics, slugs and alphabetical sorting, tax codes (mã số thuế), phone
numbers, citizen ID numbers (CCCD) and VietQR bank-transfer payloads.

`dl-vn` is part of [DevLibrary](https://github.com/vzeturn/DevLibrary). It has no
dependencies, is `#![no_std]` (it needs `alloc`), forbids `unsafe`, and builds for
`wasm32-unknown-unknown`. Every parser returns a typed `dl_vn::Error`, with a byte offset
where it applies; no input makes it panic.

## Why

Each module replaces code that is easy to get subtly wrong and that Vietnamese projects
tend to copy from blog posts:

- **`words`**: the amount-in-words line ("số tiền bằng chữ") printed on e-invoices. The
  hard parts are the zero rules: 1 005 is "một nghìn không trăm linh năm", 1 000 000 005 is
  "một tỷ không trăm linh năm", 21 is "hai mươi mốt" but 11 is "mười một", 25 is
  "hai mươi lăm" but 105 is "một trăm linh năm". Decimal amounts are parsed exactly, without
  floating point.
- **`text`**: removing diacritics for search and file names, URL slugs, fixing the case of
  typed names, and sorting names in Vietnamese order. Plain string comparison puts "Ánh"
  after "Yến"; `compare_vi` follows the Unicode CLDR rules instead. Text from some keyboards
  and macOS file names arrives decomposed (NFD); every function handles it.
- **`tax`**: validating a buyer's MST before calling an e-invoice provider, including the
  check digit, 13-digit branch codes and the 12-digit personal codes used since July 2025.
- **`phone`**: turning whatever a customer typed into one canonical form for SMS, Zalo or a
  CRM, and telling mobiles from landlines.
- **`cccd`**: reading the province, gender and birth year encoded in a citizen ID number,
  and rejecting numbers that cannot exist.
- **`vietqr`**: generating the payload of a NAPAS247 transfer QR code for a hotel folio, a
  POS receipt or an invoice, and reading one back with CRC verification.

## Features

- Number reading up to `u64::MAX` ("mười tám tỷ tỷ ..."), with northern or southern forms:
  linh/lẻ, nghìn/ngàn, tỷ/tỉ, bốn/tư, and an optional "chẵn".
- Money in words for đồng and for any currency with a minor unit, negative amounts ("Âm").
- Diacritics removal for all 12 vowels × 6 tones in both cases plus đ/Đ, NFC and NFD input.
- Slugs, name normalisation with precomposed output, CLDR `vi` collation.
- MST validation (10, 13 and 12 digits) with the check digit, branch and parent access.
- Phone parsing of national and international forms, E.164 output, carrier and area lookup.
- CCCD parsing: province (63-province table), gender, century, birth year.
- VietQR build and parse (EMVCo MPM, NAPAS profile), CRC-16/CCITT-FALSE, 76-entry bank table.

## Installation

From GitHub:

```toml
[dependencies]
dl-vn = { git = "https://github.com/vzeturn/DevLibrary" }
```

From a local checkout:

```toml
[dependencies]
dl-vn = { path = "../DevLibrary/crates/dl-vn" }
```

Inside the DevLibrary workspace, use `dl-vn.workspace = true`. The minimum Rust version is
1.88.

## Quick start

```rust
use dl_vn::cccd::Cccd;
use dl_vn::tax::TaxCode;
use dl_vn::vietqr::VietQr;
use dl_vn::{phone, text, words};

assert_eq!(
    words::vnd_to_words(1_250_000),
    "Một triệu hai trăm năm mươi nghìn đồng"
);
assert_eq!(text::slugify("Khách sạn Đà Nẵng - Phòng 101!"), "khach-san-da-nang-phong-101");
assert!(TaxCode::parse("0100109106").is_ok());
assert_eq!(phone::normalize_phone("(+84)912-345-678")?.e164(), "+84912345678");
assert_eq!(Cccd::parse("079203000123")?.birth_year(), 2003);

let payload = VietQr::new("970436", "1031933430").to_payload()?;
assert!(payload.starts_with("000201010211"));
# Ok::<(), dl_vn::Error>(())
```

## Guide

### Amount in words for e-invoices (`words`)

`vnd_to_words` produces the capitalised line printed under the total. The defaults are the
northern forms: "linh", "nghìn", "tỷ", "bốn".

```rust
use dl_vn::words::{self, Four, Linh, Options, Thousand};

assert_eq!(
    words::vnd_to_words(12_345_000),
    "Mười hai triệu ba trăm bốn mươi lăm nghìn đồng"
);
assert_eq!(
    words::vnd_to_words(1_000_000_005),
    "Một tỷ không trăm linh năm đồng"
);

// Southern forms, with "chẵn" for a round amount.
let south = Options { linh: Linh::Le, thousand: Thousand::Ngan, chan: true, ..Options::default() };
assert_eq!(
    words::vnd_to_words_with(1_005_000, &south),
    "Một triệu không trăm lẻ năm ngàn đồng chẵn"
);

// "tư" instead of "bốn" after "mươi".
let tu = Options { four: Four::Tu, ..Options::default() };
assert_eq!(words::number_to_words_with(24_000, &tu), "hai mươi tư nghìn");

// Plain numbers, lower case.
assert_eq!(words::number_to_words(115), "một trăm mười lăm");
```

Invoices in a foreign currency, and adjustment invoices with a negative amount, go through
`amount_to_words`, which parses a decimal string exactly. Thousands separators are rejected
rather than guessed: "1.000.000" is an error, not one million and not one.

```rust
use dl_vn::Error;
use dl_vn::words::amount_to_words;

assert_eq!(
    amount_to_words("1234.56", "đô la Mỹ", Some(("xu", 2)))?,
    "Một nghìn hai trăm ba mươi bốn đô la Mỹ và năm mươi sáu xu"
);
assert_eq!(amount_to_words("-500000", "đồng", None)?, "Âm năm trăm nghìn đồng");
assert_eq!(
    amount_to_words("1.000.000", "đồng", None),
    Err(Error::InvalidCharacter { index: 5, ch: '.' })
);
assert_eq!(amount_to_words("10.5", "đồng", None), Err(Error::TooManyDecimals { index: 3 }));
# Ok::<(), Error>(())
```

### Slugs and search keys (`text`)

```rust
use dl_vn::text::{remove_diacritics, slugify};

assert_eq!(slugify("Khách sạn Đà Nẵng - Phòng 101!"), "khach-san-da-nang-phong-101");
assert_eq!(remove_diacritics("ĐẶNG THỊ HƯƠNG"), "DANG THI HUONG");
// Decomposed input (NFD), as produced by some keyboards and macOS file names.
assert_eq!(remove_diacritics("Nguye\u{302}\u{303}n"), "Nguyen");
```

### Sorting customer names (`text`)

`compare_vi` orders letters as a ă â b c d đ e ê g h i k l m n o ô ơ p q r s t u ư v x y,
then tones (none, huyền, hỏi, ngã, sắc, nặng), then case. Vietnamese lists are usually
sorted by given name, the last word:

```rust
use dl_vn::text::{compare_vi, normalize_name};

let mut customers = vec![
    normalize_name("đỗ minh ĐỨC"),
    normalize_name("trần văn bình"),
    normalize_name("PHẠM THỊ ÂN"),
    normalize_name("hoàng văn dũng"),
    normalize_name("nguyễn thị ánh"),
    normalize_name("lê văn an"),
];
let given = |s: &str| s.rsplit(' ').next().unwrap_or("").to_owned();
customers.sort_by(|a, b| compare_vi(&given(a), &given(b)).then_with(|| compare_vi(a, b)));
assert_eq!(
    customers,
    [
        "Lê Văn An",
        "Nguyễn Thị Ánh",
        "Phạm Thị Ân",
        "Trần Văn Bình",
        "Hoàng Văn Dũng",
        "Đỗ Minh Đức",
    ]
);
```

### Validating an MST before calling an e-invoice API (`tax`)

E-invoice providers reject a whole invoice when the buyer's tax code is wrong. Check it at
the form instead:

```rust
use dl_vn::Error;
use dl_vn::tax::TaxCode;

fn buyer_tax_code(input: &str) -> Result<String, Error> {
    // Display gives the conventional form, with a dash for branches.
    Ok(TaxCode::parse(input.trim())?.to_string())
}

assert_eq!(buyer_tax_code("0100109106")?, "0100109106"); // Viettel Group
assert_eq!(buyer_tax_code(" 0100109106475 ")?, "0100109106-475"); // a Viettel branch
assert_eq!(buyer_tax_code("0100109107"), Err(Error::Checksum));

let branch = TaxCode::parse("0100109106-475")?;
assert_eq!(branch.parent().map(|p| p.to_string()), Some("0100109106".into()));
assert_eq!(branch.branch(), Some(475));

// Individuals and household businesses: the 12-digit personal ID number.
assert!(matches!(TaxCode::parse("079203000123")?, TaxCode::PersonalId12(_)));
# Ok::<(), Error>(())
```

### Normalising phone numbers for SMS and Zalo (`phone`)

```rust
use dl_vn::phone::{Carrier, Kind, normalize_phone};

for typed in ["0912 345 678", "+84 912345678", "84912345678", "(+84)912-345-678"] {
    let p = normalize_phone(typed)?;
    assert_eq!(p.national(), "0912345678");
    assert_eq!(p.e164(), "+84912345678");
}

let p = normalize_phone("098 765 4321")?;
assert!(p.is_mobile()); // only mobiles receive SMS
assert_eq!(p.carrier(), Some(Carrier::Viettel));

let office = normalize_phone("028 3829 1234")?;
assert_eq!(office.kind(), Kind::Landline);
assert_eq!(office.area_name(), Some("Hồ Chí Minh"));

// Without a trunk 0 or a country code the number is ambiguous.
assert!(normalize_phone("912345678").is_err());
# Ok::<(), dl_vn::Error>(())
```

### Reading a CCCD number (`cccd`)

```rust
use dl_vn::cccd::{Cccd, Gender};

let id = Cccd::parse("001305000123")?;
assert_eq!(id.province(), Some("Hà Nội"));
assert_eq!(id.gender(), Gender::Female);
assert_eq!(id.birth_year(), 2005);

// Code 003 is reserved: no card carries it.
assert!(Cccd::parse("003305000123").is_err());
# Ok::<(), dl_vn::Error>(())
```

### Generating a VietQR payment payload (`vietqr`)

```rust
use dl_vn::text::remove_diacritics;
use dl_vn::vietqr::{Initiation, VietQr, bank_by_name};

let bank = bank_by_name("Vietcombank").unwrap();
let mut qr = VietQr::new(bank.bin, "1031933430");
qr.initiation = Initiation::Dynamic; // one payment
qr.amount = Some(1_250_000);
// The message must be printable ASCII, at most 25 characters.
qr.purpose = Some(remove_diacritics("Tiền phòng 101"));

let payload = qr.to_payload()?;
assert!(payload.contains("5303704540712500005802VN"));

// Reading a scanned code verifies the CRC and every field.
let scanned = VietQr::parse(&payload)?;
assert_eq!(scanned, qr);
assert_eq!(scanned.bank().map(|b| b.short_name), Some("Vietcombank"));
# Ok::<(), dl_vn::Error>(())
```

To draw the code, pass the payload to the sibling crate
[`dl-qr`](https://github.com/vzeturn/DevLibrary/tree/main/crates/dl-qr). It is not a
dependency of `dl-vn`, so this example is not compiled here:

```text
// Cargo.toml: dl-qr = { git = "https://github.com/vzeturn/DevLibrary" }
use dl_qr::{Ecc, QrCode, SvgOptions};

let code = QrCode::encode_text(&payload, Ecc::Medium)?;
let svg = code.to_svg(&SvgOptions::default()); // for a web page or a PDF invoice
print!("{}", code.to_text(4));                  // for a terminal
```

## API overview

| Module | Items |
|---|---|
| `words` | `number_to_words`, `number_to_words_with`, `vnd_to_words`, `vnd_to_words_with`, `amount_to_words`, `amount_to_words_with`, `Options { linh, thousand, billion, four, chan }`, `Linh`, `Thousand`, `Billion`, `Four` |
| `text` | `remove_diacritics`, `slugify`, `normalize_name`, `compare_vi` |
| `tax` | `TaxCode { Enterprise10(Mst), Branch13(BranchMst), PersonalId12(Cccd) }`, `TaxCode::parse`, `as_str`, `parent`, `branch`, `Mst`, `BranchMst`, `check_digit`, `is_valid_tax_code` |
| `phone` | `normalize_phone`, `Phone { national, e164, kind, is_mobile, carrier, area_code, area_name }`, `Kind`, `Carrier` |
| `cccd` | `Cccd::parse`, `province`, `place_code`, `gender`, `century`, `birth_year`, `serial`, `Gender`, `province_name` |
| `vietqr` | `VietQr { initiation, bin, account, service, amount, bill_number, purpose }`, `VietQr::new`, `to_payload`, `parse`, `bank`, `Initiation`, `Service`, `crc16`, `BANKS`, `Bank`, `bank_by_bin`, `bank_by_name`, `NAPAS_GUID`, `MAX_AMOUNT` |
| crate root | `Error` |

`TaxCode`, `Cccd`, `Phone` and `VietQr` implement `FromStr`; the first three implement
`Display`.

## Sources and regulations

| Rule | Source |
|---|---|
| Number reading: mốt, lăm, linh/lẻ, không trăm, nghìn/ngàn, tỷ, triệu tỷ, tỷ tỷ, bốn/tư | [Wikipedia, Vietnamese numerals](https://en.wikipedia.org/wiki/Vietnamese_numerals); [CLDR rbnf vi](https://github.com/unicode-org/cldr/blob/main/common/rbnf/vi.xml) for "tư" |
| Collation: letter order, tone order huyền < hỏi < ngã < sắc < nặng | [CLDR collation vi.xml](https://github.com/unicode-org/cldr/blob/main/common/collation/vi.xml) |
| MST structure and check digit (weights 31 29 23 19 17 13 7 5 3, mod 11) | [python-stdnum `vn/mst.py`](https://github.com/arthurdejong/python-stdnum/blob/master/stdnum/vn/mst.py), checked against published codes (see `tax` docs) |
| 12-digit personal ID as tax code from 1 July 2025 | Law 38/2019/QH14 Art. 35(7), Circular 86/2024/TT-BTC Art. 5, 7, 39 ([vietnam.vn](https://www.vietnam.vn/de/su-dung-so-dinh-danh-ca-nhan-thay-cho-ma-so-thue-tu-01-7-2025-luu-y-voi-nguoi-nop-thue), [baochinhphu.vn](https://en.baochinhphu.vn/citizen-identification-cards-replace-social-insurance-codes-from-august-1-11125080616115797.htm)) |
| Mobile and landline numbering, area codes, carrier codes | [Wikipedia, Telephone numbers in Vietnam](https://en.wikipedia.org/wiki/Telephone_numbers_in_Vietnam) |
| 095 allocated to Viettel (auction of 10 July 2026) | [Báo Tin tức](https://baotintuc.vn/dien-tu-vien-thong/dau-so-di-dong-095-cuoi-cung-duoc-dua-ra-dau-gia-thanh-cong-20260710191538115.htm) |
| Virtual operators iTel 087, Wintel 055, Local 089, VNSKY 077, FPT 0775 | [VnExpress International](https://e.vnexpress.net/news/tech/vietnam-innovation/new-virtual-carrier-enters-vietnam-telecom-market-5109905.html) |
| CCCD structure (Decree 137/2015/NĐ-CP), province codes (Circular 07/2016/TT-BCA) | [Wikipedia, Căn cước công dân](https://vi.wikipedia.org/wiki/Căn_cước_công_dân_(Việt_Nam)) |
| VietQR fields, lengths, ANS character set, amount format, CRC, examples | NAPAS, "Quy định về định dạng mã VietQR trong Dịch vụ Chuyển nhanh NAPAS247" v1.0, 09/2021 ([transcript](https://github.com/shinxz12/napas-qr-python/blob/main/docs/vietqr-spec.md)) |
| CRC-16/CCITT-FALSE parameters and check value `29B1` | [CRC RevEng catalogue, CRC-16/IBM-3740](https://reveng.sourceforge.io/crc-catalogue/16.htm) (alias CRC-16/CCITT-FALSE) |
| Bank BINs, codes and names | [api.vietqr.io/v2/banks](https://api.vietqr.io/v2/banks), checked on 2026-11-15 |

## Limitations

- **Carrier lookup** reports the carrier a code was allocated to. Numbers ported to another
  network keep their prefix, and virtual operators on a host's range are reported as the
  host.
- **Landlines** are validated against the area code table only; subscriber number ranges are
  not checked. Short codes, 1800/1900 numbers, the 069 security code and VSAT numbers are
  rejected.
- **CCCD** province names are the 63 provinces before the July 2025 merger into 34; no
  official mapping of new codes was found. Codes 100–999 (births registered abroad) are
  accepted without a country name, because no published country code table was found. The
  number has no check digit, so a well-formed number may not exist.
- **MST** check digit rules come from python-stdnum and published codes, not from a legal
  text; no official publication of the formula was found. Legacy 10-digit personal codes
  are reported as `Enterprise10`.
- **VietQR** handles transfers to accounts and cards (QRIBFTTA, QRIBFTTC) in đồng. Merchant
  payments with other service codes, tips and fees (fields 55–57) are not built and are
  reported as unsupported or ignored when parsed. Text fields must be printable ASCII, as
  the specification requires; codes from generators that put diacritics in the message, or
  that count lengths in UTF-8 bytes, are rejected. The NAPAS247 per-transaction limit
  (under 500 million đồng per the NAPAS terms) is not enforced.
- **Collation** covers Latin letters, digits and punctuation; other scripts sort after
  Latin letters by code point. It implements the CLDR rules directly rather than the full
  Unicode Collation Algorithm.

## License

Licensed under either of the MIT license or the Apache License, Version 2.0, at your
option, as stated in the [DevLibrary repository](https://github.com/vzeturn/DevLibrary).
