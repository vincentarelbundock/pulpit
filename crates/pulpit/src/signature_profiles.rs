//! Standalone signing-identity profiles and their editor state.
//!
//! The persisted model lives in `settings`; this module owns transient UI
//! state and the signature-pad widget. Private key bytes and passphrases never
//! enter either model.

use std::path::PathBuf;

use iced::mouse;
use iced::widget::canvas;
use iced::{Color, Point, Rectangle, Renderer, Theme};
use zeroize::Zeroize;

use crate::settings::{
    SignaturePoint, SigningIdentityProfile, StoredCredential, StoredCredentialSummary,
    StoredSignatureAppearance, StoredSignatureContent, StoredSignaturePosition,
    StoredSignatureSize,
};
use crate::theme::Palette;

pub const MAX_PROFILE_STROKES: usize = 64;
const MAX_PROFILE_POINTS_PER_STROKE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProfileSource {
    #[default]
    Create,
    Existing,
}

impl ProfileSource {
    pub const ALL: [ProfileSource; 2] = [ProfileSource::Create, ProfileSource::Existing];

    pub fn label(self) -> &'static str {
        match self {
            ProfileSource::Create => "Create new credential",
            ProfileSource::Existing => "Use existing .p12/.pfx",
        }
    }
}

#[derive(Debug)]
pub struct ProfileEditor {
    pub editing_id: Option<String>,
    pub id: String,
    pub name: String,
    pub full_name: String,
    pub organization: String,
    pub email: String,
    pub source: ProfileSource,
    pub external_path: Option<PathBuf>,
    pub passphrase: String,
    pub confirm_passphrase: String,
    pub identity: Option<StoredCredentialSummary>,
    pub appearance: StoredSignatureAppearance,
    pub busy: bool,
    pub error: Option<String>,
}

impl Drop for ProfileEditor {
    fn drop(&mut self) {
        self.passphrase.zeroize();
        self.confirm_passphrase.zeroize();
    }
}

impl ProfileEditor {
    pub fn create(id: String) -> Self {
        Self {
            editing_id: None,
            id,
            name: String::new(),
            full_name: String::new(),
            organization: String::new(),
            email: String::new(),
            source: ProfileSource::Create,
            external_path: None,
            passphrase: String::new(),
            confirm_passphrase: String::new(),
            identity: None,
            appearance: StoredSignatureAppearance::default(),
            busy: false,
            error: None,
        }
    }

    pub fn edit(profile: &SigningIdentityProfile) -> Self {
        Self {
            editing_id: Some(profile.id.clone()),
            id: profile.id.clone(),
            name: profile.name.clone(),
            full_name: common_name(&profile.identity.subject).to_string(),
            organization: String::new(),
            email: String::new(),
            source: match profile.credential {
                StoredCredential::Managed => ProfileSource::Create,
                StoredCredential::External { .. } => ProfileSource::Existing,
            },
            external_path: match &profile.credential {
                StoredCredential::Managed => None,
                StoredCredential::External { path } => Some(path.clone()),
            },
            passphrase: String::new(),
            confirm_passphrase: String::new(),
            identity: Some(profile.identity.clone()),
            appearance: profile.appearance.clone(),
            busy: false,
            error: None,
        }
    }

    pub fn is_editing(&self) -> bool {
        self.editing_id.is_some()
    }

    pub fn validate_for_save(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Give this signature profile a name.".to_string());
        }
        if self.is_editing() {
            return Ok(());
        }
        if self.source == ProfileSource::Create && self.full_name.trim().is_empty() {
            return Err("Enter the name to put in the signing certificate.".to_string());
        }
        if self.source == ProfileSource::Existing && self.external_path.is_none() {
            return Err("Choose a .p12 or .pfx credential file.".to_string());
        }
        if self.source == ProfileSource::Create && self.passphrase.is_empty() {
            return Err("Enter the credential passphrase.".to_string());
        }
        if self.source == ProfileSource::Create && self.passphrase != self.confirm_passphrase {
            return Err("The two passphrases do not match.".to_string());
        }
        if matches!(
            self.appearance.content,
            StoredSignatureContent::Ink | StoredSignatureContent::InkAndText
        ) && self.appearance.strokes.is_empty()
        {
            return Err("Draw a signature, or choose the name-and-date appearance.".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum ProfileMsg {
    StartAdd,
    StartEdit(String),
    CancelEdit,
    NameChanged(String),
    FullNameChanged(String),
    OrganizationChanged(String),
    EmailChanged(String),
    SourceChanged(ProfileSource),
    ChooseExternal,
    ExternalChosen(Option<PathBuf>),
    PassphraseChanged(String),
    ConfirmPassphraseChanged(String),
    ContentChanged(StoredSignatureContent),
    VisibleChanged(bool),
    PositionChanged(StoredSignaturePosition),
    SizeChanged(StoredSignatureSize),
    StrokeCommitted(Vec<SignaturePoint>),
    ClearInk,
    Save,
    CreationFinished(Box<Result<SigningIdentityProfile, String>>),
    SetDefault(String),
    AskRemove(String),
    CancelRemove,
    ConfirmRemove { delete_credential: bool },
}

#[derive(Debug, Clone)]
pub struct ProfileRemoval {
    pub id: String,
    pub error: Option<String>,
}

pub fn random_profile_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| format!("could not generate a profile identifier: {error}"))?;
    Ok(hex_lower(&bytes))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 0x0f) as usize] as char);
    }
    text
}

