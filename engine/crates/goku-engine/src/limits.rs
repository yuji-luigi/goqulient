//! Port of GokuCore's `ResultLimits` / `RowCollector` / `Paging`: no result ever holds more than `HARD_MAX_ROWS` rows,
//! and long cells are clipped. Drivers stream rows into a `RowCollector` and stop reading once `append` returns false.
use unicode_segmentation::UnicodeSegmentation;

/// No result grid ever holds more rows than this, regardless of settings.
pub const HARD_MAX_ROWS: usize = 100_000;
/// Individual cell values are truncated beyond this many characters for display.
pub const MAX_CELL_CHARACTERS: usize = 32 * 1024;
/// Appended to a cell (or Mongo document) cut at `MAX_CELL_CHARACTERS`.
pub const CLIP_MARKER: &str = "… [truncated]";
/// Default `maxRows` of a connection profile.
pub const DEFAULT_MAX_ROWS: usize = 1000;

/// True when a value was clipped, so copying it needs a full-fidelity re-read.
pub fn is_clipped(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.ends_with(CLIP_MARKER))
}

/// Same rule as Swift: over the limit in UTF-8 bytes → keep the first `MAX_CELL_CHARACTERS` characters (grapheme
/// clusters, which is what a Swift `Character` is) and add the marker.
pub fn clip(s: &str) -> String {
    clip_to(s, MAX_CELL_CHARACTERS)
}

fn clip_to(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    match s.grapheme_indices(true).nth(max) {
        Some((end, _)) => format!("{}{CLIP_MARKER}", &s[..end]),
        None => format!("{s}{CLIP_MARKER}"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowCollector {
    max_rows: usize,
    rows: Vec<Vec<Option<String>>>,
    truncated: bool,
}

impl RowCollector {
    pub fn new(max_rows: usize) -> Self {
        Self { max_rows: max_rows.clamp(1, HARD_MAX_ROWS), rows: Vec::new(), truncated: false }
    }

    pub fn max_rows(&self) -> usize {
        self.max_rows
    }

    pub fn is_full(&self) -> bool {
        self.rows.len() >= self.max_rows
    }

    /// Returns false once the cap is reached (the caller should stop reading).
    pub fn append(&mut self, row: Vec<Option<String>>) -> bool {
        if self.rows.len() >= self.max_rows {
            self.truncated = true;
            return false;
        }
        self.rows.push(row.into_iter().map(|c| c.map(|v| clip(&v))).collect());
        true
    }

    pub fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn rows(&self) -> &[Vec<Option<String>>] {
        &self.rows
    }

    pub fn into_rows(self) -> (Vec<Vec<Option<String>>>, bool) {
        (self.rows, self.truncated)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Paging {
    pub offset: usize,
    pub limit: usize,
}

impl Paging {
    pub fn new(offset: i64, limit: i64) -> Self {
        Self { offset: offset.max(0) as usize, limit: (limit.max(1) as usize).min(HARD_MAX_ROWS) }
    }
}

impl Default for Paging {
    fn default() -> Self {
        Self::new(0, 500)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collector_caps_and_flags() {
        let mut c = RowCollector::new(2);
        assert!(c.append(vec![Some("a".into())]));
        assert!(c.append(vec![None]));
        assert!(c.is_full() && !c.truncated());
        assert!(!c.append(vec![Some("c".into())]));
        assert!(c.truncated());
        assert_eq!(c.rows().len(), 2);
    }

    #[test]
    fn collector_limits_are_clamped() {
        assert_eq!(RowCollector::new(0).max_rows(), 1);
        assert_eq!(RowCollector::new(10_000_000).max_rows(), HARD_MAX_ROWS);
    }

    #[test]
    fn clip_counts_bytes_then_keeps_characters() {
        assert_eq!(clip_to("abc", 3), "abc");
        assert_eq!(clip_to("abcd", 3), format!("abc{CLIP_MARKER}"));
        // 3 characters but 6 bytes: over the byte limit, yet nothing to cut beyond 3 characters.
        assert_eq!(clip_to("ééé", 3), format!("ééé{CLIP_MARKER}"));
        // A flag emoji is one grapheme cluster: never split in the middle.
        assert_eq!(clip_to("🇮🇹🇯🇵x", 1), format!("🇮🇹{CLIP_MARKER}"));
        let long = "x".repeat(MAX_CELL_CHARACTERS + 5);
        assert!(is_clipped(Some(&clip(&long))));
        assert!(!is_clipped(Some("short")) && !is_clipped(None));
    }

    #[test]
    fn paging_clamps() {
        assert_eq!(Paging::new(-5, 0), Paging { offset: 0, limit: 1 });
        assert_eq!(Paging::new(3, 1_000_000), Paging { offset: 3, limit: HARD_MAX_ROWS });
    }
}
