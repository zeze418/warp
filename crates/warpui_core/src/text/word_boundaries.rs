use std::borrow::Cow;
use std::collections::HashSet;
use std::iter::Peekable;

use itertools::Either;
use string_offset::CharOffset;

use super::cjk;
use super::point::Point;
use super::words::is_default_word_boundary;
use super::TextBuffer;

/// This enum configures how the WordBoundaries iterator defines a "word"
#[derive(Clone, Debug)]
pub enum WordBoundariesPolicy {
    /// Break words on spaces and the characters specified in words::is_default_word_boundary
    Default,
    /// Break words on spaces plus a specific set of provided characters
    Custom(HashSet<char>),
    /// Break words only on ASCII whitespace
    OnlyWhitespace,
}

impl WordBoundariesPolicy {
    /// Returns whether `c` is a word-boundary (separator) character under this policy.
    pub fn is_word_boundary(&self, c: char) -> bool {
        match self {
            WordBoundariesPolicy::Default => is_default_word_boundary(c),
            WordBoundariesPolicy::Custom(boundary_chars) => {
                c.is_whitespace() || boundary_chars.contains(&c)
            }
            WordBoundariesPolicy::OnlyWhitespace => c.is_whitespace(),
        }
    }
}

#[derive(Clone, Copy)]
pub enum WordBoundariesApproach {
    ForwardWordStarts,
    ForwardWordEnds,
    BackwardWordStarts,
}

/// Iterator that returns the edges of words from a given offset, based on the selected approach
pub struct WordBoundaries<'a, T: TextBuffer + ?Sized> {
    offset: CharOffset,
    chars: Peekable<Either<T::Chars<'a>, T::CharsReverse<'a>>>,
    buffer: &'a T,
    in_word: bool,
    approach: WordBoundariesApproach,
    policy: Cow<'a, WordBoundariesPolicy>,
    done: bool,
    /// The offset the iterator was created at, before any stepping. Used to seed CJK segmentation.
    start_offset: CharOffset,
    /// Whether to merge in CJK (Chinese) dictionary-segmentation boundaries.
    cjk_enabled: bool,
    /// Whether a CJK cut exactly at `start_offset` should be yielded (matches the inclusive vs.
    /// exclusive semantics of the constructor this iterator came from).
    cjk_inclusive: bool,
    /// Whether [`Self::init_cjk`] has run yet (CJK cuts are computed lazily on first use).
    cjk_initialized: bool,
    /// Precomputed CJK boundary offsets, ordered in the direction of travel.
    cjk_cuts: Peekable<std::vec::IntoIter<CharOffset>>,
    /// One-item lookahead for the separator-based boundary stream, used while merging.
    pending_base: Option<Point>,
    /// Whether the separator-based stream has been exhausted.
    base_done: bool,
}

impl<'a, T: TextBuffer + ?Sized> WordBoundaries<'a, T> {
    pub fn with_policy(mut self, policy: impl Into<Cow<'a, WordBoundariesPolicy>>) -> Self {
        self.policy = policy.into();
        self
    }

    /// Whether CJK (Chinese) dictionary segmentation is merged into the boundary stream in
    /// addition to the separator-based boundaries. Enabled by default; see [`super::cjk`]. This is
    /// a no-op for text that contains no Han characters.
    pub fn with_cjk(mut self, enabled: bool) -> Self {
        self.cjk_enabled = enabled;
        self
    }

    /// Shared constructor that fills in the common defaults. `cjk_inclusive` mirrors whether the
    /// chosen approach includes a boundary sitting exactly on the starting offset.
    fn build(
        offset: CharOffset,
        chars: Peekable<Either<T::Chars<'a>, T::CharsReverse<'a>>>,
        buffer: &'a T,
        in_word: bool,
        approach: WordBoundariesApproach,
        cjk_inclusive: bool,
    ) -> Self {
        Self {
            offset,
            chars,
            buffer,
            in_word,
            approach,
            policy: Cow::Owned(WordBoundariesPolicy::Default),
            done: false,
            start_offset: offset,
            cjk_enabled: true,
            cjk_inclusive,
            cjk_initialized: false,
            cjk_cuts: Vec::new().into_iter().peekable(),
            pending_base: None,
            base_done: false,
        }
    }

    /// Create an iterator that will return the starts of words moving forwards
    pub fn forward_starts(offset: CharOffset, chars: T::Chars<'a>, buffer: &'a T) -> Self {
        Self::build(
            offset,
            Either::Left(chars).peekable(),
            buffer,
            true,
            WordBoundariesApproach::ForwardWordStarts,
            false,
        )
    }

