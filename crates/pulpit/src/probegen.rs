//! Which asking of a question is current.
//!
//! A probe that runs on a helper thread answers *later*, and between the
//! asking and the answer somebody may ask again — the concrete case being the
//! startup appearance probe still out on its thread when a suspend/resume
//! refreshes the same preferences synchronously. The late answer then carries
//! the pre-suspend desktop, and applying it would put a stale theme back.
//!
//! The discipline is a counter: an answer carries the generation it was asked
//! under, and only the current generation's answer is applied. Asking again —
//! synchronously or not — advances the generation, which is what makes every
//! earlier answer recognisably stale.

/// The counter. `Copy` tokens go out with each asking; `advance` invalidates
/// every token minted before it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ProbeGeneration(u64);

impl ProbeGeneration {
    /// The token to send with a probe being launched now.
    pub fn current(self) -> u64 {
        self.0
    }

    /// The question has been answered by other means (or asked again):
    /// everything still in flight is now stale.
    pub fn advance(&mut self) {
        self.0 = self.0.wrapping_add(1);
    }

    /// Is an answer carrying this token still worth applying?
    pub fn accepts(self, token: u64) -> bool {
        self.0 == token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_from_before_a_refresh_is_stale() {
        // The resume-vs-startup race: the probe is launched, a resume
        // refreshes the preferences synchronously, and only then does the
        // probe's answer land. It must be dropped, or the pre-suspend theme
        // comes back.
        let mut generation = ProbeGeneration::default();
        let launched_with = generation.current();
        generation.advance(); // the resume refresh
        assert!(!generation.accepts(launched_with));
    }

    #[test]
    fn an_undisturbed_answer_is_applied() {
        let generation = ProbeGeneration::default();
        assert!(generation.accepts(generation.current()));
    }

    #[test]
    fn each_refresh_invalidates_everything_before_it() {
        let mut generation = ProbeGeneration::default();
        let first = generation.current();
        generation.advance();
        let second = generation.current();
        generation.advance();
        assert!(!generation.accepts(first));
        assert!(!generation.accepts(second));
        assert!(generation.accepts(generation.current()));
    }
}
