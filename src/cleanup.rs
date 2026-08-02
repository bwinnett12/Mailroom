// src/cleanup.rs
//
// Title cleanup — a flat, human-editable list of junk phrases to strip
// from incoming titles before they become a minted subnest's folder
// name. One phrase per line; blank lines and lines starting with `#`
// are ignored. Matching is a literal, case-insensitive substring — not
// regex — so the file stays editable without needing any pattern-
// matching syntax, just the exact junk text you're seeing.
//
// Loaded once at startup, same as schema.ron and Mailroom.toml — a new
// phrase needs a restart to take effect, same tradeoff as everything
// else loaded at boot today.
//
// Assumes ASCII-ish phrases and titles (case-folding on non-ASCII
// Unicode can change byte length, which this doesn't account for) —
// a reasonable simplification for personal ebook/paper titles, not a
// general-purpose text-cleaning tool.

use std::path::Path;

/// Read the phrase list from disk. Missing file, empty file, and a
/// file that's all comments are all fine — they just mean no cleanup
/// happens, not an error.
pub fn load(path: &Path) -> anyhow::Result<Vec<String>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect())
}

/// Strip every configured phrase out of `title` (case-insensitive,
/// literal substring), then collapse whatever separator debris the
/// removals leave behind (doubled spaces, a stray "--", etc.) down to
/// single separators before trimming the ends.
///
/// For cleanest results, include surrounding punctuation in the phrase
/// itself — e.g. "(z-lib.org)" rather than just "z-lib.org" — so
/// nothing is left dangling around the gap. The collapsing step is a
/// safety net for whatever's left, not a substitute for that.
pub fn clean(title: &str, phrases: &[String]) -> String {
    let mut cleaned = title.to_string();

    for phrase in phrases {
        if phrase.is_empty() {
            continue;
        }
        let lower_phrase = phrase.to_lowercase();
        let mut result = String::with_capacity(cleaned.len());
        let mut last_end = 0;
        let mut search_from = 0;
        loop {
            let lower_remaining = cleaned[search_from..].to_lowercase();
            match lower_remaining.find(&lower_phrase) {
                Some(pos) => {
                    let match_start = search_from + pos;
                    let match_end = match_start + phrase.len();
                    result.push_str(&cleaned[last_end..match_start]);
                    last_end = match_end;
                    search_from = match_end;
                }
                None => break,
            }
        }
        result.push_str(&cleaned[last_end..]);
        cleaned = result;
    }

    // Collapse runs of separator-ish characters (spaces, hyphens,
    // underscores, dots) left behind by removal down to a single one,
    // then trim them off the ends entirely.
    let mut collapsed = String::with_capacity(cleaned.len());
    let mut last_was_sep = false;
    for c in cleaned.chars() {
        let is_sep = matches!(c, ' ' | '-' | '_' | '.');
        if is_sep {
            if !last_was_sep {
                collapsed.push(c);
            }
            last_was_sep = true;
        } else {
            collapsed.push(c);
            last_was_sep = false;
        }
    }

    collapsed
        .trim_matches(|c: char| matches!(c, '-' | '_' | ' ' | '.'))
        .to_string()
}





// ── cleanup::clean tests — append to the end of src/cleanup.rs ────────────
#[cfg(test)]
mod clean_tests {
    use super::*;
 
    #[test]
    fn strips_a_configured_phrase_case_insensitively() {
        let phrases = vec!["(z-lib.org)".to_string()];
        assert_eq!(
            clean("American Sour Beers (Z-Lib.org)", &phrases),
            "American Sour Beers"
        );
    }
 
    #[test]
    fn strips_multiple_phrases_in_one_pass() {
        let phrases = vec!["[EPUB]".to_string(), "(Kindle Edition)".to_string()];
        assert_eq!(
            clean("My Book [EPUB] (Kindle Edition)", &phrases),
            "My Book"
        );
    }
 
    #[test]
    fn collapses_debris_left_by_a_mid_string_removal() {
        let phrases = vec!["-- Retail".to_string()];
        assert_eq!(
            clean("My Book -- Retail Edition", &phrases),
            "My Book Edition"
        );
    }
 
    #[test]
    fn no_matching_phrases_leaves_title_unchanged() {
        let phrases = vec!["(z-lib.org)".to_string()];
        assert_eq!(clean("A Perfectly Clean Title", &phrases), "A Perfectly Clean Title");
    }
 
    #[test]
    fn empty_phrase_list_is_a_no_op() {
        assert_eq!(clean("Untouched Title", &[]), "Untouched Title");
    }
}