    /// Create an iterator that will return the ends of words moving forwards, exclusive of the
    /// offset position.
    ///
    /// Example: For a buffer of "word one two three", with an offset of `4` (immediately after
    /// the 'word'), this will yield columns [8, 12, 18], the ends of `one`, `two`, and `three`,
    /// but _excluding_ the initial position at the end of `word`.
    pub fn forward_ends_exclusive(offset: CharOffset, chars: T::Chars<'a>, buffer: &'a T) -> Self {
        Self::build(
            offset,
            Either::Left(chars).peekable(),
            buffer,
            false,
            WordBoundariesApproach::ForwardWordEnds,
            false,
        )
    }

    /// Create an iterator that will return the ends of words moving forwards, inclusive of the
    /// offset position.
    ///
    /// Example: For a buffer of "word one two three", with an offset of `4` (immediately after
    /// the 'word'), this will yield columns [4, 8, 12, 18], the ends of all four words,
    /// _including_ the initial position at the end of `word`.
    pub fn forward_ends_inclusive(offset: CharOffset, chars: T::Chars<'a>, buffer: &'a T) -> Self {
        Self::build(
            offset,
            Either::Left(chars).peekable(),
            buffer,
            true,
            WordBoundariesApproach::ForwardWordEnds,
            true,
        )
    }

    /// Create an iterator that will return the starts of words moving _backwards_, exclusive of
    /// the offset position
    ///
    /// Example: For a buffer of "word one two three", with an offset of `13` (immediately before
    /// the 'three'), this will yield columns [9, 5, 0], the starts of `two`, `one`, and `word`,
    /// but _excluding_ the initial position at the start of `three`.
    pub fn backward_starts_exclusive(
        offset: CharOffset,
        chars: T::CharsReverse<'a>,
        buffer: &'a T,
    ) -> Self {
        Self::build(
            offset,
            Either::Right(chars).peekable(),
            buffer,
            false,
            WordBoundariesApproach::BackwardWordStarts,
            false,
        )
    }

    /// Create an iterator that will return the starts of words moving _backwards_, inclusive of
    /// the offset position
    ///
    /// Example: For a buffer of "word one two three", with an offset of `13` (immediately before
    /// the 'three'), this will yield columns [13, 9, 5, 0], the starts of all four words,
    /// _including_ the initial position at the start of `three`.
    pub fn backward_starts_inclusive(
        offset: CharOffset,
        chars: T::CharsReverse<'a>,
        buffer: &'a T,
    ) -> Self {
        Self::build(
            offset,
            Either::Right(chars).peekable(),
            buffer,
            true,
            WordBoundariesApproach::BackwardWordStarts,
            true,
        )
    }

    fn step(&mut self) {
        self.chars.next();
        match self.approach {
            WordBoundariesApproach::ForwardWordStarts | WordBoundariesApproach::ForwardWordEnds => {
                self.offset += 1;
            }
            WordBoundariesApproach::BackwardWordStarts => {
                self.offset -= 1;
            }
        }
    }

    fn is_word_boundary(&self, c: char) -> bool {
        self.policy.is_word_boundary(c)
    }

    /// Whether this iterator advances toward higher offsets.
    fn travel_forward(&self) -> bool {
        matches!(
            self.approach,
            WordBoundariesApproach::ForwardWordStarts | WordBoundariesApproach::ForwardWordEnds
        )
    }

    /// Lazily compute the CJK segmentation boundaries relevant to this traversal and store them in
    /// `cjk_cuts`, ordered in the direction of travel. This reads a bounded window of text around
    /// the start offset so that runs of Han characters can be segmented as whole words.
    fn init_cjk(&mut self) {
        self.cjk_initialized = true;

        // How far (in characters) to look on either side of the start offset for CJK runs. This
        // bounds the cost on very large buffers; text-input buffers are far shorter than this.
        const CJK_WINDOW: usize = 2048;

        let start = self.start_offset;

        // Read up to `CJK_WINDOW` characters before the start (in reverse), then restore reading
        // order so the window is a single left-to-right slice of characters. Reading from both
        // sides lets us segment a run that the start offset sits in the middle of.
        let mut window: Vec<char> = self
            .buffer
            .chars_rev_at(start)
            .map(|chars| chars.take(CJK_WINDOW).collect())
            .unwrap_or_default();
        let before_count = window.len();
        window.reverse();

        if let Ok(chars) = self.buffer.chars_at(start) {
            window.extend(chars.take(CJK_WINDOW));
        }

        let window_start = start.as_usize().saturating_sub(before_count);
        let start_usize = start.as_usize();

        let mut cuts: Vec<CharOffset> = cjk::interior_cuts_in_chars(&window, window_start)
            .into_iter()
            .map(CharOffset::from)
            .filter(|cut| {
                let offset = cut.as_usize();
                match (self.travel_forward(), self.cjk_inclusive) {
                    (true, true) => offset >= start_usize,
                    (true, false) => offset > start_usize,
                    (false, true) => offset <= start_usize,
                    (false, false) => offset < start_usize,
                }
            })
            .collect();

        if self.travel_forward() {
            cuts.sort_by_key(|cut| cut.as_usize());
        } else {
            cuts.sort_by_key(|cut| std::cmp::Reverse(cut.as_usize()));
        }

        self.cjk_cuts = cuts.into_iter().peekable();
    }

