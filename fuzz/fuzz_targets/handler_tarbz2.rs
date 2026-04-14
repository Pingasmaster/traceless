#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    traceless_fuzz::fuzz_handler("application/x-bzip2", "tar.bz2", data);
});
