//! `cargo bench -p dl-time`
use std::hint::black_box;

use dl_bench::Bench;
use dl_time::{Date, Duration, Locale, Offset, OffsetDateTime};

fn main() {
    let mut b = Bench::from_args();
    let ts = "2026-11-15T10:20:30.123456789+07:00";
    let t = OffsetDateTime::parse_rfc3339(ts).unwrap();
    let utc = t.to_utc();

    b.run("parse_rfc3339 (9-digit fraction)", || {
        OffsetDateTime::parse_rfc3339(black_box(ts))
    });
    b.run("parse_rfc3339 (Z, no fraction)", || {
        OffsetDateTime::parse_rfc3339(black_box("2026-11-15T03:20:30Z"))
    });
    b.run("to_rfc3339", || black_box(t).to_rfc3339());
    b.run("from_unix_seconds", || {
        OffsetDateTime::from_unix_seconds(black_box(1_794_712_830))
    });
    b.run("unix_seconds", || black_box(t).unix_seconds());
    b.run("to_offset(VIETNAM)", || {
        black_box(utc).to_offset(Offset::VIETNAM)
    });
    b.run("to_http_date", || black_box(t).to_http_date());
    b.run("parse_http_date", || {
        OffsetDateTime::parse_http_date(black_box("Sun, 06 Nov 1994 08:49:37 GMT"))
    });
    b.run("format dd/MM/yyyy HH:mm:ss", || {
        black_box(t).format("dd/MM/yyyy HH:mm:ss")
    });
    b.run("format_locale Vi dddd, 'ngay' dd/MM/yyyy", || {
        black_box(t).format_locale("dddd, 'ngày' dd/MM/yyyy", Locale::Vi)
    });
    b.run("parse dd/MM/yyyy HH:mm:ss", || {
        dl_time::DateTime::parse(black_box("15/11/2026 10:20:30"), "dd/MM/yyyy HH:mm:ss")
    });
    let d = Date::new(2026, 1, 31).unwrap();
    b.run("Date::add_months", || {
        black_box(d).add_months(black_box(13))
    });
    b.run("Date::iso_week", || black_box(d).iso_week());
    b.run("Duration::parse_iso8601", || {
        black_box("P1DT2H3M4.5S").parse::<Duration>()
    });
}
