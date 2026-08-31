//! A deterministic platform for tests and for desktops with no adapter yet.
//!
//! It claims nothing, records everything, and can be told to pretend a
//! capability exists so the application's fallback paths can be exercised
//! from a unit test.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::platform::appearance::SystemAppearance;
use crate::platform::capabilities::Capabilities;
use crate::platform::inhibit::{InhibitState, InhibitToken};
use crate::platform::paths::Directories;
use crate::platform::services::PlatformServices;
use crate::platform::Outcome;

#[derive(Debug)]
#[allow(dead_code)] // reached by its tests, not by the application
pub struct NullPlatform {
    name: &'static str,
    capabilities: Capabilities,
    appearance: SystemAppearance,
    inhibition_works: bool,
    directories: Option<Directories>,
    /// Everything that was asked of the platform, in order.
    calls: Mutex<Vec<String>>,
    inhibit_calls: AtomicUsize,
    release_calls: AtomicUsize,
}

impl NullPlatform {
    #[allow(dead_code)] // reached by its tests, not by the application
    pub fn new(name: &'static str) -> NullPlatform {
        NullPlatform {
            name,
            capabilities: Capabilities {
                backend: name.to_string(),
                ..Capabilities::default()
            },
            appearance: SystemAppearance::Unknown,
            inhibition_works: false,
            directories: None,
            calls: Mutex::new(Vec::new()),
            inhibit_calls: AtomicUsize::new(0),
            release_calls: AtomicUsize::new(0),
        }
    }

    #[allow(dead_code)] // unreached, including by its own tests
    pub fn with_capabilities(mut self, capabilities: Capabilities) -> NullPlatform {
        self.capabilities = capabilities;
        self
    }

    #[allow(dead_code)] // reached by its tests, not by the application
    pub fn with_appearance(mut self, appearance: SystemAppearance) -> NullPlatform {
        self.appearance = appearance;
        self.capabilities.system_appearance = appearance != SystemAppearance::Unknown;
        self
    }

    #[allow(dead_code)] // reached by its tests, not by the application
    pub fn holding_inhibition(mut self) -> NullPlatform {
        self.inhibition_works = true;
        self.capabilities.sleep_inhibition = true;
        self
    }

    #[allow(dead_code)] // unreached, including by its own tests
    pub fn rooted_at(mut self, root: &Path) -> NullPlatform {
        self.directories = Some(Directories::under(root));
        self
    }

    /// Everything the application asked the platform to do.
    #[allow(dead_code)] // reached by its tests, not by the application
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    #[allow(dead_code)] // reached by its tests, not by the application
    pub fn inhibit_calls(&self) -> usize {
        self.inhibit_calls.load(Ordering::Relaxed)
    }

    #[allow(dead_code)] // reached by its tests, not by the application
    pub fn release_calls(&self) -> usize {
        self.release_calls.load(Ordering::Relaxed)
    }

    #[allow(dead_code)] // reached by its tests, not by the application
    fn record(&self, call: impl Into<String>) {
        self.calls.lock().unwrap().push(call.into());
    }
}

impl PlatformServices for NullPlatform {
    fn name(&self) -> &'static str {
        self.name
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }

    fn directories(&self) -> Directories {
        self.directories.clone().unwrap_or_else(Directories::detect)
    }

    fn system_appearance(&self) -> SystemAppearance {
        self.appearance
    }

    fn open(&self, target: &str) -> Outcome {
        self.record(format!("open {target}"));
        Outcome::Unsupported {
            what: "Opening a link",
        }
    }

    /// Recorded, and never sent anywhere. The null adapter exists so a test
    /// can watch what the application asked for; a printer is the last thing
    /// it should be able to reach.
    fn print(&self, job: &crate::platform::services::PrintJob) -> Outcome {
        self.record(format!("print {}", job.file.display()));
        Outcome::Unsupported { what: "Printing" }
    }

    fn inhibit(&self) -> InhibitState {
        self.inhibit_calls.fetch_add(1, Ordering::Relaxed);
        self.record("inhibit");
        if self.inhibition_works {
            InhibitState::Held {
                mechanism: "null",
                token: InhibitToken::None,
            }
        } else {
            InhibitState::Unavailable {
                reason: "this platform adapter cannot inhibit sleep".into(),
                attempts: vec!["null: not implemented".into()],
            }
        }
    }

    fn release_inhibit(&self, _state: &InhibitState) -> Outcome {
        self.release_calls.fetch_add(1, Ordering::Relaxed);
        self.record("release");
        Outcome::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn unsupported_operations_are_refused_not_silently_dropped() {
        let platform = NullPlatform::new("test");
        assert!(matches!(
            platform.open("https://example.invalid"),
            Outcome::Unsupported { .. }
        ));
        assert!(matches!(
            platform.print(&crate::platform::services::PrintJob {
                file: PathBuf::from("/tmp/deck.pdf"),
                title: "deck".into(),
                pages: Vec::new(),
                copies: 1,
                destination: None,
            }),
            Outcome::Unsupported { .. }
        ));
        // Nothing reached a printer, and the attempt is on the record.
        assert_eq!(platform.calls().len(), 2, "every request is recorded");
    }

    #[test]
    fn capabilities_can_be_posed_for_a_test() {
        let platform = NullPlatform::new("test")
            .with_appearance(SystemAppearance::Light)
            .holding_inhibition();
        assert!(platform.capabilities().system_appearance);
        assert!(platform.capabilities().sleep_inhibition);
        assert_eq!(platform.system_appearance(), SystemAppearance::Light);
    }
}
