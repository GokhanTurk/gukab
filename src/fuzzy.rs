//! Shared typo-tolerant fuzzy matching, used by both the host list and the macro
//! picker.
//!
//! [`fuzzy_score`] is the heart: it returns `Some(score)` (higher = better) for a
//! match, or `None` for no match. The ranking is what makes typing "fen3" float
//! "Feneryolu-3" to the top — the closer/cleaner the match, the higher the score.
//! [`fuzzy_match`] is just `fuzzy_score(..).is_some()` for callers that only need
//! a yes/no.

/// Score tiers (higher tiers always beat lower ones, regardless of bonuses):
/// substring is the strongest signal, then subsequence (all chars in order with
/// gaps), then a fuzzy typo-tolerant fallback.
const SUBSTRING_BASE: i32 = 10_000;
const SUBSEQUENCE_BASE: i32 = 5_000;
const TYPO_BASE: i32 = 1_000;

/// Match `query` against `target` (both already lowercased). True if they match
/// at all — see [`fuzzy_score`] for the ranked variant.
pub fn fuzzy_match(query: &str, target: &str) -> bool {
    fuzzy_score(query, target).is_some()
}

/// Rank `query` against `target` (both already lowercased). Returns `Some(score)`
/// where a higher score means a closer match, or `None` if they do not match.
///
/// Tiers, strongest first:
/// 1. **Substring** — `target` contains `query` verbatim. Bonus for matching at
///    the start or right after a separator, penalty the further in it sits.
/// 2. **Subsequence** — all query chars appear in order with gaps. Bonus for
///    consecutive runs and matches at word boundaries; penalty for gaps.
/// 3. **Typo fallback** — a small edit distance, only for queries ≥3 chars.
///
/// Spaces in `query` are ignored throughout (so "fen 3" acts like "fen3").
pub fn fuzzy_score(query: &str, target: &str) -> Option<i32> {
    let q: String = query.chars().filter(|&c| c != ' ').collect();
    if q.is_empty() {
        return Some(0);
    }

    // Tier 1: exact substring (covers "erkez" in "merkez", "fen" in "feneryolu").
    if let Some(byte_pos) = target.find(&q) {
        let char_pos = target[..byte_pos].chars().count() as i32;
        let mut score = SUBSTRING_BASE - char_pos * 2; // earlier is better
        if byte_pos == 0 {
            score += 100; // matches at the very start
        } else if is_boundary(target, byte_pos) {
            score += 50; // matches right after a separator / case-shift
        }
        // Shorter targets that are "more fully" matched rank a touch higher.
        score -= (target.chars().count() as i32 - q.chars().count() as i32).max(0);
        return Some(score);
    }

    // Tier 2: subsequence with contiguity / boundary bonuses.
    if let Some(bonus) = subsequence_score(&q, target) {
        return Some(SUBSEQUENCE_BASE + bonus);
    }

    // Tier 3: typo tolerance, only for queries long enough that an edit-distance
    // match is meaningful — otherwise 1-2 char queries match almost anything.
    let qlen = q.chars().count();
    if qlen < 3 {
        return None;
    }
    let threshold = (qlen / 4).max(1);
    let dist = osa_substring_distance(&q, target);
    if dist <= threshold {
        return Some(TYPO_BASE - dist as i32 * 100);
    }
    None
}

/// Greedy subsequence scorer: walks `target` once, matching each `query` char in
/// order. Rewards consecutive matches and matches at word boundaries; penalizes
/// the gaps between matched chars. Returns `None` if not all query chars are
/// found in order. `query` is assumed already space-stripped and lowercased.
fn subsequence_score(query: &str, target: &str) -> Option<i32> {
    let t: Vec<char> = target.chars().collect();
    let mut score = 0i32;
    let mut ti = 0usize;
    let mut prev_match: Option<usize> = None;

    for qc in query.chars() {
        // Advance through target until this query char is found.
        let mut matched = None;
        while ti < t.len() {
            if t[ti] == qc {
                matched = Some(ti);
                break;
            }
            ti += 1;
        }
        let mi = matched?;

        // Boundary bonus: start, after a separator, or a letter→digit shift.
        if mi == 0 || is_sep(t[mi - 1]) || (t[mi - 1].is_alphabetic() && t[mi].is_numeric()) {
            score += 30;
        }
        match prev_match {
            Some(p) if mi == p + 1 => score += 20, // consecutive run
            Some(p) => score -= (mi - p - 1) as i32, // gap penalty
            None => score -= mi as i32,             // leading gap before first match
        }

        prev_match = Some(mi);
        ti = mi + 1;
    }
    Some(score)
}

/// Separator characters that mark a word boundary in host/macro names.
fn is_sep(c: char) -> bool {
    matches!(c, '-' | '_' | ' ' | '.' | '/' | ':' | '@')
}

/// True if a substring starting at byte `pos` begins at a word boundary: right
/// after a separator, or at a letter→digit transition (so "3" in "feneryolu-3").
fn is_boundary(target: &str, pos: usize) -> bool {
    let prev = target[..pos].chars().next_back();
    let cur = target[pos..].chars().next();
    match (prev, cur) {
        (Some(p), _) if is_sep(p) => true,
        (Some(p), Some(c)) if p.is_alphabetic() && c.is_numeric() => true,
        _ => false,
    }
}

