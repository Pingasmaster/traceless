#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    traceless_fuzz::fuzz_handler("audio/aiff", "aiff", data);
});
