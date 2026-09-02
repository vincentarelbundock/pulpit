//! Signing (SPEC-signing.md §31, §79.4): the Sign dialog's state machine,
//! the identity and credential cache it draws on, the append-only gate,
//! and the signature panel — everything grouped into [`SignState`] and
//! held as one field (`App::sign`) rather than a dozen flat ones.

use std::path::PathBuf;

use iced::Task;

use super::{pick_pdf_named, App, Message, SignTargetCandidates};

/// Everything the Sign flow (SPEC-signing.md §31) needs between ticks.
///
/// Grouped into one field on `App` (§79.4) rather than left as a dozen flat
/// fields: the dialog's own state machine (`flow`), the identity and
/// credential cache it draws on, the scratch files a save-then-sign step
/// writes, and the append-only gate and signature panel that read the
/// document independently of whether a flow is open at all.
#[derive(Default)]
pub struct SignState {
    /// The Sign dialog's state machine, or `None` when it is closed. Its own
    /// module so the state machine can be unit-tested without a display
    /// (§30.2: signing always runs supervisor-side, never in the render
    /// worker, which is why this lives on `App` rather than being routed
    /// through `reader_link`).
    pub(crate) flow: Option<crate::signing::SigningFlow>,
    /// Saved identity selected for the current signing flow. `None` means
    /// the user chose an ad-hoc credential file.
    pub(crate) profile: Option<String>,
    /// Saved copy to sign when the flow first had to write unsaved edits.
    /// Otherwise the active document remains the source.
    pub(crate) source: Option<PathBuf>,
    /// The signature field a page-surface click asked to sign into
    /// (`SignMsg::StartInField`, SPEC-signing.md §31.1), consulted once by
    /// `enter_signing_review` to preselect it among `sign_target_candidates`
    /// — or, when preflight does not offer it, to say so in the Review step
    /// rather than quietly sign into a different field.
    /// Cleared when the flow starts, cancels or finishes — not when the
    /// credential changes mid-flow, since switching credentials does not
    /// change which field was clicked.
    pub(crate) prefill_field: Option<String>,
    /// Credentials unlocked this session, by profile id, so a reader signing
    /// several documents — or several fields — types a passphrase once
    /// instead of once per signature.
    ///
    /// The tradeoff is deliberate and worth naming: key material now lives in
    /// memory for as long as the application runs, rather than only for the
    /// span of one Sign flow. That is the point of the cache, not a lapse.
    /// §30.2 still holds around it — the key never leaves this process, never
    /// crosses `reader_link` to a worker, is never written anywhere, and each
    /// [`pulpit_render::sign::Credential`] zeroizes its key material when the
    /// last `Arc` to it drops, which at the latest is process exit. Entries
    /// are keyed by profile id and evicted when that profile is edited or
    /// removed; nothing else clears the map, because "until the user quits"
    /// is exactly the lifetime asked for.
    ///
    /// Ad-hoc credentials chosen through the file picker are never cached:
    /// they have no stable identity to key on, and picking one is an explicit
    /// one-off.
    pub(crate) unlocked_credentials:
        std::collections::HashMap<String, std::sync::Arc<pulpit_render::sign::Credential>>,
    /// Whether the document open right now is being kept append-only
    /// (SPEC-signing.md §31.3), because it was found to already carry a
    /// signature. `None` when the question has not been answered yet, or
    /// when nothing is open.
    pub(crate) append_only: Option<crate::signing::AppendOnlyMode>,
    /// A document was just opened and its bytes contain a signature; the
    /// append-only offer is waiting for an answer before any mutation is
    /// permitted (§31.3, A9).
    pub(crate) pending_append_only_offer: bool,
    /// What structural discovery found in the open document at open time
    /// (§31.3, §31.4): the signature panel's data, and the append-only
    /// offer's trigger. Re-verified (not just re-discovered) each time the
    /// panel is opened would be more current, but discovery already reads
    /// the whole file once per open and the panel is about the document as
    /// it was opened, not a live view of a file nothing here is editing.
    pub(crate) document_signatures: Vec<pulpit_render::verify::SignatureVerification>,
    /// Whether the signature panel (§31.4) is open.
    pub(crate) signature_panel_open: bool,
    /// Which entry in `document_signatures`, if any, has its detail view
    /// expanded.
    pub(crate) signature_panel_expanded: Option<usize>,
    /// When §31.1 step 3's write began, for the panel that covers it.
    ///
    /// The write is usually over in a few milliseconds and occasionally takes
    /// a second — a full rewrite of a long document — so a panel shown the
    /// instant it starts is a modal nobody can read, and one never shown lets
    /// a stroke drawn mid-write miss the signature made from those bytes.
    /// Timed instead: the surface is blocked from the first millisecond, and
    /// the sheet explaining why appears only once the wait is long enough to
    /// need explaining.
    pub(crate) saving_since: Option<std::time::Instant>,
    /// The signed copy the Sign flow has asked to open, so
    /// `finish_document_prepare` knows this document's signature is one the
    /// reader just made rather than one they have walked into.
    pub(crate) opening_signed_copy: Option<PathBuf>,
    /// The scratch file §31.1 step 3's save writes to, while signing an
    /// edited document.
    ///
    /// Signing an annotated document used to ask *twice* — once for where to
    /// put the annotated copy, once for where to put the signed one — and
    /// left two files behind where the reader wanted one. The intermediate
    /// copy is not a thing anyone asked for: it exists because the signature
    /// has to be computed over bytes on disk, and the edits are in memory.
    /// So it is written here, unasked, and deleted as soon as the signature
    /// has been made from it. The only file the reader names, and the only
    /// one left afterwards, is the signed one.
    ///
    /// Beside the source rather than in the system temporary directory, and
    /// hidden: it is a copy of the reader's document, and it should be no
    /// more exposed than the document already is.
    pub(crate) temp: Option<PathBuf>,
}

