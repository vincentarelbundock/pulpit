#![no_main]

use libfuzzer_sys::fuzz_target;
use pulpit_render::verify::RevisionMap;

fuzz_target!(|data: &[u8]| {
    // RevisionMap::build must return Ok or typed Err, never panic.
    let _result = RevisionMap::build(data);
});