    /// The separator-based boundary stream: the original word-boundary logic, unaware of CJK.
    fn next_separator_boundary(&mut self) -> Option<Point> {
        while let Some(&c) = self.chars.peek() {
            match self.approach {
                // For forward word starts, we look for the transition from not in a word (i.e. in
                // a separator) to in a word. That boundary is the start of a new word
                WordBoundariesApproach::ForwardWordStarts => {
                    if self.in_word {
                        self.step();

                        if self.is_word_boundary(c) {
                            self.in_word = false;
                        }
                    } else if self.is_word_boundary(c) {
                        self.step();
                    } else {
                        // We are not in a word, but the next character _is_ in a word, so
                        // we've found the start of the next word. We mark ourselves as being
                        // in a word (for the next iteration), then return the point.
                        self.in_word = true;
                        return self.buffer.to_point(self.offset).ok();
                    }
                }
                // For forward word ends, we look for the transition from in a word to not in a
                // word. That boundary is the end of the current word. We also look for the same
                // boundary for backward starts, since going backwards the transition from in a
                // word to not in a word represents the _beginning_ of the current word
                WordBoundariesApproach::ForwardWordEnds
                | WordBoundariesApproach::BackwardWordStarts => {
                    if self.in_word {
                        if self.is_word_boundary(c) {
                            // We are in a word, but the next character is _not_ in a word, so we
                            // have found the boundary. We mark ourselves as not being in a word,
                            // then return the point.
                            self.in_word = false;
                            return self.buffer.to_point(self.offset).ok();
                        } else {
                            self.step();
                        }
                    } else {
                        self.step();

                        if !self.is_word_boundary(c) {
                            self.in_word = true;
                        }
                    }
                }
            }
        }

        // We have consumed all of the characters in the given direction. However, we should also
        // treat the end (or beginning if backward) of the buffer as a word boundary. We only want
        // to return that once, however, so we mark ourselves as done afterwards.
        if self.done {
            None
        } else {
            self.done = true;

            self.buffer.to_point(self.offset).ok()
        }
    }
}

impl<T: TextBuffer + ?Sized> Iterator for WordBoundaries<'_, T> {
    type Item = Point;

    fn next(&mut self) -> Option<Self::Item> {
        let cjk_active =
            self.cjk_enabled && !matches!(&*self.policy, WordBoundariesPolicy::OnlyWhitespace);
        if !cjk_active {
            return self.next_separator_boundary();
        }

        if !self.cjk_initialized {
            self.init_cjk();
        }

        // Merge the separator-based boundary stream with the precomputed CJK cut stream. Both are
        // monotonic in the direction of travel, so we emit whichever boundary comes next,
        // de-duplicating any position that appears in both.
        loop {
            if self.pending_base.is_none() && !self.base_done {
                match self.next_separator_boundary() {
                    Some(point) => self.pending_base = Some(point),
                    None => self.base_done = true,
                }
            }

            let base_offset = self
                .pending_base
                .and_then(|point| self.buffer.to_offset(point).ok())
                .map(|offset| offset.as_usize());
            // If we have a pending base boundary but can't resolve it for comparison, just emit it
            // rather than dropping it.
            if self.pending_base.is_some() && base_offset.is_none() {
                return self.pending_base.take();
            }
            let cjk_offset = self.cjk_cuts.peek().map(|cut| cut.as_usize());

            match (base_offset, cjk_offset) {
                (None, None) => return None,
                (Some(_), None) => return self.pending_base.take(),
                (None, Some(_)) => {
                    let cut = self.cjk_cuts.next()?;
                    if let Ok(point) = self.buffer.to_point(cut) {
                        return Some(point);
                    }
                }
                (Some(base), Some(cjk)) => {
                    if base == cjk {
                        // Same position from both streams; consume both, emit once.
                        self.cjk_cuts.next();
                        return self.pending_base.take();
                    }
                    let take_base = if self.travel_forward() {
                        base < cjk
                    } else {
                        base > cjk
                    };
                    if take_base {
                        return self.pending_base.take();
                    }
                    let cut = self.cjk_cuts.next()?;
                    if let Ok(point) = self.buffer.to_point(cut) {
                        return Some(point);
                    }
                }
            }
        }
    }
}

impl From<WordBoundariesPolicy> for Cow<'_, WordBoundariesPolicy> {
    fn from(policy: WordBoundariesPolicy) -> Self {
        Cow::Owned(policy)
    }
}

impl<'a> From<&'a WordBoundariesPolicy> for Cow<'a, WordBoundariesPolicy> {
    fn from(policy: &'a WordBoundariesPolicy) -> Self {
        Cow::Borrowed(policy)
    }
}

#[cfg(test)]
#[path = "word_boundaries_tests.rs"]
mod tests;
