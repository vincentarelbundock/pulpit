#![no_main]

use libfuzzer_sys::fuzz_target;
use pulpit_render::verify::{discover_signatures, RevisionMap};

fuzz_target!(|data: &[u8]| {
    // Build revision map and discover signatures on arbitrary bytes.
    // Must not panic; errors are typed.
    if let Ok(revisions) = RevisionMap::build(data) {
        let _result = discover_signatures(data, &revisions);
    }
});
