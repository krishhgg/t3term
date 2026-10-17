# Appearance

## Sub-features

- **The three choices.** System, Light and Dark. Light draws T3 Code's light colors and Dark its dark ones. System draws whichever macOS shows, read with `defaults read -g AppleInterfaceStyle` at start and again each time Alt+Shift+A comes back to System. Off macOS, or when the read fails, System draws dark at start and otherwise keeps what draws.
- **The toast.** After each press the status line reads `Appearance: System`, `Appearance: Light` or `Appearance: Dark`. The next key clears it.
- **Saving.** settings.json keeps the choice as `appearanceMode`: `system`, `light` or `dark`. A file without the key, or with a value t3term doesn't know, draws dark and keeps the rest of the file.
- **The light colors in truecolor.** The canvas is `#fcfcfc`, the sidebar `#fafafa`, text `#27272a`, borders and chips `#e4e4e7`, the composer and menus white, and the primary color `#1b4ed8`. Dark's canvas is `#0a0a0a` and its sidebar `#000000`.
- **Text on the primary color.** The send button's `↑` once the composer has a draft, Approve, and the label that says how far the transcript is scrolled. Light draws it white on `#1b4ed8`. Dark keeps `#f5f5f5` on `#346bf1`.
- **One redraw.** A switch redraws every row at once in the new colors: the transcript, the details, the linked pull requests' list and a pull request's details, and the worktree setup card. The composer keeps its draft and an open menu stays open.

## How to get to it (user POV)

Alt+Shift+A from anywhere: the sidebar, the composer, the transcript, the details, over an open menu or the details dialog. It never types into the composer. On a Mac it needs Option to send Meta, as every Alt key does. Alt+A without Shift still answers an approval.

## Driving it with t3v

Scenario `links` at 200x50 in truecolor, the default. tmux sends `M-A` as Esc then `A`, which t3term reads as Alt+Shift+A. Each capture prints its `.txt` and `.ans` paths, and the `.ans` keeps tmux's SGR codes, one per attribute, such as `ESC[48;2;252;252;252m` for a background. The greps check colors there.

```bash
$T3V start
$T3V click --text "Mixed: every state" && $T3V wait --text "/  Mixed: every state"
$T3V keys --literal draft && $T3V wait --text "│ draft"                 # a draft lights the send button
$T3V keys M-b && $T3V wait --text "Pull requests 11"
$T3V keys Tab Tab Right && $T3V wait --text "#62" --text "9 open · 11 linked"
D=$($T3V capture dark | grep 'ans$') && grep -q '48;2;10;10;10m' "$D" && grep -q '48;2;23;23;23m' "$D"   # dark's canvas and details, drawn before the switch

# From the details pane: Dark, then System, then Light. System follows macOS, so its colors aren't checked.
$T3V keys M-A && $T3V wait --text "Appearance: System"
$T3V keys M-A && $T3V wait --text "Appearance: Light" --text "│ draft" --text "#62"
L=$($T3V capture light | grep 'ans$') && grep -q '48;2;252;252;252m' "$L" && grep -q '48;2;250;250;250m' "$L"   # light's canvas and sidebar
! grep -q '48;2;10;10;10m\|48;2;0;0;0m\|48;2;23;23;23m\|38;2;245;245;245m' "$L"   # no row kept a dark color
grep -q '38;2;255;255;255m.\[48;2;27;78;216m ↑' "$L"                    # white on light's primary on the send button
grep -q '"appearanceMode": "light"' "$RUN/home/.config/t3term/settings.json"

# A pull request's details in the dialog, then Dark over it. Down clears the toast, so the status line shows the dialog's Esc close.
$T3V keys Down Enter && $T3V wait --text "← List" --text "Repository"
$T3V resize 120 40 && $T3V wait --text "Thread details" --text "Esc close" --idle 300
$T3V keys M-A && $T3V wait --text "Appearance: Dark" --text "← List" --text "Repository"
K=$($T3V capture dark-again | grep 'ans$') && grep -q '48;2;10;10;10m' "$K"
! grep -q '48;2;252;252;252m\|48;2;250;250;250m\|48;2;255;255;255m\|38;2;39;39;42m' "$K"   # no row kept a light color
grep -q '38;2;245;245;245m.\[48;2;52;107;241m ↑' "$K"                   # dark keeps its text color on the send button

# Over an open menu, which stays open, and a restart that keeps Light.
$T3V keys Escape && $T3V wait --gone "Thread details"
$T3V keys M-m && $T3V wait --text "Search models"
$T3V keys M-A && $T3V wait --text "Appearance: System" --text "Search models"
$T3V keys M-A && $T3V wait --text "Appearance: Light" --text "Search models"
$T3V restart
R=$($T3V capture restarted | grep 'ans$') && grep -q '48;2;252;252;252m' "$R" && ! grep -q '48;2;10;10;10m' "$R"

# A value t3term doesn't know draws dark and keeps the rest of the file.
printf '{"appearanceMode": "sepia", "sidebarHidden": true}\n' > "$RUN/home/.config/t3term/settings.json"
$T3V restart --ready "Ctrl+B sidebar"
U=$($T3V capture unknown | grep 'ans$') && grep -q '48;2;10;10;10m' "$U" && ! grep -q '48;2;252;252;252m' "$U"
$T3V keys M-A && $T3V wait --text "Appearance: System"
grep -q '"appearanceMode": "system"' "$RUN/home/.config/t3term/settings.json" && grep -q '"sidebarHidden": true' "$RUN/home/.config/t3term/settings.json"
```

`--color 256` shows the 256-color form: light's grays round to three, so the composer, menus and the sidebar share the canvas's white. `t3v render NAME NAME.png` draws a capture to look at.

## Gotchas

- System draws what the machine running the drive shows, because t3term reads macOS's own setting. A recipe checks only the toast for System, never its colors.
- Captures are the proof of a color. `.txt` has no colors, so a claim about colors cites the `.ans` or a PNG rendered from it.
- tmux writes one SGR code per attribute and repeats a color only where it changes, so look for a color's code, not for a whole cell's style. The send button is the exception the recipe relies on: its foreground and background codes come next to each other.
- The send button takes the primary color only while the composer holds a draft. Without one it draws muted.
- Alt+A, Alt+S and Alt+D answer an approval, and the fake server serves none, so the recipe can't show that Alt+A still answers one. Alt+Shift+A never does.
- The worktree setup card isn't reachable: the fake server answers `subscribeWorktreeSetup` with null only.
- The choice is saved in the run's HOME and survives `restart`. A new run starts dark.
