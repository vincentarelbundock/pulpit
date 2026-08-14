//! What a runtime can actually do (`docs-src/internals.typ`).
//!
//! A runtime is chosen by capability, never by a backend name alone. An
//! executable called `chromium` that does not implement the CDP methods this
//! adapter needs is *not* a usable HTML runtime, and saying so here is what
//! keeps operating-system branches out of the rest of the code.

use std::path::PathBuf;

use pulpit_core::overlay::{ContentKind, WebRequirements};
use serde::{Deserialize, Serialize};

use crate::protocol::{CapabilityRequest, PixelFormat, RuntimeId};

/// Whether a runtime can be used at all, and why not when it cannot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Availability {
    Available,
    /// Nothing to run: no executable, no library, no plugin.
    NotInstalled {
        detail: String,
    },
    /// Installed but not usable: wrong version, missing protocol method,
    /// missing codec.
    Incompatible {
        detail: String,
    },
    /// Deliberately withheld from automatic selection until it passes the
    /// qualification gates (section 19.4).
    NotQualified {
        detail: String,
    },
    /// This build has the runtime's Cargo feature switched off.
    NotBuilt,
}

impl Availability {
    pub fn is_available(&self) -> bool {
        matches!(self, Availability::Available)
    }

    pub fn detail(&self) -> &str {
        match self {
            Availability::Available => "available",
            Availability::NotInstalled { detail }
            | Availability::Incompatible { detail }
            | Availability::NotQualified { detail } => detail,
            Availability::NotBuilt => "not built into this package",
        }
    }
}

/// A degradation the presenter is entitled to know about before going on
/// stage, even when the runtime is otherwise usable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Limitation {
    /// Renders occasional snapshots, not a continuous stream.
    SnapshotOnly,
    /// Frames arrive as compressed screencast images, not raw pixels.
    CompressedFrames {
        codec: String,
    },
    /// Audio cannot be produced.
    NoAudio,
    /// Seeking is unavailable or inaccurate.
    NoAccurateSeek,
    /// Keyboard or text input is not forwarded.
    NoKeyboard,
    /// Debugging runs over loopback TCP rather than an inherited pipe.
    TcpDebugging,
    Other {
        detail: String,
    },
}

impl std::fmt::Display for Limitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Limitation::SnapshotOnly => f.write_str("renders snapshots, not continuous animation"),
            Limitation::CompressedFrames { codec } => {
                write!(f, "frames arrive as {codec}, not raw pixels")
            }
            Limitation::NoAudio => f.write_str("cannot produce audio"),
            Limitation::NoAccurateSeek => f.write_str("cannot seek accurately"),
            Limitation::NoKeyboard => f.write_str("does not forward keyboard input"),
            Limitation::TcpDebugging => {
                f.write_str("controls the browser over loopback TCP rather than a private pipe")
            }
            Limitation::Other { detail } => f.write_str(detail),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ContentCapabilities {
    pub kinds: Vec<ContentKind>,
    /// MIME families, e.g. `image/gif`, `video/mp4`, `text/html`.
    pub mime: Vec<String>,
    pub formats: Vec<PixelFormat>,
    pub max_width: u32,
    pub max_height: u32,
    /// Can it produce a continuous stream, or only occasional snapshots?
    pub continuous_frames: bool,
}

