//! Fixed-point amounts. All token amounts are `u64` micro-units (6 decimals).
//! Floating point is never used for money, only for analytics such as health factor.

pub const MICRO: u64 = 1_000_000;

/// Parse a decimal string such as `100`, `100.5` or `0.000001` into micro-units.
pub fn parse_amount(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("amount is empty".into());
    }
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(format!("invalid amount '{s}'"));
    }
    let digits = |p: &str| p.chars().all(|c| c.is_ascii_digit());
    if !digits(int_part) || !digits(frac_part) {
        return Err(format!("invalid amount '{s}' (digits and one '.' only)"));
    }
    if frac_part.len() > 6 {
        return Err(format!("amount '{s}' has more than 6 decimal places"));
    }
    let int: u64 = if int_part.is_empty() {
        0
    } else {
        int_part
            .parse()
            .map_err(|_| format!("amount '{s}' is too large"))?
    };
    let mut frac_str = frac_part.to_string();
    while frac_str.len() < 6 {
        frac_str.push('0');
    }
    let frac: u64 = frac_str
        .parse()
        .map_err(|_| format!("invalid amount '{s}'"))?;
    int.checked_mul(MICRO)
        .and_then(|v| v.checked_add(frac))
        .ok_or_else(|| format!("amount '{s}' is too large"))
}

/// Format micro-units as a decimal string with trailing zeros trimmed.
pub fn format_amount(v: u64) -> String {
    let int = v / MICRO;
    let frac = v % MICRO;
    if frac == 0 {
        int.to_string()
    } else {
        let f = format!("{frac:06}");
        format!("{int}.{}", f.trim_end_matches('0'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_integers_and_decimals() {
        assert_eq!(parse_amount("100").unwrap(), 100_000_000);
        assert_eq!(parse_amount("100.5").unwrap(), 100_500_000);
        assert_eq!(parse_amount("0.000001").unwrap(), 1);
        assert_eq!(parse_amount(".5").unwrap(), 500_000);
        assert_eq!(parse_amount(" 7 ").unwrap(), 7_000_000);
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [
            "",
            ".",
            "-1",
            "1e3",
            "1.2.3",
            "abc",
            "1.0000001",
            "18446744073709551615",
        ] {
            assert!(parse_amount(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn formats_round_trip() {
        for s in ["0", "1", "100.5", "0.000001", "123456.789"] {
            assert_eq!(format_amount(parse_amount(s).unwrap()), s);
        }
    }
}
