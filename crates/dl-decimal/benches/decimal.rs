//! `cargo bench -p dl-decimal`
use dl_bench::Bench;
use dl_decimal::{Decimal, RoundingMode};
use std::fmt::Write;
use std::hint::black_box;

fn main() {
    let mut b = Bench::from_args();
    let money: Decimal = "1234567.89".parse().unwrap();
    let qty: Decimal = "3.5".parse().unwrap();
    let long: Decimal = "12345678901234567890.123456789012345678".parse().unwrap();
    let third = Decimal::ONE / Decimal::from(3);

    b.run("parse \"1234567.89\"", || {
        black_box("1234567.89").parse::<Decimal>()
    });
    b.run("parse 38 digits", || {
        black_box("12345678901234567890.123456789012345678").parse::<Decimal>()
    });
    b.run("to_string 1234567.89", || black_box(money).to_string());
    b.run("to_string 38 digits", || black_box(long).to_string());
    // Formatting alone, without the heap allocation of `to_string`.
    let mut out = String::with_capacity(64);
    b.run("write! 1234567.89 (reused buffer)", || {
        out.clear();
        let _ = write!(out, "{}", black_box(money));
        out.len()
    });
    let mut out = String::with_capacity(64);
    b.run("write! 38 digits (reused buffer)", || {
        out.clear();
        let _ = write!(out, "{}", black_box(long));
        out.len()
    });
    b.run("to_string_grouped VN", || {
        black_box(money).to_string_grouped(2, '.', ',', RoundingMode::HalfUp)
    });
    b.run("add same scale", || black_box(money) + black_box(money));
    b.run("add mixed scale", || black_box(money) + black_box(qty));
    b.run("add 38 digits (rounds)", || {
        black_box(long) + black_box(third)
    });
    b.run("mul money", || black_box(money) * black_box(qty));
    b.run("mul 38 digits (rounds)", || {
        black_box(long) * black_box(third)
    });
    b.run("div money / 3.5", || black_box(money) / black_box(qty));
    b.run("div 38 digits", || black_box(long) / black_box(third));
    b.run("div_round 2 dp HalfUp", || {
        black_box(money).div_round(black_box(qty), 2, RoundingMode::HalfUp)
    });
    b.run("round_dp 28 -> 2", || {
        black_box(third).round_dp(2, RoundingMode::HalfUp)
    });
    b.run("round_dp 2 -> 0", || {
        black_box(money).round_dp(0, RoundingMode::HalfEven)
    });
    b.run("cmp mixed scale", || black_box(money) < black_box(qty));
}