impl App {
    /// Where §31.1 step 3's save should write, when the save in flight is
    /// signing's own. `None` for an ordinary Save As, which asks.
    pub(super) fn signing_scratch_destination(&mut self) -> Option<PathBuf> {
        if !matches!(
            self.sign.flow,
            Some(crate::signing::SigningFlow::SavingFirst)
        ) {
            return None;
        }
        if let Some(existing) = self.sign.temp.clone() {
            return Some(existing);
        }
        let source = self
            .documents
            .active()
            .map(|document| document.path.clone())?;
        let directory = source
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        // One per process: two Sign flows cannot be in flight at once, and a
        // file left behind by a crash is overwritten by the next run rather
        // than accumulating.
        let scratch = directory.join(format!(".pulpit-signing-{}.pdf", std::process::id()));
        self.sign.temp = Some(scratch.clone());
        Some(scratch)
    }

    /// Delete the scratch copy, if one was written. Called wherever the Sign
    /// flow ends — completed, refused, or cancelled — so the file never
    /// outlives the signature it was made for.
    pub(super) fn discard_signing_scratch(&mut self) {
        let Some(scratch) = self.sign.temp.take() else {
            return;
        };
        // Best effort: a scratch file that cannot be removed is worth a line
        // in the diagnostics bundle and nothing more. It is not an error the
        // reader can act on, and the signature it was made for either exists
        // or was already reported as failed.
        if let Err(error) = std::fs::remove_file(&scratch) {
            if error.kind() != std::io::ErrorKind::NotFound {
                self.diagnostics.note(format!(
                    "could not remove the signing scratch file {}: {error}",
                    scratch.display()
                ));
            }
        }
    }

    /// Whether this profile's credential is already unlocked for the session,
    /// so the profile picker can say which ones will not ask for a passphrase.
    pub(crate) fn is_profile_unlocked(&self, id: &str) -> bool {
        self.sign.unlocked_credentials.contains_key(id)
    }

    // --- Signing (SPEC-signing.md §31) --------------------------------

    /// The Sign flow's state machine, driven from `Message::Sign`.
    ///
    /// Signing always runs here, in the supervisor process, and never in the
    /// document worker or a render worker (§30.2): the private key exists
    /// only for the span of this flow, in a `Credential` whose key material
    /// is zeroized on drop, and it is never sent across the `reader_link`
    /// pipe.
    pub(super) fn handle_sign(&mut self, msg: crate::signing::SignMsg) -> Task<Message> {
        use crate::signing::{SignMsg, SigningFlow};
        use zeroize::Zeroize;

        match msg {
            // Both entry points start from nothing held over — including a
            // scratch copy a previous attempt left behind, which would
            // otherwise be signed in place of this document's edits.
            SignMsg::Start => {
                self.end_sign_flow();
                self.start_sign_flow()
            }
            SignMsg::StartInField(name) => {
                self.end_sign_flow();
                self.sign.prefill_field = Some(name);
                self.start_sign_flow()
            }
            SignMsg::ResumeAfterSave => {
                // §31.1 step 3's save has landed. The clicked field is still
                // in `signing_prefill_field` on purpose: it is the whole
                // reason this flow knows where to sign.
                if !matches!(self.sign.flow, Some(SigningFlow::SavingFirst)) {
                    return Task::none();
                }
                self.begin_signing()
            }
            SignMsg::Cancel => {
                self.end_sign_flow();
                Task::none()
            }
            SignMsg::ProfileChosen(id) => {
                // Selecting is not submitting: a passphrase typed for the
                // profile that was showing is dropped rather than carried to
                // a credential it was never typed for.
                let Some(SigningFlow::Unlock {
                    profile_id,
                    passphrase,
                    error,
                    busy,
                }) = self.sign.flow.as_mut()
                else {
                    return Task::none();
                };
                if *busy || *profile_id == id {
                    return Task::none();
                }
                passphrase.zeroize();
                passphrase.clear();
                *error = None;
                profile_id.clone_from(&id);
                self.sign.profile = Some(id);
                Task::none()
            }
            SignMsg::PassphraseChanged(typed) => {
                if let Some(SigningFlow::Unlock {
                    passphrase, busy, ..
                }) = self.sign.flow.as_mut()
                {
                    if !*busy {
                        passphrase.zeroize();
                        *passphrase = typed;
                    }
                }
                Task::none()
            }
            SignMsg::PassphraseSubmit => self.sign_submit_unlock(),
            SignMsg::CredentialLoaded(Ok(credential)) => {
                let Some(SigningFlow::Unlock {
                    profile_id,
                    passphrase,
                    ..
                }) = self.sign.flow.as_mut()
                else {
                    return Task::none();
                };
                // The step is about to be replaced, and dropping the `String`
                // would leave the passphrase in the freed allocation (§30.2).
                passphrase.zeroize();
                let profile_id = profile_id.clone();
                // Remember it for the rest of the session. Every credential
                // the flow loads now comes from a saved profile, so there is
                // always a stable id to key on — see `unlocked_credentials`
                // for the tradeoff.
                self.sign
                    .unlocked_credentials
                    .insert(profile_id, credential.clone());
                self.sign_proceed_with_credential(credential)
            }
            SignMsg::CredentialLoaded(Err(detail)) => {
                // Back to the same step with the same profile selected: a
                // mistyped passphrase is not a reason to start over.
                if let Some(SigningFlow::Unlock {
                    passphrase,
                    error,
                    busy,
                    ..
                }) = self.sign.flow.as_mut()
                {
                    passphrase.zeroize();
                    passphrase.clear();
                    *error = Some(detail);
                    *busy = false;
                }
                Task::none()
            }
            SignMsg::OverrideValidity => {
                // §33's last paragraph, answered: the certificate is still
                // expired, and signing with it is now on the record as a
                // choice.
                let Some(SigningFlow::ConfirmValidity { credential, .. }) = self.sign.flow.take()
                else {
                    return Task::none();
                };
                self.sign_open_destination_picker(credential)
            }
            SignMsg::DestinationChosen(Some(chosen)) => {
                // The picker ran its own overwrite confirmation, so whatever
                // came back is allowed to already exist.
                self.sign_execute(chosen)
            }
            SignMsg::DestinationChosen(None) => {
                // The one dialog signing shows was dismissed. Nothing has
                // been written, so there is nothing to say about it.
                self.end_sign_flow();
                Task::none()
            }
            SignMsg::Completed(Ok(outcome)) => {
                let countersigning = matches!(
                    self.sign.flow.as_ref(),
                    Some(SigningFlow::Signing { options, .. }) if options.countersigning
                );
                self.end_sign_flow();
                let notice = crate::signing::signed_notice(
                    &outcome.destination,
                    &outcome.report,
                    &outcome.verification,
                    countersigning,
                );
                tracing::info!(message = %notice.message);
                self.diagnostics
                    .note(format!("{} {}", notice.message, notice.detail));
                let intent = if notice.verified {
                    crate::toast::Intent::Info
                } else {
                    // A signature that does not verify seconds after being
                    // made is a failure, whatever the write returned, and it
                    // must not fade on a four-second timer. The copy is still
                    // opened: reading it is how the reader finds out what is
                    // wrong with it.
                    crate::toast::Intent::Error
                };
                self.toasts
                    .push(intent, notice.message, Some(notice.detail), self.now);
                // The signature is in a *new* file beside the source, and
                // leaving the unsigned original on screen reads as a
                // signature that never appeared. So the copy is opened, and
                // `finish_document_prepare` takes the append-only answer for
                // it rather than asking the reader to confirm what they have
                // just done.
                self.sign.opening_signed_copy = Some(outcome.destination.clone());
                self.open_document(outcome.destination)
            }
            SignMsg::Completed(Err(detail)) => {
                self.refuse_signing(detail);
                Task::none()
            }
        }
    }

