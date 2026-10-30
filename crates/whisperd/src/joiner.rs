//! Spacing between transcript pieces typed one after another.
//!
//! Each pause in a session types its own piece. Typing them raw ran words
//! together ("Open VS codeHey Ubuntu"). The rules follow Myna's commit buffer
//! (`myna-desktop/src/controller.rs`, `CommitBuffer`): one space between
//! pieces unless whitespace is already there, none before closing
//! punctuation, none after an opening bracket or quote, none between CJK or
//! Thai characters (those scripts don't separate words with spaces).

/// Remembers the last character typed in this session.
#[derive(Debug, Default)]
pub struct Joiner {
    last: Option<char>,
}

impl Joiner {
    /// Text to type for `piece`, with a leading space if it needs one.
    /// Returns `None` when the piece is blank.
    pub fn next(&mut self, piece: &str) -> Option<String> {
        let piece = piece.trim();
        let first = piece.chars().next()?;
        let space = match self.last {
            None => false,
            Some(prev) => needs_space(prev, first),
        };
        self.last = piece.chars().last();
        Some(if space {
            format!(" {piece}")
        } else {
            piece.to_string()
        })
    }
}

/// A piece dictated into a terminal: Whisper writes sentences ("Git
/// status."), a shell wants commands ("git status"). Lowercase the first
/// letter unless the word looks like an acronym ("AWS"), and drop a final
/// period or comma.
pub fn for_terminal(piece: &str) -> String {
    let piece = piece.trim().trim_end_matches(['.', ',']);
    let mut chars = piece.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    if chars.clone().next().is_some_and(char::is_uppercase) {
        return piece.to_string();
    }
    first.to_lowercase().chain(chars).collect()
}

fn needs_space(prev: char, next: char) -> bool {
    if prev.is_whitespace() || next.is_whitespace() {
        return false;
    }
    if "([{\"'“‘¿¡".contains(prev) {
        return false;
    }
    if ".,;:!?)]}…\"'”’%".contains(next) {
        return false;
    }
    !(no_word_spaces(prev) && no_word_spaces(next))
}

/// CJK and Thai: words are not separated by spaces.
fn no_word_spaces(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x30FF   // CJK punctuation, Hiragana, Katakana
        | 0x3400..=0x4DBF // CJK Extension A
        | 0x4E00..=0x9FFF // CJK Unified
        | 0xF900..=0xFAFF // CJK Compatibility
        | 0xFF00..=0xFFEF // Full-width forms
        | 0x0E00..=0x0E7F // Thai
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn join(pieces: &[&str]) -> String {
        let mut j = Joiner::default();
        pieces.iter().filter_map(|p| j.next(p)).collect()
    }

    #[test]
    fn terminal_pieces_read_as_commands() {
        assert_eq!(for_terminal("Git status."), "git status");
        assert_eq!(for_terminal(" Ls -la,"), "ls -la");
        assert_eq!(for_terminal("AWS s3 ls."), "AWS s3 ls");
        assert_eq!(for_terminal("Why is it failing?"), "why is it failing?");
        assert_eq!(for_terminal("."), "");
    }

    #[test]
    fn pieces_are_separated_by_one_space() {
        assert_eq!(
            join(&["Open VS code.", "Hey Ubuntu"]),
            "Open VS code. Hey Ubuntu"
        );
    }

    #[test]
    fn whisper_leading_and_trailing_spaces_are_trimmed() {
        assert_eq!(join(&[" Hello ", " world"]), "Hello world");
    }

    #[test]
    fn no_space_before_closing_punctuation() {
        assert_eq!(join(&["Hello", ", world", "!"]), "Hello, world!");
    }

    #[test]
    fn no_space_after_an_opening_bracket() {
        assert_eq!(join(&["call it (", "later)"]), "call it (later)");
    }

    #[test]
    fn cjk_pieces_join_without_spaces() {
        assert_eq!(join(&["今日は", "いい天気"]), "今日はいい天気");
    }

    #[test]
    fn blank_pieces_type_nothing_and_keep_state() {
        assert_eq!(join(&["one", "   ", "", "two"]), "one two");
    }

    #[test]
    fn the_first_piece_has_no_leading_space() {
        assert_eq!(join(&["first"]), "first");
    }
}
