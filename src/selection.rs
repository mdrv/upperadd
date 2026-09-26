//! Block-level text selection shared by the sticky body and the overlay
//! preview pane. Glyph-accurate selection would need per-block text-layout
//! hit-testing (gpui has no selectable-text element); whole blocks are the
//! v0.1.1 unit — click a paragraph, drag across several, copy the range.

use std::ops::RangeInclusive;
use std::time::{Duration, Instant};

/// How long the "copied" chip stays visible (timed show/hide — never fade,
/// §28 animation-freeze rule).
pub const COPIED_CHIP: Duration = Duration::from_millis(1500);

/// Selection state for one pane (sticky body / preview pane).
#[derive(Default)]
pub struct PaneSel {
    sel: Option<Sel>,
    dragging: bool,
    copied_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Sel {
    anchor: usize,
    head: usize,
}

impl PaneSel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether block `ix` falls inside the current selection (paint flag).
    pub fn is_selected(&self, ix: usize) -> bool {
        self.sel_range().is_some_and(|r| r.contains(&ix))
    }

    /// The selected block range, anchor/head normalized.
    pub fn sel_range(&self) -> Option<RangeInclusive<usize>> {
        let s = self.sel?;
        Some(s.anchor.min(s.head)..=s.anchor.max(s.head))
    }

    /// Mouse-down on a block: start (or restart) the selection there.
    pub fn block_down(&mut self, ix: usize) -> bool {
        self.dragging = true;
        self.copied_at = None;
        let same = self.sel
            == Some(Sel {
                anchor: ix,
                head: ix,
            });
        self.sel = Some(Sel {
            anchor: ix,
            head: ix,
        });
        !same
    }

    /// Mouse moved over a block while dragging: extend the selection.
    pub fn block_drag(&mut self, ix: usize) -> bool {
        if !self.dragging {
            return false;
        }
        let Some(sel) = &mut self.sel else {
            return false;
        };
        if sel.head == ix {
            return false;
        }
        sel.head = ix;
        true
    }

    /// Mouse-up: finish the drag. True when a selection exists to act on.
    pub fn block_up(&mut self) -> bool {
        self.dragging = false;
        self.sel.is_some()
    }

    /// Mark "copied" now (chip shows for COPIED_CHIP).
    pub fn copied(&mut self) {
        self.copied_at = Some(Instant::now());
    }

    pub fn chip_visible(&self) -> bool {
        self.copied_at.is_some_and(|t| t.elapsed() < COPIED_CHIP)
    }

    /// Drop the selection (preview switched sections, empty-space click…).
    /// True when there was anything to drop.
    pub fn clear(&mut self) -> bool {
        let had = self.sel.is_some() || self.dragging;
        self.sel = None;
        self.dragging = false;
        had
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_selects_normalized_range() {
        let mut sel = PaneSel::new();
        assert!(sel.block_down(3));
        assert!(sel.block_drag(1));
        assert!(sel.block_up());
        assert_eq!(sel.sel_range(), Some(1..=3));
        assert!(sel.is_selected(2));
        assert!(!sel.is_selected(4));
    }

    #[test]
    fn drag_without_down_is_ignored() {
        let mut sel = PaneSel::new();
        assert!(!sel.block_drag(2));
        assert!(!sel.block_up());
        assert_eq!(sel.sel_range(), None);
    }

    #[test]
    fn down_restarts_the_selection() {
        let mut sel = PaneSel::new();
        sel.block_down(1);
        sel.block_drag(5);
        sel.block_up();
        // Plain click on an earlier block collapses to just that block.
        assert!(sel.block_down(0));
        sel.block_up();
        assert_eq!(sel.sel_range(), Some(0..=0));
    }

    #[test]
    fn chip_shows_until_cleared() {
        let mut sel = PaneSel::new();
        assert!(!sel.chip_visible());
        sel.copied();
        assert!(sel.chip_visible());
    }
}
