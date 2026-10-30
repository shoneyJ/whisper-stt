VOICE COMMANDS (whisper-stt)                 q = close   / = search
=================================================================

WAKE WORD
  "Jarvis, <command>"        first word of what you say
  "Hey Jarvis, <command>"    same; a misheard command shows a notice
  "Jarvis" ... <command>     pause after the name, then the command
                             (within 6 s; the pill ripples and a hint card
                             on the right lists commands for the focused app)
  "Hey niri, <command>"      old wake word, still works

  Without "hey", anything not understood is ignored, so
  "Jarvis was great" said aloud does nothing.
  Listening stops while you dictate. Off: WHISPER_STT_WAKE=0.


DICTATION
  Super+J                    start / stop dictation
  "Jarvis, listen"           start dictation   (also: dictate)

  While dictating, say alone, after a short pause:
  "stop listening"           stop dictation    (also: stop dictation)
  "enter"                    Enter, in any app (also: run it, send it, execute)

  Dictated into a terminal, text is typed as a command:
  "Git status." is typed as  git status


TERMINAL KEYS     terminal focused; while dictating, or after "Jarvis"
  "enter"                    Enter (also: run it, send it, execute)
  "cancel"                   Ctrl+C            (also: control C)
  "clear"                    Ctrl+L            (also: clear screen)
  "run again"                Up, Enter         (also: again, repeat)
  "tab"                      Tab
  "escape"                   Esc
  "delete word"              Ctrl+W
  "clear line"               Ctrl+U


TMUX              acts on the tmux client you used last
  "Jarvis, session main"     switch session    (also: switch to main, main)
  "Jarvis, next session"     next session
  "Jarvis, previous session" previous session  (also: go back, terminal only)
  "Jarvis, last session"     last-used session
  "Jarvis, new window"       new window, same directory
  "Jarvis, window 2"         go to window 2    (also: window two)
  "Jarvis, next window"      next window       (also: previous window)
  "Jarvis, split right"      side-by-side pane (also: split vertically)
  "Jarvis, split down"       stacked pane      (also: split horizontally)
  "Jarvis, pane left"        move to pane      (also: right / up / down)
  "Jarvis, zoom pane"        zoom / unzoom pane
  "Jarvis, scroll up"        copy mode, one page up


DESKTOP (niri)
  "Jarvis, terminal"         focus the last-used terminal
  "Jarvis, browser"          focus Zen         (also: go to browser, zen)
  "Jarvis, workspace 3"      go to workspace   (also: workspace web / chat / code)
  "Jarvis, maximize"         full-width column


ZEN BROWSER       Zen focused
  "Jarvis, new tab"          Ctrl+T
  "Jarvis, close tab"        Ctrl+W            (Ctrl+Shift+T reopens)
  "Jarvis, next tab"         Ctrl+Tab          (also: previous tab)
  "Jarvis, tab 3"            Alt+3             (tab 9 = last tab)
  "Jarvis, go back"          Alt+Left          (also: forward)
  "Jarvis, reload"           Ctrl+R            (also: refresh)
  "Jarvis, search <words>"   address bar, type, Enter
  "Jarvis, find <words>"     find in page


NOT AVAILABLE ON PURPOSE
  Closing tmux sessions, windows or panes: a misheard word can't be
  undone.


TIPS
  - Short single words get misheard. Two-word forms are safer:
    "zoom pane", "run it", "send it".
  - Search words come from the small wake model and can be garbled
    ("tokio" came out as "Tokyo"). For long queries, say
    "Jarvis, new tab", then dictate the query with Super+J.
  - Voice commands are silent. The result shows visually: green check
    (bounces) or red cross (shakes) on the pill, and the card shows what
    happened ("tmux: main:2", "No session like ...") for ~2 s.
    Dictation is silent too (WHISPER_STT_SOUNDS=0 in the service): the
    pill pulses while starting, turns blue with a level bar while
    listening, and pulses while transcribing.
  - Try a command without speaking:
      whisperctl run split right
  - Check how well it hears YOUR voice:
      just record-commands     (one 3 s clip per phrase, kept local)
      just eval-commands       (score; `just eval-commands tiny.en` to compare)
  - Full design and test evidence: doc/plan/07-voice-commands.md