    /// Forget everything the Sign flow was holding, including the loaded
    /// credential in whichever step held it. The session's cache of unlocked
    /// credentials is deliberately untouched: it outlives one signature.
    pub(super) fn end_sign_flow(&mut self) {
        self.sign.flow = None;
        self.sign.profile = None;
        self.sign.source = None;
        self.sign.prefill_field = None;
        self.sign.saving_since = None;
        // Whether the signature was made, refused or cancelled, the scratch
        // copy has no reason to outlive the flow that wrote it.
        self.discard_signing_scratch();
    }

    /// Refuse to sign, saying why (§33: what failed, and that the source is
    /// unchanged). Sticky, because the reader asked for a signature and did
    /// not get one — this is the only report they will see.
    fn refuse_signing(&mut self, detail: String) {
        self.end_sign_flow();
        self.notify_error(detail, Some("The document is unchanged.".to_string()));
    }

    /// The Unlock step's Continue: unlock the selected profile, or go
    /// straight on when this session already holds its credential (which is
    /// the case whenever the step was shown only to choose between
    /// profiles).
    fn sign_submit_unlock(&mut self) -> Task<Message> {
        use crate::signing::{SignMsg, SigningFlow};

        let Some(SigningFlow::Unlock {
            profile_id,
            passphrase,
            busy,
            ..
        }) = self.sign.flow.as_ref()
        else {
            return Task::none();
        };
        if *busy {
            return Task::none();
        }
        let id = profile_id.clone();
        let passphrase = passphrase.clone();
        if let Some(credential) = self.sign.unlocked_credentials.get(&id).cloned() {
            return self.sign_proceed_with_credential(credential);
        }
        let credential_path = match self.sign_credential_path(&id) {
            Ok(path) => path,
            Err(detail) => {
                if let Some(SigningFlow::Unlock { error, .. }) = self.sign.flow.as_mut() {
                    *error = Some(detail);
                }
                return Task::none();
            }
        };
        if let Some(SigningFlow::Unlock { busy, error, .. }) = self.sign.flow.as_mut() {
            *busy = true;
            *error = None;
        }
        Task::perform(
            async move {
                let bytes = std::fs::read(&credential_path).map_err(|e| {
                    format!(
                        "could not read {}: {e}; the credential file is unchanged",
                        credential_path.display()
                    )
                })?;
                pulpit_render::sign::load_pkcs12(
                    &bytes,
                    pulpit_render::sign::Zeroizing::new(passphrase),
                )
                .map(std::sync::Arc::new)
                .map_err(|e| format!("{e}; the credential was not loaded"))
            },
            |result| Message::Sign(SignMsg::CredentialLoaded(result)),
        )
    }

    /// Where profile `id`'s credential lives, or why it cannot be used.
    fn sign_credential_path(&self, id: &str) -> Result<PathBuf, String> {
        let Some(profile) = self.settings.signatures.profile(id) else {
            return Err("That signature profile no longer exists.".to_string());
        };
        let Some(path) = self.signature_profile_credential_path(profile) else {
            return Err("The profile's credential location is invalid.".to_string());
        };
        if !path.is_file() {
            return Err(format!(
                "The credential for profile “{}” is missing at {}.",
                profile.name,
                path.display()
            ));
        }
        Ok(path)
    }

    /// With a credential in hand: describe it, stop on §33's validity gate,
    /// and otherwise go to the one dialog left.
    fn sign_proceed_with_credential(
        &mut self,
        credential: std::sync::Arc<pulpit_render::sign::Credential>,
    ) -> Task<Message> {
        use crate::signing::SigningFlow;

        // Rebuilt against the current time rather than reused, so an expiry
        // that has passed since the credential was unlocked is still caught.
        let summary = match credential.summary() {
            Ok(summary) => summary,
            Err(e) => {
                self.refuse_signing(format!("{e}; the credential was not used."));
                return Task::none();
            }
        };
        let info = crate::signing::CredentialInfo::from_summary(summary, self.unix_now());
        if !info.may_proceed() {
            self.sign.flow = Some(SigningFlow::ConfirmValidity {
                credential,
                info: Box::new(info),
            });
            return Task::none();
        }
        self.sign_open_destination_picker(credential)
    }

