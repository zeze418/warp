//! Chinese (CJK) word segmentation for the rich-text word-boundary machinery.
//!
//! The default [`super::word_boundaries`] logic only ever breaks words on whitespace and a fixed
//! set of ASCII punctuation. CJK scripts (notably Chinese) write words with no separators between
//! them, so a whole run of Han characters is otherwise treated as a single "word" — making
//! word-wise cursor movement and deletion jump over (or delete) the entire run at once.
//!
//! This module fills that gap by running a dictionary-based segmenter (`jieba-rs`) over runs of
//! CJK characters and reporting the *interior* boundaries it finds. The run's outer edges are
//! already handled by the separator-based logic, so only boundaries strictly inside a run are
//! returned here.

use std::sync::OnceLock;

use jieba_rs::Jieba;

/// A process-wide, lazily-initialized segmenter. Building it loads the embedded dictionary, which
/// is relatively expensive, so we do it exactly once. `Jieba` is read-only after construction
/// (`cut` takes `&self`), so it is safe to share across threads.
fn jieba() -> &'static Jieba {
    static JIEBA: OnceLock<Jieba> = OnceLock::new();
    JIEBA.get_or_init(Jieba::new)
}

/// Whether `c` is a CJK (Han) ideograph that should participate in dictionary segmentation.
///
/// We intentionally restrict this to Han ideographs (the script `jieba` is trained on). Kana and
/// Hangul are deliberately excluded so the Chinese segmenter does not mangle Japanese/Korean text;
/// those scripts fall back to the default separator-based behavior.
pub fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x3400..=0x4DBF      // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF    // CJK Unified Ideographs
        | 0xF900..=0xFAFF    // CJK Compatibility Ideographs
        | 0x20000..=0x2A6DF  // CJK Unified Ideographs Extension B
        | 0x2A700..=0x2EBEF  // CJK Unified Ideographs Extension C–F
    )
}

/// Find every CJK run in `chars` and return the *interior* word boundaries within each run, as
/// absolute character offsets (i.e. `base` plus the run-relative offset).
///
/// "Interior" means the run's leading and trailing edges are excluded — those are already produced
/// by the separator-based boundary logic. For example, for the run `"我喜欢吃苹果"` segmented as
/// `我 | 喜欢 | 吃 | 苹果`, this returns the offsets *between* those words, not the run's start/end.
pub fn interior_cuts_in_chars(chars: &[char], base: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !is_cjk(chars[i]) {
            i += 1;
            continue;
        }

        // Extend through the maximal contiguous run of CJK characters.
        let run_start = i;
        let mut run_end = i + 1;
        while run_end < chars.len() && is_cjk(chars[run_end]) {
            run_end += 1;
        }

        let run: String = chars[run_start..run_end].iter().collect();
        let words = jieba().cut(&run, true);
        let word_count = words.len();
        let mut acc = run_start;
        for (k, word) in words.iter().enumerate() {
            acc += word.chars().count();
            // Skip the boundary after the last word: that is the run's trailing edge.
            if k + 1 < word_count {
                out.push(base + acc);
            }
        }

        i = run_end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cuts(text: &str) -> Vec<usize> {
        let chars: Vec<char> = text.chars().collect();
        interior_cuts_in_chars(&chars, 0)
    }

    #[test]
    fn segments_basic_sentence() {
        // 我 | 喜欢 | 吃 | 苹果  -> interior boundaries at 1, 3, 4
        assert_eq!(cuts("我喜欢吃苹果"), vec![1, 3, 4]);
    }

    #[test]
    fn single_word_has_no_interior_cuts() {
        // A two-character compound is one word; no interior boundary.
        assert_eq!(cuts("世界"), Vec::<usize>::new());
        assert_eq!(cuts("好"), Vec::<usize>::new());
    }

    #[test]
    fn applies_base_offset_and_ignores_non_cjk() {
        // "ab你好世界" -> run starts at char 2; 你好 | 世界 cut at absolute offset 4.
        let chars: Vec<char> = "ab你好世界".chars().collect();
        assert_eq!(interior_cuts_in_chars(&chars, 0), vec![4]);
    }

    #[test]
    fn multiple_runs() {
        // 你好 world 世界朋友 -> cuts inside each Han run only.
        let chars: Vec<char> = "你好 world 世界朋友".chars().collect();
        // "你好" is one word (no interior cut); "世界朋友" -> 世界 | 朋友.
        // offsets: 你0 好1 ' '2 w3 o4 r5 l6 d7 ' '8 世9 界10 朋11 友12 -> cut at 11
        assert_eq!(interior_cuts_in_chars(&chars, 0), vec![11]);
    }

    #[test]
    fn pure_ascii_has_no_cuts() {
        assert_eq!(cuts("hello world"), Vec::<usize>::new());
        assert!(!is_cjk('a'));
        assert!(is_cjk('中'));
    }
}
