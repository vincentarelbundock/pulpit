//! Embed the Windows resources into `pulpit.exe`.
//!
//! An icon and a version block are part of application identity on this
//! desktop in the way a `.desktop` file is on Linux: without them Explorer,
//! the taskbar and the Add/Remove Programs list all show a generic stub. The
//! installer can set a shortcut's icon, but only the embedded one reaches the
//! taskbar and the running window.
//!
//! The `cfg` below is the *host*, not the target: a build script is compiled
//! for the machine running it, and `[target.'cfg(target_os = "windows")'
//! .build-dependencies]` in the manifest is matched the same way. The two
//! therefore agree. Cross-compiling to Windows from elsewhere produces a
//! working executable with no embedded icon; the release builds on Windows,
//! so the shipped one has it.
//!
//! A missing resource compiler warns rather than fails. An icon is not worth
//! being able to break somebody's build over.

fn main() {
    println!("cargo:rerun-if-changed=../../packaging/pulpit.ico");

    #[cfg(target_os = "windows")]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../packaging/pulpit.ico");
        resource.set("ProductName", "Pulpit");
        resource.set("FileDescription", "Pulpit — a PDF presenter");
        resource.set("CompanyName", "Vincent Arel-Bundock");
        resource.set(
            "LegalCopyright",
            "MIT OR Apache-2.0; see the bundled licences",
        );
        if let Err(e) = resource.compile() {
            // Warn rather than fail: a build without an icon is worse-looking,
            // not broken, and refusing here would make the resource compiler a
            // hard build dependency on every contributor's machine.
            println!("cargo:warning=could not embed Windows resources: {e}");
        }
    }
}
