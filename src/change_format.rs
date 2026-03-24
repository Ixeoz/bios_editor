//! History entries store "old → new"; this splits on that arrow.

/// `before → after` → (before, after). Arrow is the real unicode one, not ASCII ->.
pub fn split_change_summary(summary: &str) -> Option<(String, String)> {
    let (before, after) = summary.split_once('→')?;
    Some((before.trim().to_string(), after.trim().to_string()))
}
