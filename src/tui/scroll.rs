//! How far the transcript is scrolled, and the plan header a toggle wants held in place.
//!
//! Expanding or collapsing a plan asks the next frame to keep the card's header on its row.
//! A key or the wheel that moves the transcript after the toggle, before that frame, is the
//! newer request, so it cancels the pin. Only `set` changes the scroll, and it cancels the
//! pin, so no handler can move the transcript and leave the pin behind.

#[derive(Default)]
pub struct Scroll {
    /// Rows scrolled up from the bottom. Zero follows new output.
    rows: usize,
    /// A plan just expanded or collapsed and the row its header keeps in the next frame.
    pin: Option<(String, isize)>,
}

impl Scroll {
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Moves the transcript and cancels a pending pin. It cancels it even when `rows` is the
    /// scroll already set, as for `G` at the bottom, because the reader still asked to be there.
    pub fn set(&mut self, rows: usize) {
        self.rows = rows;
        self.pin = None;
    }

    /// Asks the next frame to keep plan `id`'s header on screen row `row`, which is negative
    /// above the screen.
    pub fn pin(&mut self, id: String, row: isize) {
        self.pin = Some((id, row));
    }

    /// The pin for the frame being drawn. A pin applies to one frame.
    pub fn take_pin(&mut self) -> Option<(String, isize)> {
        self.pin.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pinned(rows: usize) -> Scroll {
        let mut scroll = Scroll::default();
        scroll.set(rows);
        scroll.pin("plan".into(), 4);
        scroll
    }

    #[test]
    fn a_pin_lasts_one_frame() {
        let mut scroll = pinned(0);
        assert_eq!(scroll.take_pin(), Some(("plan".into(), 4)));
        assert_eq!(scroll.take_pin(), None);
        // A second toggle before the frame replaces the first.
        scroll.pin("plan".into(), 4);
        scroll.pin("other".into(), -2);
        assert_eq!(scroll.take_pin(), Some(("other".into(), -2)));
    }

    #[test]
    fn a_scroll_after_a_toggle_wins_over_its_pin() {
        // `p` then `G` or `Down` at the bottom. The scroll stays 0, so the view follows the
        // bottom, but only once the pin is gone.
        let mut scroll = pinned(0);
        scroll.set(0);
        assert_eq!((scroll.rows(), scroll.take_pin()), (0, None));
        let mut scroll = pinned(0);
        scroll.set(scroll.rows().saturating_sub(1));
        assert_eq!((scroll.rows(), scroll.take_pin()), (0, None));

        // `p` then `k`, the wheel or PageUp while reading higher up.
        let mut scroll = pinned(10);
        scroll.set(scroll.rows() + 3);
        assert_eq!((scroll.rows(), scroll.take_pin()), (13, None));

        // `p` then `g`.
        let mut scroll = pinned(10);
        scroll.set(usize::MAX / 2);
        assert_eq!(scroll.take_pin(), None);
    }
}
