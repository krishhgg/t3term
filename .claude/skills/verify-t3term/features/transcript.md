# Transcript

## Sub-features

- **Emoji that end in U+FE0F.** An emoji followed by U+FE0F (VS16), such as ⚠️ or ❤️, takes two columns, and the text after it stays in its column whether the terminal draws the emoji one column wide or two. A row that scrolls or redraws over other text keeps its spacing: `⚠️ one`, never `⚠️  one` or `❤️two`. t3term draws every row the same way, so this holds in the sidebar and the breadcrumb too, but only the transcript is driven here.
- **Code fences.** A line that starts with three or more backticks or tildes, after any spaces or tabs, opens a code panel. Its first row is a label with the first word of the fence's info string, so ```` ```rust ignore ```` labels the panel `rust`, and each code row starts a column in, on the panel's background. Only a line of the same mark, at least as long, with nothing after it but spaces or tabs closes the panel. So a four-backtick fence can hold a three-backtick example, and a run with text after it is a row of code. Other whitespace counts as text, as CommonMark has it: a run followed by U+00A0 is a row of code, and the rows after it stay in the panel. Backticks with another backtick later in the line are inline code and open no panel. A fence never closed runs to the end of the reply.

## How to get to it (user POV)

Open a thread from the sidebar with a click, or with ↑ or ↓ and Enter. Its messages draw in the transcript, the newest at the bottom. PgUp and PgDn scroll it from anywhere, and so does the wheel over it. With the transcript focused (Esc from the composer), ↑ and ↓ scroll it, and g and G go to the top and the bottom.

## Driving it with t3v

Scenario `links` at 200x50. Each block opens the thread whose last reply holds its text, as the scenario's docstring says.

```bash
$T3V start

# Emoji that end in U+FE0F: thr-none's last reply, scrolled a row at 200x30.
$T3V click --text "None: no field" && $T3V wait --text "/  None: no field" --text "every row of this list has text"
$T3V resize 200 30 && $T3V wait --text "• ⚠️ one ❤️ two ✔️ three" --idle 300
$T3V keys Escape && $T3V wait --text "↑↓/PgUp scroll"
$T3V capture emoji-before
$T3V keys Up && $T3V wait --text "↓ 1 more" --idle 300          # each emoji row now sits where a row of text was
$T3V wait --text "• ⚠️ one ❤️ two ✔️ three" --text "• ❤️ 2  🎉 3  ⚠️ 1  reactions"   # master draws "⚠️  one ❤️two"
$T3V capture emoji-scrolled
$T3V resize 200 50 && $T3V wait --text "every row of this list has text" --idle 300

# Code fences: thr-legacy's last reply.
$T3V click --text "Legacy: one old link" && $T3V wait --text "/  Legacy: one old link" --text "End of the fence check."
$T3V wait --text '```sh' --text 't3term threads --json'      # a four-backtick fence holds a three-backtick one
$T3V wait --gone '~~~' --text 'once = true'                  # a tilde fence is a panel labelled toml
$T3V wait --gone 'rust ignore' --text '```` not a close'     # the label is the info's first word; a run with text after it is code
$T3V wait --text 'is inline code, not a fence.'              # backticks with a backtick later in the line open no panel
$T3V wait --text 'AFTER_NBSP_MUST_STAY_CODE' --text '- still in the panel'   # a run followed by U+00A0 closes nothing
$T3V capture fences
```

## Gotchas

- t3v's tmux draws an emoji with U+FE0F two columns wide, tmux 3.6a's default (`variation-selector-always-wide on`). A build without the fix misdraws such a row only where the frame before held text in the column the emoji hides, so the recipe scrolls a row rather than just opening the thread, which draws right on any build. The one-column case can't be driven, because t3v's tmux has no setting for it. The tests in src/tui/wide_emoji.rs cover it.
- The recipe's emoji hold U+FE0F, which some editors and copies drop. VS16_REPLY in links_data.py writes them with escapes.
- Put backticks in single quotes on a recipe line. In double quotes bash runs them as a command.
- A `.txt` capture shows a panel only by its rows: the label, then each code row a column further in than the prose. `t3v render` shows the panel's background.
- The inline parser draws ```` ```x`y``` ```` as `xy`, where CommonMark shows x`y in code. The fences change left that as it was.
- The run followed by U+00A0 looks like a closing fence on screen. The fixture writes the U+00A0 as an escape. Where that run closes the panel, `- still in the panel` draws as the prose bullet `• still in the panel`.
- The reply is thr-legacy's last item, so at 200x50 all of it shows when the thread opens. On a shorter screen, PgUp reaches its first rows.
