//! Write the AcroForm corpus to a directory, so the cases can be opened in a
//! real viewer when a test disagrees with one.
//!
//! ```text
//! cargo run -p pulpit-render --example dump-corpus -- /tmp/corpus
//! ```

#[path = "../tests/testkit/mod.rs"]
mod testkit;

use std::path::PathBuf;

fn main() {
    let Some(directory) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: pulpit-dump-corpus <directory>");
        std::process::exit(2);
    };
    std::fs::create_dir_all(&directory).expect("cannot create the output directory");

    let mut index = String::from(
        "# pulpit AcroForm corpus\n\nEach file is wrong, or unusual, in one named way.\n\n",
    );
    let cases = testkit::corpus();
    for case in &cases {
        let path = directory.join(format!("{}.pdf", case.name));
        std::fs::write(&path, &case.bytes).expect("cannot write a case");
        index.push_str(&format!(
            "## {}\n\n{}\n\nExpectation: {:?}\n\n",
            case.name, case.note, case.expect
        ));
    }
    std::fs::write(directory.join("README.md"), index).expect("cannot write the index");
    println!("wrote {} cases to {}", cases.len(), directory.display());
}