    /// Decide what will be signed, once what preflight reports on the source
    /// is known: the destination picker, or a refusal.
    ///
    /// The target and the appearance are settled *before* the picker opens,
    /// so a document with nothing to sign into is refused while there is
    /// still nothing to take back — never after a file name has been chosen.
    /// Split from `sign_open_destination_picker` by §82.8: this is the half
    /// that needs `&mut self`, run once the read and the preflight parses —
    /// which do not — have already happened off the event loop.
    pub(super) fn sign_open_destination_picker_with(
        &mut self,
        credential: std::sync::Arc<pulpit_render::sign::Credential>,
        candidates: SignTargetCandidates,
    ) -> Task<Message> {
        use crate::signing::{SignMsg, SigningFlow};

        let Some(source) = self.signing_source_path() else {
            self.refuse_signing("There is no document open to sign.".to_string());
            return Task::none();
        };
        let options = match self.sign_prepare_options(candidates) {
            Ok(options) => options,
            Err(detail) => {
                self.refuse_signing(detail);
                return Task::none();
            }
        };
        // The name the picker opens on never points at a file that is already
        // there, so confirming the default cannot destroy anything; the
        // picker's own confirmation is what makes an overwrite a choice.
        let suggested =
            crate::signing::signed_destination(&source, &|path: &std::path::Path| path.exists());
        self.sign.flow = Some(SigningFlow::Signing {
            credential,
            options,
        });
        Task::perform(pick_pdf_named(suggested), |path| {
            Message::Sign(SignMsg::DestinationChosen(path))
        })
    }

    /// Decide what will be signed and how it will look, then ask the one
    /// question that is genuinely the reader's: where to put the copy.
    ///
    /// §82.8: reading the whole source and running two preflight parses used
    /// to happen synchronously, inside the message handler that reaches
    /// this. That work is `Self::sign_target_candidates`, run here in a
    /// `Task::perform` step; the answer comes back as
    /// `SignMsg::CandidatesReady`, which finishes the job in
    /// `sign_open_destination_picker_with`.
    fn sign_open_destination_picker(
        &mut self,
        credential: std::sync::Arc<pulpit_render::sign::Credential>,
    ) -> Task<Message> {
        let Some((path, signed_at_open)) = self.signing_source_state() else {
            self.refuse_signing("There is no document open to sign.".to_string());
            return Task::none();
        };
        Task::perform(
            Self::sign_target_candidates(path, signed_at_open),
            move |candidates| Message::SignCandidatesReady(credential, candidates),
        )
    }

    /// What this signature will land on, and what mark it will leave —
    /// derived from the click, from preflight, and from the profile, with
    /// nothing left to ask.
    ///
    /// The click wins outright or fails outright: a field the reader pointed
    /// at that preflight cannot offer is refused by name, never replaced with
    /// a preset corner of some other page.
    fn sign_prepare_options(
        &self,
        candidates: SignTargetCandidates,
    ) -> Result<crate::signing::SigningOptions, String> {
        use crate::signing::{
            pick_signing_target, prefill_missed_line, SigningOptions, TargetPick,
        };

        let SignTargetCandidates {
            candidates,
            countersigning,
        } = candidates;
        let target = match pick_signing_target(&candidates, self.sign.prefill_field.as_deref()) {
            TargetPick::Selected(choice) => choice,
            TargetPick::Missed { clicked } => return Err(prefill_missed_line(&clicked)),
            TargetPick::Nothing => {
                // Only reachable for a document that already carries a
                // signature and has no empty field left to countersign into
                // (every other preflight outcome keeps a new field on offer).
                return Err(
                    "This document has no signature field to sign into: it already \
                            carries a signature, and has no empty signature field left to \
                            countersign."
                        .to_string(),
                );
            }
        };
        let mut options = SigningOptions {
            target: Some(target),
            countersigning,
            ..Default::default()
        };
        let context = self.placement_context(options.target.as_ref());
        let profile_appearance = self
            .sign
            .profile
            .as_deref()
            .and_then(|id| self.settings.signatures.profile(id))
            .map(|profile| profile.appearance.clone());
        crate::signing::apply_target_defaults(&mut options, profile_appearance.as_ref(), &context);
        Ok(options)
    }

    /// The body shared by `SignMsg::Start` and `SignMsg::StartInField`: §31.1
    /// step 3's save-first check, then landing on the flow's first step.
    /// Callers are responsible for setting up `signing_prefill_field` (or
    /// clearing it) before calling this — it is not touched here, since it
    /// must survive the save-first resume at `Told::Saved`, which reaches
    /// `begin_signing` by way of `SignMsg::ResumeAfterSave`.
    fn start_sign_flow(&mut self) -> Task<Message> {
        use crate::signing::SigningFlow;

        // §31.1 step 3: edits are written out first, and the signature is
        // made over those bytes. Not to a file the reader is asked to name —
        // that produced two pickers and two files for one request — but to
        // the scratch copy `pick_where_to_save_document` diverts this save
        // to, which is deleted the moment the signature exists (see
        // `signing_temp`). Whether required fields are still empty is a
        // question for the save path itself — `ask_where_to_save_document`
        // runs the "Save anyway?" review (`pending_save_review`) over that —
        // and must not decide here whether the write happens at all:
        // skipping it on an unfilled required field would sign the stale
        // on-disk copy and silently lose the edits.
        if self.has_unsaved_edits() {
            self.sign.flow = Some(SigningFlow::SavingFirst);
            self.sign.saving_since = Some(self.now);
            return self.ask_where_to_save_document();
        }
        self.begin_signing()
    }

