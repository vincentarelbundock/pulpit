//! Presentation properties that genuinely apply to every widget.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Variant {
    #[default]
    Standard,
    Compact,
    Prominent,
}

impl Variant {
    #[allow(dead_code)] // see widgets::tokens
    pub const ALL: [Variant; 3] = [Variant::Standard, Variant::Compact, Variant::Prominent];

    #[allow(dead_code)] // unreached, including by its own tests — SPEC-simplify.md §69
    pub fn label(self) -> &'static str {
        match self {
            Variant::Standard => "Standard",
            Variant::Compact => "Compact",
            Variant::Prominent => "Prominent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Align {
    Start,
    #[default]
    Center,
    End,
}

impl Align {
    #[allow(dead_code)] // see widgets::tokens
    pub const ALL: [Align; 3] = [Align::Start, Align::Center, Align::End];

    #[allow(dead_code)] // unreached, including by its own tests — SPEC-simplify.md §69
    pub fn label(self) -> &'static str {
        match self {
            Align::Start => "Start",
            Align::Center => "Centre",
            Align::End => "End",
        }
    }
}

/// The narrowest and widest a widget may be scaled. Enforced in the model,
/// not merely by the slider that usually sets it.
pub const SCALE_RANGE: (f32, f32) = (0.5, 2.0);