impl ContentCapabilities {
    pub fn supports(&self, kind: ContentKind) -> bool {
        self.kinds.contains(&kind)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InputCapabilities {
    pub pointer: bool,
    pub wheel: bool,
    pub keyboard: bool,
    pub text: bool,
    pub touch: bool,
    pub focus: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlaybackCapabilities {
    pub seek: bool,
    pub accurate_seek: bool,
    pub pause: bool,
    pub looping: bool,
    pub volume: bool,
    pub audio: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RenderingCapabilities {
    /// Does the runtime work without the compositor owning a native child
    /// window? Native Wayland makes this the deciding question.
    pub wayland_independent: bool,
    pub webgl: bool,
    /// Frames arrive already decoded rather than as a compressed screencast.
    pub raw_frames: bool,
    pub device_scale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SecurityCapabilities {
    pub javascript: bool,
    /// Can the runtime actually *enforce* a network policy, rather than
    /// merely declare one?
    pub enforces_network_policy: bool,
    pub ephemeral_storage: bool,
    pub private_profile: bool,
    pub sandbox: bool,
}

/// One runtime's answer to "what can you do for this overlay?".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeProbe {
    pub id: RuntimeId,
    pub availability: Availability,
    pub version: Option<String>,
    pub executable: Option<PathBuf>,
    pub content: ContentCapabilities,
    pub input: InputCapabilities,
    pub playback: PlaybackCapabilities,
    pub rendering: RenderingCapabilities,
    pub security: SecurityCapabilities,
    pub limitations: Vec<Limitation>,
}

impl RuntimeProbe {
    /// A probe for a runtime that is not there. Every capability is false, so
    /// an unavailable runtime can never accidentally satisfy a requirement.
    pub fn unavailable(id: RuntimeId, availability: Availability) -> Self {
        Self {
            id,
            availability,
            version: None,
            executable: None,
            content: ContentCapabilities::default(),
            input: InputCapabilities::default(),
            playback: PlaybackCapabilities::default(),
            rendering: RenderingCapabilities::default(),
            security: SecurityCapabilities::default(),
            limitations: Vec::new(),
        }
    }

    pub fn is_available(&self) -> bool {
        self.availability.is_available()
    }

    pub fn has(&self, limitation: &Limitation) -> bool {
        self.limitations.contains(limitation)
    }

    /// Does this runtime satisfy everything the overlay asked for?
    ///
    /// The comparison is deliberately blunt: a runtime missing *any* required
    /// capability is skipped even when it claims the content kind, because
    /// the alternative is a browser that opens and then cannot be clicked.
    pub fn satisfies(&self, request: &CapabilityRequest) -> Result<(), UnmetRequirement> {
        if !self.is_available() {
            return Err(UnmetRequirement::Unavailable {
                detail: self.availability.detail().to_string(),
            });
        }
        for kind in &request.kinds {
            if !self.content.supports(*kind) {
                return Err(UnmetRequirement::ContentKind(*kind));
            }
        }
        if request.continuous_animation && !self.content.continuous_frames {
            return Err(UnmetRequirement::ContinuousAnimation);
        }
        if request.pointer && !self.input.pointer {
            return Err(UnmetRequirement::Pointer);
        }
        if request.keyboard && !self.input.keyboard {
            return Err(UnmetRequirement::Keyboard);
        }
        if request.webgl && !self.rendering.webgl {
            return Err(UnmetRequirement::WebGl);
        }
        if request.audio && !self.playback.audio {
            return Err(UnmetRequirement::Audio);
        }
        Ok(())
    }
}

/// Why a candidate was skipped. Kept structured so preflight can explain the
/// selection rather than just naming the winner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum UnmetRequirement {
    #[error("not available: {detail}")]
    Unavailable { detail: String },
    #[error("does not support {}", .0.label())]
    ContentKind(ContentKind),
    #[error("cannot render continuous animation")]
    ContinuousAnimation,
    #[error("does not accept pointer input")]
    Pointer,
    #[error("does not accept keyboard input")]
    Keyboard,
    #[error("does not provide WebGL")]
    WebGl,
    #[error("cannot produce audio")]
    Audio,
}

/// Turn a bundle's declared requirements into a capability request.
pub fn request_for_web(requirements: &WebRequirements, audio: bool) -> CapabilityRequest {
    CapabilityRequest {
        kinds: vec![ContentKind::Web],
        pointer: requirements.pointer,
        keyboard: requirements.keyboard,
        webgl: requirements.webgl,
        continuous_animation: requirements.continuous_animation,
        audio,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capable() -> RuntimeProbe {
        RuntimeProbe {
            id: RuntimeId::ExternalChromium,
            availability: Availability::Available,
            version: Some("Chrome/140.0.0.0".into()),
            executable: Some(PathBuf::from("/usr/bin/google-chrome")),
            content: ContentCapabilities {
                kinds: vec![ContentKind::Web],
                mime: vec!["text/html".into()],
                formats: vec![PixelFormat::Rgba8Premultiplied],
                max_width: 8192,
                max_height: 8192,
                continuous_frames: true,
            },
            input: InputCapabilities {
                pointer: true,
                wheel: true,
                keyboard: true,
                text: true,
                touch: false,
                focus: true,
            },
            playback: PlaybackCapabilities::default(),
            rendering: RenderingCapabilities {
                wayland_independent: true,
                webgl: true,
                raw_frames: false,
                device_scale: true,
            },
            security: SecurityCapabilities {
                javascript: true,
                enforces_network_policy: true,
                ephemeral_storage: true,
                private_profile: true,
                sandbox: true,
            },
            limitations: vec![Limitation::CompressedFrames {
                codec: "JPEG".into(),
            }],
        }
    }

    #[test]
    fn a_capable_runtime_satisfies_a_matching_request() {
        let request = CapabilityRequest {
            kinds: vec![ContentKind::Web],
            pointer: true,
            keyboard: true,
            webgl: true,
            continuous_animation: true,
            audio: false,
        };
        assert!(capable().satisfies(&request).is_ok());
    }

    #[test]
    fn an_unavailable_runtime_satisfies_nothing_however_it_is_asked() {
        let probe = RuntimeProbe::unavailable(
            RuntimeId::ExternalChromium,
            Availability::NotInstalled {
                detail: "no Chromium-family browser found".into(),
            },
        );
        assert!(matches!(
            probe.satisfies(&CapabilityRequest::default()),
            Err(UnmetRequirement::Unavailable { .. })
        ));
    }

    #[test]
    fn claiming_the_content_kind_is_not_enough_when_a_capability_is_missing() {
        let mut probe = capable();
        probe.input.pointer = false;
        let request = CapabilityRequest {
            kinds: vec![ContentKind::Web],
            pointer: true,
            ..Default::default()
        };
        assert_eq!(
            probe.satisfies(&request),
            Err(UnmetRequirement::Pointer),
            "a browser that cannot be clicked is not an HTML runtime"
        );
    }

    #[test]
    fn a_snapshot_runtime_is_refused_for_content_needing_animation() {
        let mut probe = capable();
        probe.content.continuous_frames = false;
        probe.limitations.push(Limitation::SnapshotOnly);
        let request = CapabilityRequest {
            kinds: vec![ContentKind::Web],
            continuous_animation: true,
            ..Default::default()
        };
        assert_eq!(
            probe.satisfies(&request),
            Err(UnmetRequirement::ContinuousAnimation)
        );

        // The same runtime is fine for a bundle that does not animate.
        let still = CapabilityRequest {
            kinds: vec![ContentKind::Web],
            continuous_animation: false,
            ..Default::default()
        };
        assert!(probe.satisfies(&still).is_ok());
    }

    #[test]
    fn the_wrong_content_kind_is_refused_by_name() {
        let request = CapabilityRequest::for_kind(ContentKind::Video);
        assert_eq!(
            capable().satisfies(&request),
            Err(UnmetRequirement::ContentKind(ContentKind::Video))
        );
    }

    #[test]
    fn a_bundles_declared_requirements_become_the_capability_request() {
        let requirements = WebRequirements {
            pointer: true,
            keyboard: true,
            webgl: true,
            continuous_animation: false,
        };
        let request = request_for_web(&requirements, false);
        assert_eq!(request.kinds, vec![ContentKind::Web]);
        assert!(request.pointer && request.keyboard && request.webgl);
        assert!(!request.continuous_animation);
        assert!(!request.audio);
    }

    #[test]
    fn audio_is_only_satisfied_by_a_runtime_that_has_it() {
        let request = CapabilityRequest {
            kinds: vec![ContentKind::Web],
            audio: true,
            ..Default::default()
        };
        assert_eq!(capable().satisfies(&request), Err(UnmetRequirement::Audio));
    }
}