    /// §31.1 step 4, once there is nothing left to save: land on the one
    /// step that has a question in it, or on none at all.
    ///
    /// Which profile is not asked when there is only one, and the passphrase
    /// is not asked when this session already holds the credential. With
    /// neither to ask, signing shows nothing but the save picker. With no
    /// profile saved at all it refuses and names the place that accepts one
    /// (`crate::signing::NO_PROFILE_REFUSAL`): importing a `.p12` belongs in
    /// Settings, where it is named and given an appearance once, rather than
    /// in a file picker that would forget it again.
    fn begin_signing(&mut self) -> Task<Message> {
        use crate::signing::SigningFlow;

        if self.settings.signatures.profiles.is_empty() {
            self.refuse_signing(crate::signing::NO_PROFILE_REFUSAL.to_string());
            return Task::none();
        }
        let chosen = self
            .settings
            .signatures
            .default_profile
            .clone()
            .filter(|id| self.settings.signatures.profile(id).is_some())
            .or_else(|| {
                self.settings
                    .signatures
                    .profiles
                    .first()
                    .map(|profile| profile.id.clone())
            });
        let Some(chosen) = chosen else {
            self.refuse_signing(crate::signing::NO_PROFILE_REFUSAL.to_string());
            return Task::none();
        };
        self.sign.profile = Some(chosen.clone());
        // One profile, already unlocked: every answer is known, so nothing is
        // asked. This is the common case and it goes straight to the picker.
        if self.settings.signatures.profiles.len() == 1 {
            if let Some(credential) = self.sign.unlocked_credentials.get(&chosen).cloned() {
                return self.sign_proceed_with_credential(credential);
            }
        }
        self.sign.flow = Some(SigningFlow::Unlock {
            profile_id: chosen,
            passphrase: String::new(),
            error: None,
            busy: false,
        });
        Task::none()
    }

    /// §31.3's forbidden list, in terms of the commands the toolbar and the
    /// page surface actually send: arming a drawing tool, placing or editing
    /// a mark, filling any form control, and Save As (which routes through
    /// PDFium's full rewriting save and would flatten signature history).
    /// `Sign` and `SignField` are deliberately not here — countersigning is
    /// the one addition append-only mode exists to permit (whether started
    /// from the toolbar or by clicking the field itself), and
    /// `sign_execute`'s own preflight pass enforces the rest of §25.4 no
    /// matter what this check misses.
    ///
    /// `PagePressed` and `PageReleased` are not here either, and used to be.
    /// They are the *gesture*, not what it does: with no tool armed a press
    /// is the hand, which pans the page, follows a link, drags out a crop
    /// marquee or selects text — none of which touch the document. Refusing
    /// them took the hand away from every signed document, which is most of
    /// what reading one consists of. What a gesture may go on to commit is
    /// refused where it commits instead — `commit_to_document` and
    /// `ask_form_event_on` — which is both narrower and wider than this list
    /// could be: narrower because reading is untouched, wider because it
    /// also covers the paths that never came through here at all, such as
    /// typing into a field reached with Tab.
    pub(super) fn read_command_mutates(command: &crate::widgets::event::ReadCommand) -> bool {
        use crate::widgets::event::ReadCommand;
        matches!(
            command,
            ReadCommand::Arm(Some(_))
                | ReadCommand::CommitMark
                | ReadCommand::ComposeMark(_)
                | ReadCommand::DeleteSelected
                | ReadCommand::DeleteAnnotation(_)
                | ReadCommand::AddBookmark
                | ReadCommand::CommitBookmarkTitle
                | ReadCommand::DeleteBookmark(_)
                | ReadCommand::EditSelected
                | ReadCommand::PickDate(_)
                | ReadCommand::PickTime
                | ReadCommand::PickOption(_)
                | ReadCommand::ToggleOption(_)
                | ReadCommand::SaveAs
        )
    }

    /// Whether the open document has edits that Save As has not yet written
    /// — the condition §31.1 step 3 checks before signing.
    fn has_unsaved_edits(&self) -> bool {
        self.reader.can_undo()
    }

    fn signing_source_path(&self) -> Option<PathBuf> {
        self.sign.source.clone().or_else(|| {
            self.documents
                .active()
                .map(|document| document.path.clone())
        })
    }

    /// Clear a Sign flow parked at `SavingFirst` when the save it is
    /// waiting on is declined instead of completed.
    ///
    /// `SigningFlow::SavingFirst` is only ever left by the save either
    /// finishing (resumed at `Told::Saved`) or being declined or refused.
    /// Every place that can happen — the file picker returning nothing, the
    /// "Save anyway?" review being cancelled or Escaped, the user being sent
    /// back to fill a required field instead, or the save itself being
    /// refused (no active document, no reader, or the chosen destination is
    /// the open document) — must call this, or the modal is a soft-lock
    /// nothing in the UI can dismiss. Its own Cancel button (view.rs's
    /// `sign_dialog`) is defense in depth for whichever of those a future
    /// change forgets.
    pub(super) fn cancel_signing_if_saving_first(&mut self) {
        if matches!(
            self.sign.flow,
            Some(crate::signing::SigningFlow::SavingFirst)
        ) {
            self.end_sign_flow();
        }
    }

