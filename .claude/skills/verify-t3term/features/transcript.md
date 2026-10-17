# Transcript

## Sub-features

- **Emoji that end in U+FE0F.** An emoji followed by U+FE0F (VS16), such as ⚠️ or ❤️, takes two columns, and the text after it stays in its column whether the terminal draws the emoji one column wide or two. A row that scrolls or redraws over other text keeps its spacing: `⚠️ one`, never `⚠️  one` or `❤️two`. t3term draws every row the same way, so this holds in the sidebar and the breadcrumb too, but only the transcript is driven here.

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
```

## Gotchas

- t3v's tmux draws an emoji with U+FE0F two columns wide, tmux 3.6a's default (`variation-selector-always-wide on`). A build without the fix misdraws such a row only where the frame before held text in the column the emoji hides, so the recipe scrolls a row rather than just opening the thread, which draws right on any build. The one-column case can't be driven, because t3v's tmux has no setting for it. The tests in src/tui/wide_emoji.rs cover it.
- The recipe's emoji hold U+FE0F, which some editors and copies drop. VS16_REPLY in links_data.py writes them with escapes.
