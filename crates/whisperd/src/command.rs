//! Voice commands: turn what Whisper heard into one allowlisted action
//! (tmux, a terminal key, a Zen browser shortcut, starting dictation).
//!
//! The wake word is **Jarvis**: as the first word ("Jarvis, next session")
//! or after "hey". Picked by test: tiny.en heard "Jarvis" right in every
//! synthetic voice, with or without "hey", where "niri" came back as "Hal
//! needy", "Hey, needy", "Neary", "near you". "Hey niri" still works.
//! Without "hey", a command that isn't understood is ignored silently, so
//! "Jarvis was great" said aloud does nothing.
//!
//! Whisper mishears short words, so nothing here matches exactly:
//!
//! - the wake word is any of a few known mishearings, or within one edit of
//!   one, and spelled-out letters ("N I R I") are joined first
//! - the session name is matched against the *live* session list, the
//!   closed vocabulary that makes fuzzy matching safe
//!
//! The result is an [`Intent`] from a fixed list. Text never reaches a
//! shell, and there is no destructive intent (no kill).

use crate::focus::App;

/// What a voice command may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Tmux(Tmux),
    /// Press a key in the focused window.
    Key(Key),
    /// A Zen shortcut; only parsed while the browser is focused.
    Browser(Browser),
    /// Desktop: focus an app, a workspace, maximize.
    Niri(Niri),
    /// Same as Super+J.
    StartDictation,
}

/// Keys a voice command may press. All but `Enter` act only while a
/// terminal is focused: "cancel" (Ctrl+C) would copy in a browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Enter,
    /// Ctrl+C
    Cancel,
    /// Ctrl+L
    Clear,
    /// Up, Enter: run the last command again.
    Again,
    Tab,
    Escape,
    /// Ctrl+W
    DeleteWord,
    /// Ctrl+U
    ClearLine,
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Key::Enter => "Enter",
            Key::Cancel => "Ctrl+C",
            Key::Clear => "Ctrl+L",
            Key::Again => "run again",
            Key::Tab => "Tab",
            Key::Escape => "Esc",
            Key::DeleteWord => "Ctrl+W",
            Key::ClearLine => "Ctrl+U",
        })
    }
}

impl std::fmt::Display for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Browser::NewTab => f.write_str("new tab"),
            Browser::CloseTab => f.write_str("close tab"),
            Browser::NextTab => f.write_str("next tab"),
            Browser::PreviousTab => f.write_str("previous tab"),
            Browser::Tab(n) => write!(f, "tab {n}"),
            Browser::Back => f.write_str("back"),
            Browser::Forward => f.write_str("forward"),
            Browser::Reload => f.write_str("reload"),
            Browser::Search(q) => write!(f, "search \u{201c}{q}\u{201d}"),
            Browser::Find(q) => write!(f, "find \u{201c}{q}\u{201d}"),
        }
    }
}

impl Key {
    pub fn terminal_only(self) -> bool {
        self != Key::Enter
    }
}