    /// §31.1 step 5's target field candidates, computed from a cheap
    /// preflight pass over the document as it stands on disk.
    ///
    /// Bytes are read and preflighted unconditionally — even for a document
    /// `append_only` has never touched — because an *unsigned* document can
    /// carry an empty `/Sig` field a sender left for the recipient to fill,
    /// and that field should be offered (ahead of `NewField`) rather than
    /// silently ignored. `append_only` itself is consulted only as the
    /// fallback for when the source cannot be read at all: `AppendOnly` or
    /// `EditAnyway` means a signature was found on open, so the safe
    /// fallback is "no candidates" rather than a `NewField` the engine would
    /// refuse; `None` means the document was never seen carrying one, so
    /// `NewField` is always a safe fallback.
    ///
    /// Whether the document on disk right now actually carries a signature
    /// is decided from what preflight reports about the bytes at
    /// `signing_source_path()`, not from `append_only` directly:
    /// `EditAnyway` overrides the *mutation* refusal, but a document it was
    /// set on can still carry a signature (and the engine refuses
    /// `NewInvisibleField` outright once it does), or can have lost every
    /// signature to a save-first rewrite in between.
    ///
    /// `preflight_sign` returns exactly one target when there is an
    /// unambiguous empty field, which is the overwhelmingly common shape (a
    /// sender leaves one signature field for the recipient); a document with
    /// several empty fields reports `AmbiguousSignatureField`, whose
    /// `candidates` all come back here for the Options step's target picker
    /// to offer, rather than silently picking the first one.
    /// Everything `sign_target_candidates` needs from `self` before the read
    /// and the preflight parses, which §82.8 moves off the event loop: a
    /// full-length `std::fs::read` plus two preflight passes over the file
    /// were running synchronously inside a message handler.
    fn signing_source_state(&self) -> Option<(PathBuf, bool)> {
        use crate::signing::AppendOnlyMode;
        let path = self.signing_source_path()?;
        let signed_at_open = matches!(
            self.sign.append_only,
            Some(AppendOnlyMode::AppendOnly) | Some(AppendOnlyMode::EditAnyway)
        );
        Some((path, signed_at_open))
    }

    /// Read the source and run preflight, off the event loop (§82.8): a
    /// `Task::perform` step feeds the answer back as
    /// `SignMsg::CandidatesReady`. Free of `self` so it can run inside the
    /// `async move` block Iced's executor drives independently.
    async fn sign_target_candidates(path: PathBuf, signed_at_open: bool) -> SignTargetCandidates {
        use crate::signing::TargetChoice;
        use pulpit_render::verify::preflight::{preflight_certify, preflight_sign};

        // The source could not be read to preflight: there is no way to tell
        // the user why from here, so this degrades silently to the best
        // guess `append_only` supports. The engine restates the real failure
        // (unreadable file, encrypted document, …) at Sign time, when there
        // is a place to show it.
        let unreadable_fallback = if signed_at_open {
            SignTargetCandidates {
                candidates: Vec::new(),
                countersigning: true,
            }
        } else {
            SignTargetCandidates {
                candidates: vec![TargetChoice::NewField],
                countersigning: false,
            }
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return unreadable_fallback;
        };
        // A save-first step ahead of this one can go through PDFium's full
        // rewrite, which drops every existing signature (§31.1 step 3) — so
        // the saved copy may in fact be unsigned even when `append_only` was
        // set. Ask the bytes, not the enum: `preflight_certify` is cheap (it
        // is the engine's own "is this document unsigned" check) and its
        // refusal already counts the signatures it found.
        Self::sign_target_candidates_from_signals(
            preflight_certify(&bytes),
            preflight_sign(&bytes, None),
        )
    }

    /// The pure half of [`Self::sign_target_candidates`]: what to offer
    /// given what preflight reports on the bytes, independent of how those
    /// bytes were read. `certify` is `preflight_certify`'s result and
    /// `sign_preflight` is `preflight_sign(bytes, None)`'s result, the
    /// auto-detected empty field or the reason none was found.
    ///
    /// `NewField` is suppressed only when the document is known to already
    /// carry a signature — `certify` refusing specifically with
    /// `CertificationNotAllowed`, the one refusal that counts existing
    /// `/Sig` values — because the engine refuses `NewInvisibleField`
    /// outright once any `/Sig` has a `/V`. Every other `certify` refusal
    /// (an encrypted or unparseable document, say) is not evidence of an
    /// existing signature, so it degrades to the unsigned case: `NewField`
    /// stays offered, and the engine's own preflight names the true reason
    /// if signing is in fact impossible. Any empty field the document
    /// already carries is always offered first — so it is the default
    /// `pick_signing_target` falls back to without a prefill — with
    /// `NewField` appended last as the fallback that is available whenever
    /// the document is not already signed.
    fn sign_target_candidates_from_signals(
        certify: std::result::Result<(), pulpit_render::verify::preflight::PreflightRefusal>,
        sign_preflight: std::result::Result<
            pulpit_render::verify::preflight::PreflightOk,
            pulpit_render::verify::preflight::PreflightRefusal,
        >,
    ) -> SignTargetCandidates {
        use crate::signing::TargetChoice;
        use pulpit_render::verify::preflight::PreflightRefusal;

        let countersigning = matches!(
            certify,
            Err(PreflightRefusal::CertificationNotAllowed { .. })
        );
        let mut candidates: Vec<TargetChoice> = match sign_preflight {
            Ok(ok) => vec![TargetChoice::ExistingField(ok.target_field)],
            Err(PreflightRefusal::AmbiguousSignatureField { candidates }) => candidates
                .into_iter()
                .map(TargetChoice::ExistingField)
                .collect(),
            Err(_) => Vec::new(),
        };
        if !countersigning {
            candidates.push(TargetChoice::NewField);
        }
        SignTargetCandidates {
            candidates,
            countersigning,
        }
    }

    /// Where a visible mark could go for `target`, as the Sign flow's state
    /// machine wants it (`crate::signing::PlacementContext`).
    ///
    /// An existing field the reader can point at was drawn by the sender at a
    /// definite place on a definite page, so that box is the answer and no
    /// preset applies. Locating it means finding the field's first widget in
    /// the reader session's form-field list and checking that its box has an
    /// area: the engine refuses `FieldRect` for a missing or zero-area
    /// `/Rect` (it would produce an appearance stream no viewer can show), so
    /// a field that would be refused falls back here rather than at Sign
    /// time. Everything else — a new field, an unplaced field, a document
    /// whose fields were never described — gets presets against the page the
    /// reader is currently showing.
    fn placement_context(
        &self,
        target: Option<&crate::signing::TargetChoice>,
    ) -> crate::signing::PlacementContext {
        use crate::signing::{PlacementContext, TargetChoice};

        if let Some(TargetChoice::ExistingField(name)) = target {
            if let Some((page, bounds)) = self.reader.field_widget_box(name) {
                if bounds.right > bounds.left && bounds.bottom > bounds.top {
                    return PlacementContext::FieldBox {
                        field: name.clone(),
                        page_index: page.0,
                    };
                }
            }
        }
        PlacementContext::Presets {
            page_index: self.reader.current_page().map_or(0, |page| page.0),
        }
    }

