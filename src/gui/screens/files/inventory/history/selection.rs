//! Multi-selection of trash entries: click selects one, Ctrl/Cmd+click
//! toggles, Shift+click selects the range from the last clicked entry, and
//! the check boxes toggle single entries or all shown ones.

use std::collections::BTreeSet;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Selection {
    ids: BTreeSet<String>,
    /// Last clicked entry, the start of a Shift+click range.
    anchor: Option<String>,
}

impl Selection {
    pub(crate) fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.ids.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// A click on `id`; `order` is the shown entries' ids, top to bottom.
    pub(crate) fn click(&mut self, id: &str, order: &[&str], toggle: bool, range: bool) {
        let anchor = self
            .anchor
            .as_deref()
            .and_then(|anchor| order.iter().position(|&o| o == anchor));
        match (range, anchor, order.iter().position(|&o| o == id)) {
            (true, Some(from), Some(to)) => {
                if !toggle {
                    self.ids.clear();
                }
                let (low, high) = (from.min(to), from.max(to));
                self.ids
                    .extend(order[low..=high].iter().map(|id| id.to_string()));
                return; // The anchor stays for the next range.
            }
            _ if toggle => self.toggle(id),
            _ => {
                self.ids.clear();
                self.ids.insert(id.to_string());
            }
        }
        self.anchor = Some(id.to_string());
    }

    /// The check box of one entry.
    pub(crate) fn toggle(&mut self, id: &str) {
        if !self.ids.remove(id) {
            self.ids.insert(id.to_string());
        }
        self.anchor = Some(id.to_string());
    }

    /// Whether every shown entry is selected (false when none are shown).
    pub(crate) fn all(&self, order: &[&str]) -> bool {
        !order.is_empty() && order.iter().all(|id| self.ids.contains(*id))
    }

    /// The header check box: select every shown entry, or clear them.
    pub(crate) fn set_all(&mut self, order: &[&str], on: bool) {
        for id in order {
            if on {
                self.ids.insert(id.to_string());
            } else {
                self.ids.remove(*id);
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        self.ids.clear();
        self.anchor = None;
    }

    /// Drops ids that are no longer listed (after a refresh).
    pub(crate) fn retain(&mut self, listed: &[&str]) {
        self.ids.retain(|id| listed.contains(&id.as_str()));
        if self
            .anchor
            .as_deref()
            .is_some_and(|anchor| !listed.contains(&anchor))
        {
            self.anchor = None;
        }
    }

    /// Selected ids in `order` (hidden selected entries are left out).
    pub(crate) fn in_order(&self, order: &[&str]) -> Vec<String> {
        order
            .iter()
            .filter(|id| self.ids.contains(**id))
            .map(|id| id.to_string())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDER: [&str; 5] = ["a", "b", "c", "d", "e"];

    #[test]
    fn click_toggle_and_range() {
        let mut s = Selection::default();
        s.click("b", &ORDER, false, false);
        assert_eq!(s.in_order(&ORDER), ["b"]);
        s.click("d", &ORDER, false, true);
        assert_eq!(s.in_order(&ORDER), ["b", "c", "d"]);
        // The anchor stays at "b": a new range replaces the old one.
        s.click("a", &ORDER, false, true);
        assert_eq!(s.in_order(&ORDER), ["a", "b"]);
        s.click("e", &ORDER, true, false);
        assert_eq!(s.in_order(&ORDER), ["a", "b", "e"]);
        s.click("a", &ORDER, true, false);
        assert_eq!(s.in_order(&ORDER), ["b", "e"]);
        s.click("c", &ORDER, false, false);
        assert_eq!(s.in_order(&ORDER), ["c"]);
        // Shift without an anchor in the list selects just the entry.
        let mut fresh = Selection::default();
        fresh.click("d", &ORDER, false, true);
        assert_eq!(fresh.in_order(&ORDER), ["d"]);
    }

    #[test]
    fn select_all_retain_and_clear() {
        let mut s = Selection::default();
        assert!(!s.all(&[]));
        s.set_all(&ORDER[..3], true);
        assert!(s.all(&ORDER[..3]) && !s.all(&ORDER));
        s.toggle("b");
        assert_eq!(s.len(), 2);
        s.retain(&["c", "x"]);
        assert_eq!(s.in_order(&ORDER), ["c"]);
        s.set_all(&ORDER, false);
        assert!(s.is_empty());
        s.toggle("a");
        s.clear();
        assert!(s.is_empty() && !s.contains("a"));
    }
}
