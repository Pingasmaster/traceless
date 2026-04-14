#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    traceless_fuzz::fuzz_handler("image/tiff", "tiff", data);
});