/// Minimum Optimal String Alignment distance (Levenshtein plus adjacent
/// transpositions) of `query` against the best-matching substring of `target`.
///
/// The first DP row is all zeros so a match may start anywhere in `target`, and
/// the result is the minimum of the last row so it may end anywhere. Runs in
/// `O(query_len * target_len)` with three rolling rows (the third holds row i-2,
/// needed for the transposition case).
fn osa_substring_distance(query: &str, target: &str) -> usize {
    let q: Vec<char> = query.chars().collect();
    let t: Vec<char> = target.chars().collect();
    let (m, n) = (q.len(), t.len());
    if m == 0 {
        return 0;
    }
    if n == 0 {
        return m;
    }

    let mut prev2 = vec![0usize; n + 1]; // row i-2
    let mut prev = vec![0usize; n + 1]; // row i-1 (row 0 = all zeros: start anywhere)
    let mut cur = vec![0usize; n + 1];

    for i in 1..=m {
        cur[0] = i; // deleting the first i query chars
        for j in 1..=n {
            let cost = if q[i - 1] == t[j - 1] { 0 } else { 1 };
            let mut val = (prev[j] + 1) // delete from query
                .min(cur[j - 1] + 1) // skip a target char
                .min(prev[j - 1] + cost); // substitute / match
            if i > 1 && j > 1 && q[i - 1] == t[j - 2] && q[i - 2] == t[j - 1] {
                val = val.min(prev2[j - 2] + 1); // adjacent transposition
            }
            cur[j] = val;
        }
        std::mem::swap(&mut prev2, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }
    // After the final swaps `prev` holds row m; match may end at any column.
    prev.iter().copied().min().unwrap_or(m)
}

#[cfg(test)]
mod tests {
    use super::{fuzzy_match, fuzzy_score};

    /// Of several targets, the best-ranked one for `query`. Mirrors how the host
    /// list picks which row to select.
    fn best<'a>(query: &str, targets: &[&'a str]) -> &'a str {
        targets
            .iter()
            .filter_map(|t| fuzzy_score(query, t).map(|s| (s, *t)))
            .max_by_key(|(s, _)| *s)
            .map(|(_, t)| t)
            .unwrap_or("")
    }

    #[test]
    fn substring_matches() {
        assert!(fuzzy_match("merkez", "merkeza-2kat01"));
        assert!(fuzzy_match("erkez", "merkeza-2kat01")); // mid-substring
    }

    #[test]
    fn transposition_matches() {
        assert!(fuzzy_match("merkze", "merkeza-2kat01")); // the reported case
        assert!(fuzzy_match("mrekez", "merkeza-2kat01")); // another transposition
    }

    #[test]
    fn single_typo_matches() {
        assert!(fuzzy_match("merkex", "merkeza-2kat01")); // one substitution
    }

    #[test]
    fn unrelated_does_not_match() {
        assert!(!fuzzy_match("zzzzz", "merkeza-2kat01"));
        assert!(!fuzzy_match("ankara", "merkeza-2kat01"));
    }

    #[test]
    fn short_queries_are_substring_only() {
        assert!(fuzzy_match("me", "merkeza-2kat01")); // substring
        assert!(!fuzzy_match("xy", "merkeza-2kat01")); // no fuzzy for <3 chars
    }

    #[test]
    fn subsequence_matches() {
        assert!(fuzzy_match("mer2", "merkezd-2kat")); // all chars in order with gaps
        assert!(fuzzy_match("mer 2", "merkezd-2kat")); // space in query is skipped
        assert!(fuzzy_match("mrd", "merkezd")); // skip middle chars
        assert!(fuzzy_match("2kat", "merkezd-2kat")); // from middle onwards
        assert!(fuzzy_match("a1", "merkeza-1kat")); // abbreviated hostname
    }

    #[test]
    fn subsequence_no_match() {
        assert!(!fuzzy_match("xxxx", "merkeza-2kat")); // x not present
        assert!(!fuzzy_match("mzzz", "merkeza-2kat")); // zzz not present after m
    }

    #[test]
    fn ranking_picks_closest() {
        // The reported case: "fen3" should rank "feneryolu-3" above the siblings,
        // even though all four fuzzily match.
        let fen = ["feneryolu-1", "feneryolu-2", "feneryolu-3", "feneryolu-4"];
        assert_eq!(best("fen3", &fen), "feneryolu-3");
        assert_eq!(best("fen1", &fen), "feneryolu-1");
        assert_eq!(best("fen 4", &fen), "feneryolu-4"); // space ignored

        // A clean prefix should beat a mid-string substring of the same length.
        assert!(fuzzy_score("mer", "merkez").unwrap() > fuzzy_score("mer", "izmer").unwrap());

        // A subsequence/typo match must always rank below any real substring.
        assert!(fuzzy_score("fen3", "feneryolu-3").unwrap() > fuzzy_score("fen3", "feneryolu-1").unwrap());
    }
}