    /// §31.1 steps 8–9: sign, then reopen and verify what was produced.
    fn sign_execute(&mut self, destination: PathBuf) -> Task<Message> {
        use crate::signing::{SignMsg, SignOutcome, SigningFlow};
        use pulpit_render::sign::{SignRequest, SigningProfile};

        let Some(SigningFlow::Signing {
            credential,
            options,
            ..
        }) = self.sign.flow.take()
        else {
            return Task::none();
        };
        let profile_appearance = self
            .sign
            .profile
            .as_deref()
            .and_then(|id| self.settings.signatures.profile(id))
            .map(|profile| profile.appearance.clone());
        // §25.5: build a visible appearance when requested, against the page
        // the plan named when the choice was made — not the page showing now,
        // which the reader may have scrolled away from since.
        let appearance = options.appearance_page_index().and_then(|page_index| {
            let geometry = self
                .reader
                .page_geometry(pulpit_core::page::PageIndex(page_index))?;
            let signer_cn =
                crate::signing::subject_common_name(&credential.summary().ok()?.subject)
                    .to_string();
            let signing_time_label = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default();
            crate::signing::appearance_for_profile(
                &options,
                profile_appearance.as_ref(),
                &signer_cn,
                &signing_time_label,
                // The whole geometry, not just its displayed size: the crop
                // origin and the rotation are what turn the box the reader
                // chose into the /Rect a viewer honours.
                geometry,
            )
        });
        let Some(source) = self.signing_source_path() else {
            self.refuse_signing("There is no document open to sign.".to_string());
            return Task::none();
        };
        // Both of these were settled before the picker opened
        // (`sign_prepare_options`), so reaching either arm is a bug rather
        // than a situation — but signing must not proceed on a guess.
        let Some(target) = options
            .target
            .as_ref()
            .map(pulpit_render::sign::SignTarget::from)
        else {
            self.refuse_signing("No signature field was selected.".to_string());
            return Task::none();
        };
        self.sign.flow = Some(SigningFlow::Signing {
            credential: credential.clone(),
            options: options.clone(),
        });

        let signing_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut id2 = [0u8; 16];
        // §23: a random second element for the trailer's /ID. This crate
        // never draws randomness inside `pulpit-render` (§22.3); the
        // supervisor is where the RNG lives.
        if getrandom::getrandom(&mut id2).is_err() {
            // Fall back to a value derived from the signing time rather than
            // failing the whole flow over an RNG that could not be reached:
            // the ID only has to be unlikely to collide, not secret.
            id2[..8].copy_from_slice(&signing_time.to_le_bytes());
        }
        let request = SignRequest {
            profile: SigningProfile::AdbePkcs7Detached,
            signing_time,
            field: target,
            // §31.1 step 5's free-text metadata is not collected: three
            // empty boxes were most of what made the dialog that asked for
            // them read as work, and nothing else fills them.
            reason: None,
            location: None,
            contact: None,
            id2,
            tight_size_estimates: false,
            appearance,
        };
        let destination_for_task = destination.clone();

        Task::perform(
            async move {
                pulpit_render::sign::sign_document_file(
                    &source,
                    &destination_for_task,
                    &credential,
                    &request,
                )
                .map_err(|e| format!("{e}"))
                .and_then(|report| {
                    let bytes = std::fs::read(&destination_for_task).map_err(|e| {
                        format!(
                            "signed {} but could not reopen it to verify: {e}",
                            destination_for_task.display()
                        )
                    })?;
                    let verification = pulpit_render::verify::verify_signatures(&bytes)
                        .map_err(|e| format!("signed the document but verification failed: {e}"))?;
                    Ok(SignOutcome {
                        report: std::sync::Arc::new(report),
                        destination: destination_for_task.clone(),
                        verification: std::sync::Arc::new(verification),
                    })
                })
            },
            |result| Message::Sign(SignMsg::Completed(result)),
        )
    }
}

#[cfg(test)]
mod append_only_tests {
    use super::App;
    use crate::widgets::event::ReadCommand;

    #[test]
    fn a_signed_document_still_takes_the_gestures_that_only_read_it() {
        // With no tool armed a press is the hand: it pans the page, follows a
        // link, drags out a crop marquee, selects text. Refusing the gesture
        // took all of that away from every signed document, which is most of
        // what reading one consists of — and the hand is the tool a reader
        // spends the whole time in.
        for command in [ReadCommand::PagePressed, ReadCommand::PageReleased] {
            assert!(
                !App::read_command_mutates(&command),
                "{command:?} is a gesture, not a change to the document"
            );
        }
    }

    #[test]
    fn what_a_gesture_goes_on_to_commit_is_still_refused() {
        // The list keeps everything that *is* a change, and the two choke
        // points the gestures reach — `commit_to_document` and
        // `ask_form_event_on` — refuse the rest.
        for command in [
            ReadCommand::Arm(Some(pulpit_core::annotation::AnnotationTool::Ink)),
            ReadCommand::CommitMark,
            ReadCommand::DeleteSelected,
            // Deleting from the annotations panel is the same removal reached
            // from a list rather than from the page, and is refused the same.
            ReadCommand::DeleteAnnotation(
                pulpit_core::annotate::AnnotationId::imported("mark").expect("a usable name"),
            ),
            ReadCommand::EditSelected,
            ReadCommand::PickTime,
            ReadCommand::SaveAs,
        ] {
            assert!(
                App::read_command_mutates(&command),
                "{command:?} changes the document and must be refused"
            );
        }
        // Signing is the one addition append-only mode exists to permit.
        assert!(!App::read_command_mutates(&ReadCommand::Sign));
        // Printing is not a change to the document either. It writes a
        // scratch copy for a marked-up print, the same way signing does, and
        // reading a signed document and printing it is most of what anyone
        // does with one.
        assert!(!App::read_command_mutates(&ReadCommand::Print));
        assert!(!App::read_command_mutates(&ReadCommand::Arm(None)));
    }
}

