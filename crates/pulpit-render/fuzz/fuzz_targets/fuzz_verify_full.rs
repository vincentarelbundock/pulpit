#![no_main]

use libfuzzer_sys::fuzz_target;
use pulpit_render::verify::verify_signatures;

fuzz_target!(|data: &[u8]| {
    // verify_signatures is the main entry point covering coverage
    // classification + CMS parsing paths. Must not panic; errors are typed.
    let _result = verify_signatures(data);
});
