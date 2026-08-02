//! One-frame arbitration for Poolrooms' animated waiting raft.

use dwemer_poolrooms::water::Surface;
use egui::{Context, Rect};

/// The single living waiting region admitted by one Poolrooms surface.
///
/// Product widgets claim their visible rectangle while drawing. Composition
/// consumes the largest claim, which lets a prominent waiting card supersede a
/// concurrent status label without coupling either caller to draw order.
#[derive(Debug, Default)]
pub struct LivingWait {
    claim: Option<Rect>,
}

impl LivingWait {
    /// Mark a visible widget or panel as waiting during the current frame.
    pub fn claim(&mut self, rect: Rect) {
        if !rect.is_positive() {
            return;
        }
        if self
            .claim
            .is_none_or(|incumbent| area(rect) > area(incumbent))
        {
            self.claim = Some(rect);
        }
    }

    /// Reconcile this frame's claims with the physical loading raft.
    ///
    /// Consuming the claim makes disappearance automatic: a later frame with
    /// no waiting widget always settles the raft.
    pub fn compose(&mut self, ctx: &Context, water: &mut Surface) {
        match self.claim.take() {
            Some(rect) => water.show_loading(ctx, rect),
            None => water.hide_loading(),
        }
    }
}

fn area(rect: Rect) -> f32 {
    rect.width() * rect.height()
}

#[cfg(test)]
mod tests {
    use egui::{Rect, pos2};

    use super::*;

    #[test]
    fn largest_waiting_surface_wins_without_draw_order_authority() {
        let small = Rect::from_min_max(pos2(2.0, 3.0), pos2(12.0, 8.0));
        let large = Rect::from_min_max(pos2(20.0, 30.0), pos2(60.0, 70.0));

        let mut forward = LivingWait::default();
        forward.claim(small);
        forward.claim(large);

        let mut backward = LivingWait::default();
        backward.claim(large);
        backward.claim(small);

        assert_eq!(forward.claim, Some(large));
        assert_eq!(backward.claim, Some(large));
    }

    #[test]
    fn dead_rectangles_cannot_arm_animation() {
        let mut wait = LivingWait::default();
        wait.claim(Rect::ZERO);
        wait.claim(Rect::NOTHING);
        assert_eq!(wait.claim, None);
    }

    #[test]
    fn frame_claim_is_consumed_exactly_once() {
        let rect = Rect::from_min_max(pos2(2.0, 3.0), pos2(12.0, 8.0));
        let mut wait = LivingWait::default();
        wait.claim(rect);
        assert_eq!(wait.claim.take(), Some(rect));
        assert_eq!(wait.claim.take(), None);
    }
}
