#![no_main]

use libfuzzer_sys::fuzz_target;
use pulpit_render::verify::{
    check_signature, ByteRange, ContentsExtent, SignatureCoverage, StructuralReport,
};

fuzz_target!(|data: &[u8]| {
    // Create a minimal StructuralReport to directly call check_signature
    // on arbitrary bytes. This tests CMS parsing with a doctored contents blob.
    // Must not panic; errors are typed.

    // Use the data's length as fake extents to ensure the test exercises
    // edge cases in CMS extraction and parsing.
    let len = data.len() as u64;
    let half = len / 2;

    let report = StructuralReport {
        field_name: "Sig".to_string(),
        coverage: SignatureCoverage::Unclear,
        later_revisions: false,
        contents_extent: ContentsExtent {
            c_start: half.saturating_sub(10),
            c_end: half.saturating_add(10),
        },
        byte_range: ByteRange {
            z: 0,
            len1: half,
            start2: half,
            len2: 0,
        },
        sig_dict_revision: 0,
        declared_docmdp: None,
        sub_filter: Some("adbe.pkcs7.detached".to_string()),
        mod_date: None,
    };

    let _result = check_signature(data, &report);
});