/// The whole utterance names a key ("enter", "press enter", "clear line").
fn key(words: &[&str]) -> Option<Key> {
    Some(match words {
        // "send"/"submit"/"go"/"return" alone were misheard in tests;
        // these weren't.
        ["enter" | "execute"] | ["press" | "hit", "enter"] | ["run" | "send", "it"] => Key::Enter,
        ["cancel"] | ["control" | "ctrl", "c"] => Key::Cancel,
        ["clear"] | ["clear", "screen"] => Key::Clear,
        ["again"] | ["run", "again"] | ["repeat"] => Key::Again,
        ["tab"] => Key::Tab,
        ["escape"] => Key::Escape,
        ["delete", "word"] => Key::DeleteWord,
        ["clear", "line"] => Key::ClearLine,
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tmux {
    Switch(String),
    Next,
    Previous,
    Last,
    NewWindow,
    Window(u32),
    NextWindow,
    PreviousWindow,
    Split(Split),
    Pane(Dir),
    Zoom,
    ScrollUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Split {
    /// Side by side (`split-window -h`).
    Right,
    /// Stacked (`split-window -v`).
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Niri {
    /// The most recently used window of that kind.
    Focus(App),
    /// Index or name ("3", "web").
    Workspace(String),
    Maximize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Browser {
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    Tab(u32),
    Back,
    Forward,
    Reload,
    Search(String),
    Find(String),
}

/// Said alone as one dictated piece, these act instead of being typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spoken {
    Key(Key),
    StopDictation,
}

/// A dictated piece that is only a key name ("enter", "clear line") or
/// "stop listening". A sentence that merely contains the word is typed.
pub fn spoken_key(piece: &str) -> Option<Spoken> {
    let t = tokens(piece);
    let t: Vec<&str> = t.iter().map(String::as_str).collect();
    match t.as_slice() {
        ["stop" | "end", "dictation" | "dictating" | "listening"] => Some(Spoken::StopDictation),
        words => key(words).map(Spoken::Key),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// No wake phrase where one was required: ordinary speech, ignore.
    NotWake,
    /// The wake phrase and nothing else: listen for the command next.
    WakeOnly,
    Command(Intent),
    /// Addressed to us, but not understood. The text says why.
    Unknown(String),
}

/// Words heard in place of "hey".
/// Not "a"/"he": "a nearly…", "he needs…" would wake us.
const HEY: &[&str] = &["hey", "hay", "hi", "hei", "hal", "ok", "okay", "eh"];

/// The wake word; wakes as the first word, with or without "hey". Within
/// one edit also counts.
const JARVIS: &[&str] = &["jarvis", "jervis", "jarvas"];

/// The old wake word, kept as a fallback; needs "hey". Within one edit also
/// counts ("nire", "niry"). Not "nearly": too common after "hey".
const NIRI: &[&str] = &["niri", "neri", "nira", "neary", "nearie", "needy", "nieri"];

/// Filler before or around the command that carries no meaning.
const FILLER: &[&str] = &["please", "tmux", "demux", "the", "now"];

/// A session matches when its similarity is at least this, and clearly
/// better than the runner-up.
const MIN_SIMILARITY: f32 = 0.6;
const MIN_MARGIN: f32 = 0.15;

/// Parse a transcript. With `require_wake`, text that doesn't start with
/// the wake word is [`Parsed::NotWake`]; without it (after a lone wake
/// word, or `whisperctl run`) the wake word is optional. `app` is the
/// focused app: browser shortcuts only parse while the browser is focused.
pub fn parse(text: &str, sessions: &[String], require_wake: bool, app: App) -> Parsed {
    let tokens = tokens(text);
    let (rest, quiet) = match strip_wake(&tokens) {
        Some((rest, with_hey)) => (rest, !with_hey),
        None if require_wake => return Parsed::NotWake,
        None => (&tokens[..], false),
    };
    let raw: Vec<&str> = rest.iter().map(String::as_str).collect();
    let words: Vec<&str> = raw
        .iter()
        .copied()
        .filter(|t| !FILLER.contains(t))
        .collect();
    if words.is_empty() {
        return Parsed::WakeOnly;
    }
    match command(&raw, &words, sessions, app) {
        // "Jarvis was great": not addressed to us.
        Parsed::Unknown(_) if quiet => Parsed::NotWake,
        parsed => parsed,
    }
}

fn command(raw: &[&str], rest: &[&str], sessions: &[String], app: App) -> Parsed {
    let has = |words: &[&str]| rest.iter().any(|t| words.contains(t));
    if let Some(k) = key(rest) {
        return Parsed::Command(Intent::Key(k));
    }
    if has(&["dictation", "dictate", "dictating", "listen", "listening"]) {
        return if has(&["stop", "end"]) {
            Parsed::Unknown("Not dictating".into())
        } else {
            Parsed::Command(Intent::StartDictation)
        };
    }
    if app == App::Browser {
        if let Some(b) = browser(raw, rest) {
            return Parsed::Command(Intent::Browser(b));
        }
    }
    if let Some(parsed) = desktop(rest) {
        return parsed;
    }
    if let Some(parsed) = tmux_layout(rest) {
        return parsed;
    }
    if has(&["next"]) {
        return Parsed::Command(Intent::Tmux(Tmux::Next));
    }
    if has(&["previous", "prev", "back"]) {
        return Parsed::Command(Intent::Tmux(Tmux::Previous));
    }
    if has(&["last"]) {
        return Parsed::Command(Intent::Tmux(Tmux::Last));
    }

    // "switch to session main", "go to main", "session dot files", "main"
    let target = match rest.iter().rposition(|t| *t == "session" || *t == "to") {
        Some(i) => &rest[i + 1..],
        None if ["switch", "go", "attach", "jump", "open"].contains(&rest[0]) => &rest[1..],
        None => rest,
    };
    let target: Vec<&str> = target
        .iter()
        .copied()
        .filter(|t| *t != "session" && *t != "to")
        .collect();
    if target.is_empty() {
        return Parsed::Unknown("Which session?".into());
    }
    match best_session(&target.concat(), sessions) {
        Some(name) => Parsed::Command(Intent::Tmux(Tmux::Switch(name))),
        None => Parsed::Unknown(format!("No session like {:?}", target.join(" "))),
    }
}

/// niri: "terminal", "go to browser", "workspace 3", "maximize".
fn desktop(rest: &[&str]) -> Option<Parsed> {
    let has = |words: &[&str]| rest.iter().any(|t| words.contains(t));
    let niri = |n| Some(Parsed::Command(Intent::Niri(n)));
    if let Some(i) = rest
        .iter()
        .position(|t| ["workspace", "workspaces", "desktop"].contains(t))
    {
        return match number(rest) {
            Some(n) => niri(Niri::Workspace(n.to_string())),
            None => match rest.get(i + 1) {
                Some(name) => niri(Niri::Workspace(name.to_string())),
                None => Some(Parsed::Unknown("Which workspace?".into())),
            },
        };
    }
    if has(&["maximize", "maximise", "maximized", "maximised"]) {
        return niri(Niri::Maximize);
    }
    // Only "terminal" alone or after a verb, so "session terminal" is a session.
    let target = match rest {
        [app] | ["go" | "switch" | "focus" | "open", app] => *app,
        ["go" | "switch", "to", app] => *app,
        _ => return None,
    };
    match target {
        "terminal" => niri(Niri::Focus(App::Terminal)),
        "browser" | "zen" | "firefox" => niri(Niri::Focus(App::Browser)),
        _ => None,
    }
}

/// tmux windows and panes: "new window", "window 2", "split right",
/// "pane left", "zoom", "scroll up".
fn tmux_layout(rest: &[&str]) -> Option<Parsed> {
    let has = |words: &[&str]| rest.iter().any(|t| words.contains(t));
    let tmux = |t| Some(Parsed::Command(Intent::Tmux(t)));
    if has(&["window", "windows"]) {
        return if has(&["new", "create", "open"]) {
            tmux(Tmux::NewWindow)
        } else if has(&["next"]) {
            tmux(Tmux::NextWindow)
        } else if has(&["previous", "prev", "back"]) {
            tmux(Tmux::PreviousWindow)
        } else if let Some(n) = number(rest) {
            tmux(Tmux::Window(n))
        } else {
            Some(Parsed::Unknown("Which window?".into()))
        };
    }
    if has(&["split"]) {
        return match dir(rest) {
            _ if has(&["vertical", "vertically"]) => tmux(Tmux::Split(Split::Right)),
            _ if has(&["horizontal", "horizontally"]) => tmux(Tmux::Split(Split::Down)),
            Some(Dir::Right | Dir::Left) => tmux(Tmux::Split(Split::Right)),
            Some(Dir::Down | Dir::Up) => tmux(Tmux::Split(Split::Down)),
            None => Some(Parsed::Unknown("Split right or down?".into())),
        };
    }
    // Before "pane": "zoom pane" is a zoom.
    if has(&["zoom", "unzoom"]) {
        return tmux(Tmux::Zoom);
    }
    if has(&["pane", "panes"]) {
        return match dir(rest) {
            Some(d) => tmux(Tmux::Pane(d)),
            None => Some(Parsed::Unknown("Which pane?".into())),
        };
    }
    if has(&["scroll"]) {
        return tmux(Tmux::ScrollUp);
    }
    None
}

/// Zen shortcuts. `raw` keeps filler words, for search text.
fn browser(raw: &[&str], rest: &[&str]) -> Option<Browser> {
    let has = |words: &[&str]| rest.iter().any(|t| words.contains(t));
    let after = |words: &[&str]| -> Option<String> {
        let i = raw.iter().position(|t| words.contains(t))?;
        let mut q = &raw[i + 1..];
        if q.first() == Some(&"for") {
            q = &q[1..];
        }
        (!q.is_empty()).then(|| q.join(" "))
    };
    if let Some(q) = after(&["search", "google"]) {
        return Some(Browser::Search(q));
    }
    if let Some(q) = after(&["find"]) {
        return Some(Browser::Find(q));
    }
    if has(&["tab", "tabs"]) {
        return Some(if has(&["new", "open"]) {
            Browser::NewTab
        } else if has(&["close"]) {
            Browser::CloseTab
        } else if has(&["next"]) {
            Browser::NextTab
        } else if has(&["previous", "prev", "back"]) {
            Browser::PreviousTab
        } else {
            Browser::Tab(number(rest)?)
        });
    }
    match rest {
        ["back"] | ["go", "back"] => Some(Browser::Back),
        ["forward"] | ["go", "forward"] => Some(Browser::Forward),
        ["reload" | "refresh"] | ["reload" | "refresh", "page"] => Some(Browser::Reload),
        _ => None,
    }
}

fn number(rest: &[&str]) -> Option<u32> {
    const WORDS: &[&str] = &[
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ];
    rest.iter().find_map(|t| {
        t.parse()
            .ok()
            .or_else(|| WORDS.iter().position(|w| w == t).map(|i| i as u32))
    })
}

fn dir(rest: &[&str]) -> Option<Dir> {
    rest.iter().find_map(|t| match *t {
        "left" => Some(Dir::Left),
        "right" => Some(Dir::Right),
        "up" | "above" => Some(Dir::Up),
        "down" | "below" => Some(Dir::Down),
        _ => None,
    })
}

/// Lowercase words, punctuation dropped, runs of single letters joined
/// ("N-I-R-I" -> "niri").
fn tokens(text: &str) -> Vec<String> {
    let cleaned: String = text
        .chars()
        .map(|c| match c {
            c if c.is_alphanumeric() => c.to_ascii_lowercase(),
            // These end a spelled run; "-" and "." don't ("N-I-R. I.").
            ',' | ';' | '!' | '?' => '|',
            _ => ' ',
        })
        .collect();
    let mut out: Vec<String> = Vec::new();
    let mut letters = String::new();
    for word in cleaned.replace('|', " | ").split_whitespace() {
        // "a" alone is a word unless it continues a run ("N-I-R-A").
        if word.chars().count() == 1 && word != "|" && (word != "a" || !letters.is_empty()) {
            letters.push_str(word);
            continue;
        }
        if !letters.is_empty() {
            out.push(std::mem::take(&mut letters));
        }
        if word != "|" {
            out.push(word.to_string());
        }
    }
    if !letters.is_empty() {
        out.push(letters);
    }
    out
}

/// The tokens after the wake word, and whether "hey" came first.
/// "Jarvis" wakes as the first word; "niri" only after "hey". The name may
/// be split in two ("ni re").
fn strip_wake(tokens: &[String]) -> Option<(&[String], bool)> {
    let first = tokens.first()?;
    if is_name(first, JARVIS) {
        return Some((&tokens[1..], false));
    }
    if !HEY.contains(&first.as_str()) {
        return None;
    }
    for n in 1..=2 {
        let Some(name) = tokens.get(1..=n) else {
            break;
        };
        let name = name.concat();
        if is_name(&name, JARVIS) || is_name(&name, NIRI) {
            return Some((&tokens[n + 1..], true));
        }
    }
    None
}

fn is_name(word: &str, names: &[&str]) -> bool {
    names.iter().any(|n| levenshtein(word, n) <= 1)
}

fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The session `spoken` most likely names, if one stands out.
fn best_session(spoken: &str, sessions: &[String]) -> Option<String> {
    let mut scored: Vec<(f32, &String)> = sessions
        .iter()
        .map(|s| (similarity(spoken, &normalize(s)), s))
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (best, name) = *scored.first()?;
    let runner_up = scored.get(1).map_or(0.0, |s| s.0);
    (best >= MIN_SIMILARITY && best - runner_up >= MIN_MARGIN).then(|| name.clone())
}

/// 1.0 for equal strings, 0.0 for nothing in common.
fn similarity(a: &str, b: &str) -> f32 {
    let longest = a.chars().count().max(b.chars().count());
    if longest == 0 {
        return 1.0;
    }
    1.0 - levenshtein(a, b) as f32 / longest as f32
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sessions() -> Vec<String> {
        ["done", "dotfiles", "main"].map(String::from).to_vec()
    }

    fn wake(text: &str) -> Parsed {
        parse(text, &sessions(), true, App::Terminal)
    }

    fn switch(name: &str) -> Parsed {
        Parsed::Command(Intent::Tmux(Tmux::Switch(name.into())))
    }

    #[test]
    fn real_mishearings_of_the_wake_phrase_wake_us() {
        for heard in [
            "Hey niri.",
            "Hey, needy.",
            "Hal needy",
            "Hey. Ni, re.",
            "Hey. N-I-R-A",
            "Hey. N I R. I.",
            "Hey Neary!",
        ] {
            assert_eq!(wake(heard), Parsed::WakeOnly, "{heard:?}");
        }
    }

    #[test]
    fn ordinary_speech_is_ignored() {
        for heard in [
            "Switch to session main.",
            "I'm a bit needy today.",
            "A nearly done task.",
            "He needs a session.",
            "Hey, T-Mex. Switch to session main.",
            "Hey, how are you?",
            "Hey Nick, switch to main.",
            "",
        ] {
            assert_eq!(wake(heard), Parsed::NotWake, "{heard:?}");
        }
    }

    #[test]
    fn switch_commands_in_one_breath() {
        assert_eq!(wake("Hey niri, switch to session main."), switch("main"));
        assert_eq!(wake("Hey needy, go to dot files."), switch("dotfiles"));
        assert_eq!(
            wake("Hey niri. Switch to session. Dotfile."),
            switch("dotfiles")
        );
        assert_eq!(wake("Hey niri, main"), switch("main"));
        assert_eq!(wake("Hey niri, session Maine please"), switch("main"));
        assert_eq!(wake("hey niri switch to done"), switch("done"));
    }

    #[test]
    fn relative_session_commands() {
        assert_eq!(
            wake("Hey niri, next session."),
            Parsed::Command(Intent::Tmux(Tmux::Next))
        );
        assert_eq!(
            wake("Hey niri, go back."),
            Parsed::Command(Intent::Tmux(Tmux::Previous))
        );
        assert_eq!(
            wake("Hey niri, last session"),
            Parsed::Command(Intent::Tmux(Tmux::Last))
        );
    }

    #[test]
    fn unknown_or_ambiguous_names_are_refused_not_guessed() {
        // "dotfiles" as Whisper once heard it: no live session is close.
        assert!(matches!(
            wake("Hey needy. Switch to session. Indeed."),
            Parsed::Unknown(m) if m.contains("indeed")
        ));
        let close = ["main1".to_string(), "main2".to_string()];
        assert!(matches!(
            parse("switch to main", &close, false, App::Terminal),
            Parsed::Unknown(_)
        ));
        assert!(matches!(wake("Hey niri, make coffee."), Parsed::Unknown(_)));
    }

    #[test]
    fn after_a_lone_wake_phrase_the_command_needs_no_wake() {
        assert_eq!(
            parse("Switch to session main.", &sessions(), false, App::Terminal),
            switch("main")
        );
    }

    #[test]
    fn key_and_dictation_commands() {
        let key = |k| Parsed::Command(Intent::Key(k));
        assert_eq!(wake("Hey niri, enter."), key(Key::Enter));
        assert_eq!(wake("Hey niri. Press Enter!"), key(Key::Enter));
        assert_eq!(wake("Hey niri, clear line."), key(Key::ClearLine));
        assert_eq!(wake("Hey niri, control C"), key(Key::Cancel));
        let start = Parsed::Command(Intent::StartDictation);
        assert_eq!(wake("Hey niri, start dictation."), start);
        assert_eq!(wake("Hey needy, dictate"), start);
        assert_eq!(wake("Hey niri, listen."), start);
        assert!(matches!(
            wake("Hey niri, stop dictation"),
            Parsed::Unknown(_)
        ));
        assert!(matches!(
            wake("Hey niri, stop listening"),
            Parsed::Unknown(_)
        ));
    }

    #[test]
    fn spoken_keys_only_when_said_alone() {
        let key = |k| Some(Spoken::Key(k));
        assert_eq!(spoken_key("Enter."), key(Key::Enter));
        assert_eq!(spoken_key(" Press enter!"), key(Key::Enter));
        assert_eq!(spoken_key("Cancel."), key(Key::Cancel));
        assert_eq!(spoken_key("Clear."), key(Key::Clear));
        assert_eq!(spoken_key("Run again."), key(Key::Again));
        assert_eq!(spoken_key("Tab"), key(Key::Tab));
        assert_eq!(spoken_key("Escape."), key(Key::Escape));
        assert_eq!(spoken_key("Delete word."), key(Key::DeleteWord));
        assert_eq!(spoken_key("Clear line."), key(Key::ClearLine));
        assert_eq!(spoken_key("Stop dictation."), Some(Spoken::StopDictation));
        assert_eq!(spoken_key("Stop listening."), Some(Spoken::StopDictation));
        assert_eq!(spoken_key("Cancel the meeting."), None);
        assert_eq!(spoken_key("Please enter your name."), None);
        assert_eq!(spoken_key("git status"), None);
    }

    #[test]
    fn jarvis_wakes_with_or_without_hey() {
        assert_eq!(
            wake("Jarvis, next session."),
            Parsed::Command(Intent::Tmux(Tmux::Next))
        );
        assert_eq!(wake("Hey Jarvis."), Parsed::WakeOnly);
        assert_eq!(wake("Jarvis."), Parsed::WakeOnly);
        assert_eq!(wake("Jarvis, main"), switch("main"));
        // Without "hey", what we don't understand isn't for us.
        assert_eq!(wake("Jarvis was great."), Parsed::NotWake);
        assert!(matches!(
            wake("Hey Jarvis, make coffee."),
            Parsed::Unknown(_)
        ));
        // Old wake word still works, but only after "hey".
        assert_eq!(
            wake("Hey niri, next session"),
            Parsed::Command(Intent::Tmux(Tmux::Next))
        );
        assert_eq!(wake("Niri, next session"), Parsed::NotWake);
    }

    #[test]
    fn enter_synonyms() {
        for heard in ["Run it.", "Send it.", "Execute.", "Enter."] {
            assert_eq!(
                spoken_key(heard),
                Some(Spoken::Key(Key::Enter)),
                "{heard:?}"
            );
        }
    }

    #[test]
    fn tmux_windows_and_panes() {
        let t = |t| Parsed::Command(Intent::Tmux(t));
        assert_eq!(wake("Jarvis, new window."), t(Tmux::NewWindow));
        assert_eq!(wake("Jarvis, window two."), t(Tmux::Window(2)));
        assert_eq!(wake("Jarvis, go to window 3"), t(Tmux::Window(3)));
        assert_eq!(wake("Jarvis, next window."), t(Tmux::NextWindow));
        assert_eq!(wake("Jarvis, previous window"), t(Tmux::PreviousWindow));
        assert_eq!(wake("Jarvis, split right."), t(Tmux::Split(Split::Right)));
        assert_eq!(wake("Jarvis, split down"), t(Tmux::Split(Split::Down)));
        assert_eq!(
            wake("Jarvis, split vertically"),
            t(Tmux::Split(Split::Right))
        );
        assert_eq!(wake("Jarvis, pane left."), t(Tmux::Pane(Dir::Left)));
        assert_eq!(wake("Jarvis, pane up"), t(Tmux::Pane(Dir::Up)));
        assert_eq!(wake("Jarvis, zoom."), t(Tmux::Zoom));
        assert_eq!(wake("Jarvis, zoom pane"), t(Tmux::Zoom));
        assert_eq!(wake("Jarvis, scroll up."), t(Tmux::ScrollUp));
        assert!(matches!(wake("Hey Jarvis, window."), Parsed::Unknown(_)));
        assert!(matches!(wake("Hey Jarvis, split."), Parsed::Unknown(_)));
    }

    #[test]
    fn browser_commands_only_when_the_browser_is_focused() {
        let b = |text| parse(text, &sessions(), true, App::Browser);
        let cmd = |b| Parsed::Command(Intent::Browser(b));
        assert_eq!(b("Jarvis, new tab."), cmd(Browser::NewTab));
        assert_eq!(b("Jarvis, close tab"), cmd(Browser::CloseTab));
        assert_eq!(b("Jarvis, next tab."), cmd(Browser::NextTab));
        assert_eq!(b("Jarvis, previous tab"), cmd(Browser::PreviousTab));
        assert_eq!(b("Jarvis, tab three."), cmd(Browser::Tab(3)));
        assert_eq!(b("Jarvis, go back."), cmd(Browser::Back));
        assert_eq!(b("Jarvis, forward"), cmd(Browser::Forward));
        assert_eq!(b("Jarvis, reload."), cmd(Browser::Reload));
        assert_eq!(
            b("Jarvis, search for the rust tokio docs."),
            cmd(Browser::Search("the rust tokio docs".into()))
        );
        assert_eq!(b("Jarvis, find error"), cmd(Browser::Find("error".into())));
        // tmux commands still work from the browser.
        assert_eq!(b("Jarvis, session main"), switch("main"));
        // In the terminal, "go back" is the previous tmux session.
        assert_eq!(
            wake("Jarvis, go back."),
            Parsed::Command(Intent::Tmux(Tmux::Previous))
        );
        assert_eq!(wake("Jarvis, new tab."), Parsed::NotWake);
    }

    #[test]
    fn desktop_commands() {
        let n = |n| Parsed::Command(Intent::Niri(n));
        assert_eq!(wake("Jarvis, terminal."), n(Niri::Focus(App::Terminal)));
        assert_eq!(wake("Jarvis, go to browser"), n(Niri::Focus(App::Browser)));
        assert_eq!(wake("Jarvis, switch to Zen."), n(Niri::Focus(App::Browser)));
        assert_eq!(
            wake("Jarvis, workspace three."),
            n(Niri::Workspace("3".into()))
        );
        assert_eq!(
            wake("Jarvis, workspace web"),
            n(Niri::Workspace("web".into()))
        );
        assert_eq!(wake("Jarvis, maximize."), n(Niri::Maximize));
        assert_eq!(wake("Jarvis, maximized."), n(Niri::Maximize));
        assert!(matches!(wake("Hey Jarvis, workspace"), Parsed::Unknown(_)));
        // A session that happens to be named like an app still switches.
        let s = ["terminal".to_string()];
        assert_eq!(
            parse("session terminal", &s, false, App::Terminal),
            Parsed::Command(Intent::Tmux(Tmux::Switch("terminal".into())))
        );
    }

    #[test]
    fn spelled_letters_are_joined() {
        assert_eq!(tokens("Hey. N-I-R-I, a b"), ["hey", "niri", "a", "b"]);
    }
}
