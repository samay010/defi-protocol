#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        if let Ok(a) = protocol_core::types::Action::from_line(s) {
            let b = protocol_core::types::Action::from_line(&a.to_line()).expect("round trip");
            assert_eq!(a.amount, b.amount);
        }
    }
});
