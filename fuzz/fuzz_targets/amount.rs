#![no_main]
use libfuzzer_sys::fuzz_target;
use protocol_core::amount::{format_amount, parse_amount};

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        if let Ok(v) = parse_amount(s) {
            assert_eq!(parse_amount(&format_amount(v)), Ok(v));
        }
    }
});
