//! Pure state model for the overlay search: query editing at a char
//! cursor, result selection, insert/normal mode, stale-reply guard.
//! No gpui here — everything is unit-testable.

use crate::worker::SearchHit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Typing edits the query (input is focused).
    Insert,
    /// Vim-like: j/k navigate, P/Tab pin, Esc hides, `i` returns to insert.
    Normal,
}

pub struct SearchModel {
    pub query: String,
    /// Char offset into `query` (not a byte offset).
    pub cursor: usize,
    pub results: Vec<SearchHit>,
    pub selected: usize,
    pub mode: Mode,
    /// Bumped for every request; replies carry the generation they answer,
    /// so out-of-order replies can't overwrite fresher results.
    generation: u64,
}

impl SearchModel {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            cursor: 0,
            results: Vec::new(),
            selected: 0,
            mode: Mode::Insert,
            generation: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.results.len()
    }

    /// Next generation index for a search request.
    pub fn bump_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// Apply a reply only if it answers the newest request.
    /// Returns whether state changed (caller notifies).
    pub fn apply_results(&mut self, results: Vec<SearchHit>, generation: u64) -> bool {
        if generation != self.generation {
            return false;
        }
        self.results = results;
        self.selected = self.selected.min(self.len().saturating_sub(1));
        true
    }

    /// fzf-style wraparound.
    pub fn move_selection(&mut self, delta: i64) -> bool {
        let len = self.len();
        if len == 0 {
            return false;
        }
        let next = (self.selected as i64 + delta).rem_euclid(len as i64) as usize;
        if next == self.selected {
            return false;
        }
        self.selected = next;
        true
    }

    pub fn enter_insert(&mut self) {
        self.mode = Mode::Insert;
    }

    pub fn enter_normal(&mut self) {
        self.mode = Mode::Normal;
    }

    fn byte_offset(&self, char_idx: usize) -> usize {
        self.query
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.query.len())
    }

    pub fn insert(&mut self, ch: char) -> bool {
        let byte = self.byte_offset(self.cursor);
        self.query.insert(byte, ch);
        self.cursor += 1;
        true
    }

    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let byte = self.byte_offset(self.cursor);
        let prev = self.byte_offset(self.cursor - 1);
        self.query.replace_range(prev..byte, "");
        self.cursor -= 1;
        true
    }

    pub fn delete(&mut self) -> bool {
        if self.cursor >= self.query.chars().count() {
            return false;
        }
        let byte = self.byte_offset(self.cursor);
        let next = self.byte_offset(self.cursor + 1);
        self.query.replace_range(byte..next, "");
        true
    }

    pub fn left(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        true
    }

    pub fn right(&mut self) -> bool {
        if self.cursor >= self.query.chars().count() {
            return false;
        }
        self.cursor += 1;
        true
    }

    pub fn home(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor = 0;
        true
    }

    pub fn end(&mut self) -> bool {
        let len = self.query.chars().count();
        if self.cursor >= len {
            return false;
        }
        self.cursor = len;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(path: &str) -> SearchHit {
        SearchHit {
            path: path.into(),
            line: 1,
            title: path.into(),
            section_mtime: 0,
            score: 0,
        }
    }

    #[test]
    fn editing_happens_at_the_char_cursor() {
        let mut m = SearchModel::new();
        assert!(m.insert('a'));
        assert!(m.insert('b'));
        assert!(m.insert('ä'));
        assert_eq!(m.query, "abä");
        assert_eq!(m.cursor, 3);

        assert!(m.left());
        assert!(m.left());
        assert_eq!(m.cursor, 1);
        assert!(m.insert('X')); // "aXbä"
        assert_eq!(m.query, "aXbä");

        assert!(m.backspace()); // removes 'X'
        assert_eq!(m.query, "abä");
        assert!(m.delete()); // removes 'b' under cursor
        assert_eq!(m.query, "aä");
        assert_eq!(m.cursor, 1);

        assert!(m.home());
        assert!(!m.backspace()); // nothing before cursor
        assert!(m.end());
        assert!(!m.delete()); // nothing after cursor
        assert!(!m.right());
    }

    #[test]
    fn stale_generations_are_rejected() {
        let mut m = SearchModel::new();
        let g1 = m.bump_generation();
        let g2 = m.bump_generation();
        assert!(!m.apply_results(vec![hit("old")], g1));
        assert!(m.apply_results(vec![hit("new")], g2));
        assert_eq!(m.results[0].path, "new");
    }

    #[test]
    fn selection_clamps_and_wraps() {
        let mut m = SearchModel::new();
        let g = m.bump_generation();
        m.apply_results(vec![hit("a"), hit("b"), hit("c")], g);
        assert!(m.move_selection(1));
        assert_eq!(m.selected, 1);
        assert!(m.move_selection(-1));
        assert_eq!(m.selected, 0);
        assert!(m.move_selection(-1));
        assert_eq!(m.selected, 2); // wrapped above the start
        assert!(m.move_selection(1));
        assert_eq!(m.selected, 0); // wrapped back around

        // Shrink under the selection.
        let g = m.bump_generation();
        m.apply_results(vec![hit("a")], g);
        assert_eq!(m.selected, 0);
        assert!(!m.move_selection(1)); // single item: no move
        assert_eq!(m.selected, 0);
    }
}