pub fn stored_summary(summary: pulpit_render::sign::CredentialSummary) -> StoredCredentialSummary {
    StoredCredentialSummary {
        subject: summary.subject,
        issuer: summary.issuer,
        serial: summary.serial,
        // Settings stores the validity bounds as display text — the same
        // shape the Sign flow's own caption uses — rather than the unix
        // seconds `CredentialSummary` now carries, so it is formatted once,
        // here, on the way in.
        not_before: crate::signing::format_validity_bound(summary.not_before),
        not_after: crate::signing::format_validity_bound(summary.not_after),
        sha256_fingerprint: summary.sha256_fingerprint,
        key_algorithm: summary.key_algorithm,
        key_bits: summary.key_bits,
    }
}

/// The `CN=` component of `subject`. See
/// [`crate::signing::subject_common_name`], which does the same best-effort
/// split — kept in one place after the two drifted into identical bodies
/// under different names (§80.8).
pub fn common_name(subject: &str) -> &str {
    crate::signing::subject_common_name(subject)
}

impl StoredSignatureContent {
    pub const ALL: [StoredSignatureContent; 3] = [
        StoredSignatureContent::InkAndText,
        StoredSignatureContent::Ink,
        StoredSignatureContent::Text,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StoredSignatureContent::Ink => "Ink only",
            StoredSignatureContent::Text => "Name and date",
            StoredSignatureContent::InkAndText => "Ink, name and date",
        }
    }

    pub fn uses_ink(self) -> bool {
        matches!(self, Self::Ink | Self::InkAndText)
    }
}

impl StoredSignaturePosition {
    pub const ALL: [StoredSignaturePosition; 5] = [
        StoredSignaturePosition::TopLeft,
        StoredSignaturePosition::TopRight,
        StoredSignaturePosition::BottomLeft,
        StoredSignaturePosition::BottomRight,
        StoredSignaturePosition::Center,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StoredSignaturePosition::TopLeft => "Top left",
            StoredSignaturePosition::TopRight => "Top right",
            StoredSignaturePosition::BottomLeft => "Bottom left",
            StoredSignaturePosition::BottomRight => "Bottom right",
            StoredSignaturePosition::Center => "Center",
        }
    }
}

impl StoredSignatureSize {
    pub const ALL: [StoredSignatureSize; 3] = [Self::Small, Self::Medium, Self::Large];

    pub fn label(self) -> &'static str {
        match self {
            StoredSignatureSize::Small => "Small",
            StoredSignatureSize::Medium => "Medium",
            StoredSignatureSize::Large => "Large",
        }
    }
}

#[derive(Debug, Default)]
pub struct PadState {
    drawing: bool,
    current: Vec<SignaturePoint>,
}

/// End the stroke in progress and publish it, if it drew anything.
///
/// A finger lifting and a mouse button releasing are the same gesture ending;
/// only the event that reports it differs.
fn commit_stroke(state: &mut PadState) -> Option<iced::widget::Action<crate::app::Message>> {
    state.drawing = false;
    let stroke = std::mem::take(&mut state.current);
    (!stroke.is_empty()).then(|| {
        iced::widget::Action::publish(crate::app::Message::SignatureProfile(
            ProfileMsg::StrokeCommitted(stroke),
        ))
        .and_capture()
    })
}

pub struct SignaturePad<'a> {
    pub strokes: &'a [Vec<SignaturePoint>],
    pub stroke_width: f32,
    pub palette: Palette,
}

