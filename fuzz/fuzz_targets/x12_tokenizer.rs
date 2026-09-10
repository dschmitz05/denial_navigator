#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = std::str::from_utf8(data) {
        // The contract is non-panicking rejection or a normalized parse; this
        // target intentionally asserts neither particular outcome.
        let _ = ediparser::x835::parse(input);
    }
});
