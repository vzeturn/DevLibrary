//! A Vietnamese-style sales invoice with 8% and 10% VAT, in VND.
//!
//! Shows the two common ways to compute VAT and how far apart they can be:
//! - per line: each line's VAT is rounded to whole đồng, then summed;
//! - per rate: the line amounts of each VAT rate are summed and the VAT is rounded once.
//!
//! It also spreads an invoice-level discount over the lines with `allocate`, rounds
//! the amount due for cash to 500 đồng, and splits it between three people.
//!
//! `cargo run -p dl-decimal --example vat_invoice`

use dl_decimal::{Decimal, Error, RoundingMode};

struct Line {
    name: &'static str,
    qty: Decimal,
    unit_price: Decimal,
    /// VAT rate in percent.
    vat_rate: Decimal,
}

/// Formats a VND amount the Vietnamese way: `1.234.567`.
fn vnd(x: Decimal) -> String {
    x.to_string_grouped(0, '.', ',', RoundingMode::HalfUp)
}

fn main() -> Result<(), Error> {
    let d = |s: &str| s.parse::<Decimal>();
    let lines = [
        Line {
            name: "Ca phe hat (kg)",
            qty: d("1.5")?,
            unit_price: d("333333")?,
            vat_rate: d("8")?,
        },
        Line {
            name: "Banh quy (hop)",
            qty: d("12")?,
            unit_price: d("18519")?,
            vat_rate: d("8")?,
        },
        Line {
            name: "Bia lon (thung)",
            qty: d("3")?,
            unit_price: d("312345")?,
            vat_rate: d("10")?,
        },
        Line {
            name: "Phi giao hang",
            qty: d("1")?,
            unit_price: d("45678")?,
            vat_rate: d("10")?,
        },
    ];
    // An invoice-level discount, spread over the lines in proportion to their amounts.
    let discount = Decimal::from(50_000);

    // Line amounts in whole đồng (half-up, as on the printed invoice).
    let gross: Vec<Decimal> = lines
        .iter()
        .map(|l| (l.qty * l.unit_price).round(0))
        .collect();
    let line_discounts = discount.allocate(&gross, 0)?;
    let nets: Vec<Decimal> = gross
        .iter()
        .zip(&line_discounts)
        .map(|(g, disc)| *g - *disc)
        .collect();

    println!(
        "{:<18} {:>6} {:>10} {:>11} {:>9} {:>11} {:>4} {:>9}",
        "Item", "Qty", "Price", "Amount", "Discount", "Net", "VAT", "VAT amt"
    );
    let mut vat_per_line = Decimal::ZERO;
    for ((l, g), (disc, net)) in lines
        .iter()
        .zip(&gross)
        .zip(line_discounts.iter().zip(&nets))
    {
        let vat = net.percent_of(l.vat_rate)?.round(0);
        vat_per_line += vat;
        println!(
            "{:<18} {:>6} {:>10} {:>11} {:>9} {:>11} {:>3}% {:>9}",
            l.name,
            l.qty,
            vnd(l.unit_price),
            vnd(*g),
            vnd(*disc),
            vnd(*net),
            l.vat_rate,
            vnd(vat)
        );
    }

    let net_total: Decimal = nets.iter().sum();
    let mut vat_per_rate = Decimal::ZERO;
    println!();
    for rate in [Decimal::from(8), Decimal::from(10)] {
        let base: Decimal = lines
            .iter()
            .zip(&nets)
            .filter(|(l, _)| l.vat_rate == rate)
            .map(|(_, n)| *n)
            .sum();
        let exact = base.percent_of(rate)?;
        let vat = exact.round(0);
        vat_per_rate += vat;
        println!(
            "VAT {rate:>2}% on {:>11}: exact {:>13} -> {:>9}",
            vnd(base),
            exact.to_string_grouped(2, '.', ',', RoundingMode::HalfUp),
            vnd(vat)
        );
    }

    println!();
    println!("Net total (after discount): {:>11}", vnd(net_total));
    println!("VAT rounded per line:       {:>11}", vnd(vat_per_line));
    println!("VAT rounded per rate:       {:>11}", vnd(vat_per_rate));
    let due = net_total + vat_per_rate;
    println!("Amount due (per rate):      {:>11}", vnd(due));

    let cash = due.round_to_multiple(Decimal::from(500), RoundingMode::HalfUp)?;
    println!("Cash, rounded to 500 dong:  {:>11}", vnd(cash));

    let shares = cash.split(3, 0)?;
    let shown: Vec<String> = shares.iter().map(|s| vnd(*s)).collect();
    println!("Split between 3 people:     {}", shown.join(" + "));
    assert_eq!(shares.iter().sum::<Decimal>(), cash);
    assert_eq!(line_discounts.iter().sum::<Decimal>(), discount);
    Ok(())
}