impl canvas::Program<crate::app::Message> for SignaturePad<'_> {
    type State = PadState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<iced::widget::Action<crate::app::Message>> {
        use iced::widget::Action;
        match event {
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let position = cursor.position_in(bounds)?;
                state.drawing = true;
                state.current = vec![normalise(position, bounds)];
                Some(Action::request_redraw().and_capture())
            }
            iced::Event::Mouse(mouse::Event::CursorMoved { .. }) if state.drawing => {
                if let Some(position) = cursor.position_in(bounds) {
                    push_distinct(&mut state.current, normalise(position, bounds));
                }
                Some(Action::request_redraw().and_capture())
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.drawing =>
            {
                commit_stroke(state)
            }
            iced::Event::Touch(iced::touch::Event::FingerPressed { position, .. })
                if bounds.contains(*position) =>
            {
                state.drawing = true;
                state.current = vec![normalise(*position, bounds)];
                Some(Action::request_redraw().and_capture())
            }
            iced::Event::Touch(iced::touch::Event::FingerMoved { position, .. })
                if state.drawing =>
            {
                push_distinct(&mut state.current, normalise(*position, bounds));
                Some(Action::request_redraw().and_capture())
            }
            iced::Event::Touch(
                iced::touch::Event::FingerLifted { .. } | iced::touch::Event::FingerLost { .. },
            ) if state.drawing => commit_stroke(state),
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), self.palette.surface);
        let color = self.palette.text;
        for stroke in self.strokes.iter().chain(std::iter::once(&state.current)) {
            draw_stroke(&mut frame, stroke, self.stroke_width, color, bounds);
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }
}

fn normalise(position: Point, bounds: Rectangle) -> SignaturePoint {
    SignaturePoint {
        x: ((position.x - bounds.x) / bounds.width.max(1.0)).clamp(0.0, 1.0),
        // PDF appearance coordinates originate at the bottom-left.
        y: (1.0 - (position.y - bounds.y) / bounds.height.max(1.0)).clamp(0.0, 1.0),
    }
}

fn push_distinct(stroke: &mut Vec<SignaturePoint>, point: SignaturePoint) {
    if stroke.len() >= MAX_PROFILE_POINTS_PER_STROKE {
        return;
    }
    let distinct = stroke.last().is_none_or(|last| {
        let dx = point.x - last.x;
        let dy = point.y - last.y;
        dx * dx + dy * dy >= 0.000_004
    });
    if distinct {
        stroke.push(point);
    }
}

fn draw_stroke(
    frame: &mut canvas::Frame,
    stroke: &[SignaturePoint],
    width: f32,
    color: Color,
    bounds: Rectangle,
) {
    let Some(first) = stroke.first() else {
        return;
    };
    let path = canvas::Path::new(|builder| {
        builder.move_to(denormalise(*first, bounds));
        for point in stroke.iter().skip(1) {
            builder.line_to(denormalise(*point, bounds));
        }
    });
    frame.stroke(
        &path,
        canvas::Stroke::default()
            .with_color(color)
            .with_width(width.max(1.0))
            .with_line_cap(canvas::LineCap::Round)
            .with_line_join(canvas::LineJoin::Round),
    );
}

fn denormalise(point: SignaturePoint, bounds: Rectangle) -> Point {
    Point::new(point.x * bounds.width, (1.0 - point.y) * bounds.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_profile_ids_are_valid_and_distinct() {
        let first = random_profile_id().expect("random id");
        let second = random_profile_id().expect("random id");
        assert_ne!(first, second);
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn creation_validation_keeps_keys_out_of_incomplete_profiles() {
        let mut editor = ProfileEditor::create("0".repeat(32));
        assert!(editor.validate_for_save().is_err());
        editor.name = "Personal".into();
        editor.full_name = "Ada Lovelace".into();
        editor.passphrase = "secret".into();
        editor.confirm_passphrase = "different".into();
        assert_eq!(
            editor.validate_for_save().unwrap_err(),
            "The two passphrases do not match."
        );
    }

    #[test]
    fn an_existing_container_may_use_an_empty_passphrase() {
        let mut editor = ProfileEditor::create("0".repeat(32));
        editor.name = "Imported".into();
        editor.source = ProfileSource::Existing;
        editor.external_path = Some(PathBuf::from("identity.p12"));
        assert!(editor.validate_for_save().is_ok());
    }
}
