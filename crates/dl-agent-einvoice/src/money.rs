//! Exact money arithmetic shared by the POS and FO paths: a port of `apportion`,
//! `deriveLegalRate`, `recalcTaxIntoSC` and friends. All integer math; results must
//! match the Go agent to the đồng.

/// The legal Vietnam VAT bands the recalc trusts.
pub const LEGAL_RATES: [i64; 4] = [0, 5, 8, 10];

/// `abs64`: |v|, wrapping at `i64::MIN` as Go's does.
pub fn abs64(v: i64) -> i64 {
    v.wrapping_abs()
}

/// Splits `total` over `weights` with largest-remainder rounding, so Σ shares == total.
/// Same algorithm and tie-breaking as Go's `apportion` (truncate toward zero, then hand
/// leftover units to the largest remainders, lower index first on ties).
pub fn apportion(total: i64, weights: &[i64]) -> Result<Vec<i64>, String> {
    let n = weights.len();
    let mut shares = vec![0i64; n];
    let w_sum: i64 = weights.iter().fold(0i64, |a, &w| a.wrapping_add(w));
    if w_sum == 0 {
        if total != 0 {
            return Err(format!("cannot apportion {total} over zero total weight"));
        }
        return Ok(shares);
    }
    let big_w = i128::from(w_sum);
    let mut rems = vec![0i128; n];
    let mut assigned = 0i64;
    for (i, &w) in weights.iter().enumerate() {
        let prod = i128::from(total) * i128::from(w);
        let q = prod / big_w; // truncates toward zero, like big.Int.QuoRem
        let r = prod % big_w;
        shares[i] = q as i64;
        assigned = assigned.wrapping_add(shares[i]);
        rems[i] = r.abs();
    }
    let mut residual = total.wrapping_sub(assigned);
    let mut step = 1i64;
    if residual < 0 {
        step = -1;
        residual = -residual;
    }
    let mut order: Vec<usize> = (0..n).collect();
    // The Go code's exchange sort, reproduced exactly (it is not a stable sort).
    for a in 0..n {
        for b in a + 1..n {
            if rems[order[b]] > rems[order[a]] {
                order.swap(a, b);
            }
        }
    }
    for k in 0..residual {
        let idx = order[(k as usize) % n];
        shares[idx] += step;
    }
    Ok(shares)
}

/// The legal VAT percent {0,5,8,10} from a row's raw |tax|/|net| ratio, with the 1 VND
/// lower-edge margin of the Go rule. Out-of-band ratios are errors.
pub fn derive_legal_rate(base_sub: i64, base_tax: i64) -> Result<i64, String> {
    let an = abs64(base_sub);
    let at = abs64(base_tax);
    if an == 0 {
        if at != 0 {
            return Err(format!("zero net but non-zero tax {base_tax}"));
        }
        return Ok(0);
    }
    if at.wrapping_mul(1000) < an {
        return Ok(0);
    }
    const ROUND_VND: i64 = 100;
    for r in [5i64, 8, 10] {
        let lo = (100i64.wrapping_mul(at)).wrapping_add(ROUND_VND) >= r.wrapping_mul(an);
        let hi = 10000i64.wrapping_mul(at) <= r.wrapping_mul(an).wrapping_mul(115);
        if lo && hi {
            return Ok(r);
        }
    }
    Err(format!(
        "VAT ratio {at}/{an} not in legal band {{0,5,8,10}}"
    ))
}

/// round(v × num / den), half-up, symmetric for negatives.
pub fn round_mul_div_half_up(v: i64, num: i64, den: i64) -> i64 {
    if v < 0 {
        return -round_mul_div_half_up(-v, num, den);
    }
    (v.wrapping_mul(num) + den / 2) / den
}

/// Recomputes VAT to the legal rate from the tax-inclusive gross and moves the delta out
/// of service charge, keeping net + SC + tax unchanged. Fails when SC would go negative.
pub fn recalc_tax_into_sc(
    net: i64,
    tax_old: i64,
    sc_old: i64,
    rate: i64,
) -> Result<(i64, i64), String> {
    if rate == 0 {
        return Ok((tax_old, sc_old));
    }
    let gross = net + sc_old + tax_old;
    let tax_new = round_mul_div_half_up(gross, rate, 100 + rate);
    let sc_new = gross - net - tax_new;
    if sc_new < 0 {
        return Err(format!(
            "service charge cannot absorb tax delta (net {net}, rate {rate}%, gross {gross}, tax {tax_old}→{tax_new}): scNew {sc_new}"
        ));
    }
    Ok((tax_new, sc_new))
}

/// A TaxDef.TaxRate label ("0.080000") as a trusted legal percent, or `None`.
pub fn parse_rate_label(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let f: f64 = parse_go_float(s)?;
    let rate = (f * 100.0 + 0.5) as i64;
    LEGAL_RATES.contains(&rate).then_some(rate)
}

/// The decimal forms Go's `strconv.ParseFloat` accepts that matter here (digits, sign,
/// fraction, exponent). Hex floats, `inf` and `nan` are refused, which only turns a junk
/// label into "no trusted label" — the same outcome as Go for any label SMILE stores.
fn parse_go_float(s: &str) -> Option<f64> {
    let ok = s
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E'));
    if !ok {
        return None;
    }
    s.parse().ok()
}

/// `strconv.Atoi` for the rate labels POS writes ("8", "10").
pub fn atoi(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let digits = match b.first() {
        Some(b'+') | Some(b'-') => &b[1..],
        _ => b,
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apportion_exact_sum() {
        assert_eq!(apportion(100, &[1, 1, 1]).unwrap(), vec![34, 33, 33]);
        assert_eq!(apportion(-100, &[1, 1, 1]).unwrap(), vec![-34, -33, -33]);
        assert_eq!(apportion(0, &[0, 0]).unwrap(), vec![0, 0]);
        assert!(apportion(5, &[0, 0]).is_err());
        let s = apportion(25200, &[100000, 100000, 100000]).unwrap();
        assert_eq!(s.iter().sum::<i64>(), 25200);
    }

    #[test]
    fn legal_rate_bands() {
        assert_eq!(derive_legal_rate(17146080, 1371686).unwrap(), 8);
        assert_eq!(derive_legal_rate(105000, 8400).unwrap(), 8);
        assert_eq!(derive_legal_rate(100, 0).unwrap(), 0);
        assert!(derive_legal_rate(100, 30).is_err());
        assert!(derive_legal_rate(0, 1).is_err());
    }

    #[test]
    fn recalc_and_labels() {
        assert_eq!(
            recalc_tax_into_sc(100000, 8000, 5000, 8).unwrap(),
            (8370, 4630)
        );
        assert_eq!(parse_rate_label("0.080000"), Some(8));
        assert_eq!(parse_rate_label(" 0.000000 "), Some(0));
        assert_eq!(parse_rate_label("0.200000"), None);
        assert_eq!(atoi("10"), Some(10));
        assert_eq!(atoi("V8"), None);
    }
}