#[cfg(test)]
mod sign_target_candidate_tests {
    // §31.1 step 5's target field decision, factored out of
    // `App::sign_target_candidates` as `App::sign_target_candidates_from_signals`
    // so it can be exercised against synthesized preflight outcomes rather
    // than PDF bytes — the engine's own preflight tests already cover which
    // outcome a given document produces.
    use super::App;
    use crate::signing::TargetChoice;
    use pulpit_render::verify::preflight::{PreflightOk, PreflightRefusal};

    fn ok(target_field: &str) -> PreflightOk {
        PreflightOk {
            target_field: target_field.to_string(),
            seed_value_ignored: false,
        }
    }

    #[test]
    fn an_unsigned_document_offers_its_own_empty_field_first_then_new_field() {
        // preflight_certify's Ok(()) means no /Sig field carries a value
        // yet. The sender's own empty field is offered first — so it is the
        // default the Options step selects — with NewField kept last as the
        // fallback that is always available.
        let result = App::sign_target_candidates_from_signals(Ok(()), Ok(ok("Sig1")));
        assert_eq!(
            result.candidates,
            vec![
                TargetChoice::ExistingField("Sig1".to_string()),
                TargetChoice::NewField,
            ]
        );
        assert!(!result.countersigning);
    }

    #[test]
    fn an_unsigned_document_with_no_field_at_all_offers_only_new_field() {
        // No empty /Sig field was found on an otherwise unsigned document —
        // NewField is the only sensible offer.
        let result = App::sign_target_candidates_from_signals(
            Ok(()),
            Err(PreflightRefusal::NoEmptySignatureField),
        );
        assert_eq!(result.candidates, vec![TargetChoice::NewField]);
        assert!(!result.countersigning);
    }

    #[test]
    fn an_unsigned_document_with_several_empty_fields_offers_all_of_them_then_new_field() {
        let result = App::sign_target_candidates_from_signals(
            Ok(()),
            Err(PreflightRefusal::AmbiguousSignatureField {
                candidates: vec!["Sig1".to_string(), "Sig2".to_string()],
            }),
        );
        assert_eq!(
            result.candidates,
            vec![
                TargetChoice::ExistingField("Sig1".to_string()),
                TargetChoice::ExistingField("Sig2".to_string()),
                TargetChoice::NewField,
            ]
        );
        assert!(!result.countersigning);
    }

    #[test]
    fn a_signed_document_offers_the_unambiguous_empty_field_and_never_new_field() {
        // A document already carrying a signature (certify refuses with
        // CertificationNotAllowed) must never offer NewField, because the
        // engine refuses NewInvisibleField outright once any /Sig has a /V.
        let result = App::sign_target_candidates_from_signals(
            Err(PreflightRefusal::CertificationNotAllowed {
                existing_signatures: 1,
            }),
            Ok(ok("Sig2")),
        );
        assert_eq!(
            result.candidates,
            vec![TargetChoice::ExistingField("Sig2".to_string())]
        );
        assert!(result.countersigning);
    }

    #[test]
    fn a_signed_document_with_several_empty_fields_offers_all_of_them() {
        let result = App::sign_target_candidates_from_signals(
            Err(PreflightRefusal::CertificationNotAllowed {
                existing_signatures: 1,
            }),
            Err(PreflightRefusal::AmbiguousSignatureField {
                candidates: vec!["Sig2".to_string(), "Sig3".to_string()],
            }),
        );
        assert_eq!(
            result.candidates,
            vec![
                TargetChoice::ExistingField("Sig2".to_string()),
                TargetChoice::ExistingField("Sig3".to_string()),
            ]
        );
        assert!(result.countersigning);
    }

    #[test]
    fn a_signed_document_with_no_empty_field_left_offers_nothing() {
        // The caller reports this the same way a `None` target already
        // does (see `sign_target_candidates`'s doc comment) — not NewField,
        // which the engine would refuse anyway.
        let result = App::sign_target_candidates_from_signals(
            Err(PreflightRefusal::CertificationNotAllowed {
                existing_signatures: 1,
            }),
            Err(PreflightRefusal::NoEmptySignatureField),
        );
        assert!(result.candidates.is_empty());
        assert!(result.countersigning);
    }

    #[test]
    fn an_encrypted_document_still_offers_new_field_rather_than_nothing() {
        // preflight_certify refuses an encrypted document with
        // EncryptedDocument, not CertificationNotAllowed — that is not
        // evidence of an existing signature, so the candidates must not
        // silently go empty. The engine's own preflight names the real
        // reason ("cannot sign an encrypted document") when Sign is
        // actually attempted.
        let result = App::sign_target_candidates_from_signals(
            Err(PreflightRefusal::EncryptedDocument),
            Err(PreflightRefusal::EncryptedDocument),
        );
        assert_eq!(result.candidates, vec![TargetChoice::NewField]);
        assert!(!result.countersigning);
    }

    #[test]
    fn an_unparseable_document_still_offers_new_field_rather_than_nothing() {
        let result = App::sign_target_candidates_from_signals(
            Err(PreflightRefusal::InvalidState("truncated xref".to_string())),
            Err(PreflightRefusal::InvalidState("truncated xref".to_string())),
        );
        assert_eq!(result.candidates, vec![TargetChoice::NewField]);
        assert!(!result.countersigning);
    }
}
