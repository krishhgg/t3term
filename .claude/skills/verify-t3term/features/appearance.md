# Appearance

## Sub-features

- **The three choices.** System, Light and Dark. Light draws T3 Code's light colors and Dark its dark ones. System draws whichever macOS shows, read with `defaults read -g AppleInterfaceStyle` at start and again each time Alt+Shift+A comes back to System. Off macOS, or until a read works, System draws dark. When a later read fails, System keeps what the last read that worked found.
- **The toast.** After each press the status line reads `Appearance: System`, `Appearance: Light` or `Appearance: Dark`. The next key clears it.
- **Saving.** settings.json keeps the choice as `appearanceMode`: `system`, `light` or `dark`. A file without the key, or with a value t3term doesn't know, draws dark and keeps the rest of the file.
- **T3 Code's light colors in truecolor.** The canvas is `#fcfcfc`, the sidebar `#fafafa`, text `#27272a`, borders and chips `#e4e4e7`, the composer and menus white, and the primary color `#1b4ed8`. Dark's canvas is `#0a0a0a` and its sidebar `#000000`.
- **Text on the primary color.** The send button's `↑` once the composer has a draft, Approve, and the label that says how far the transcript is scrolled. Light draws it white on `#1b4ed8`. Dark keeps `#f5f5f5` on `#346bf1`.
- **Built-in themes.** `theme` in settings.json takes `t3-chat`, `grove`, `ocean`, `ember` or `iris`, the desktop's T3 Chat, Grove, Ocean, Ember and Iris, each with a light and a dark half. `t3-code`, no key or an id t3term doesn't know, such as the desktop's older `t3-grove`, draws T3 Code's colors. t3term reads it at start, and the Change theme list saves it. Alt+Shift+A switches between the theme's halves, and its save keeps `theme`. Dark canvases in truecolor: T3 Chat `#1f1a24`, Grove `#1b2821`, Ocean `#17212b`, Ember `#291e1a`, Iris `#1d1929`. Grove light's is `#f3f7f4`.
- **What a theme sets.** The canvas, text, borders, the sidebar, the composer, menus, message bubbles, code, warnings and errors, and the action color on the send button with the text on it. Status, provider and project colors stay T3 Code's: `LD` keeps `#05df72` in dark and Open keeps `#007a55` in light.
- **The Change theme list.** A dialog titled `Change theme`, 40 columns wide, a third of the way down. Its rows are `T3 Code`, `T3 Chat`, `Grove`, `Ocean`, `Ember` and `Iris`. The theme that draws says `Current`, and each row ends with two `●`, the light half's focus color on its canvas and then the dark half's. The highlighted row takes the menu highlight, `#262626` in T3 Code's dark half, and starts on the first row. The status line reads `↑↓ choose · Enter select · Esc close`. Enter or a click on a row draws that theme at once in the half that shows and saves it as `theme`. While the list is open it takes every key, paste and click, but Alt+Shift+A and Ctrl+C or Ctrl+Q to quit, so nothing reaches the composer, the sidebar, a run or a dialog under it.
- **Focus marks.** The theme's focus color marks what is current: the sidebar's `▎` while the sidebar has the keyboard, a menu's `✓`, the details' open tab, the open details button `◨`, a pull request's `← List` and a running task step. T3 Code's focus color is its primary blue, so its halves draw as before. Grove's is `#69d69a` in dark and `#1b7d50` in light.
- **One redraw.** A switch redraws every row at once in the new colors: the transcript, the details, the linked pull requests' list and a pull request's details, and the worktree setup card. The composer keeps its draft and an open menu stays open.

## How to get to it (user POV)

Alt+Shift+A from anywhere: the sidebar, the composer, the transcript, the details, over an open menu or the details dialog. It never types into the composer. On a Mac it needs Option to send Meta, as every Alt key does. Alt+A without Shift still answers an approval.

Ctrl+Alt+A opens the Change theme list from any pane, over a menu, which it closes, or over the details dialog. ↑ and ↓, `j` and `k`, Ctrl+P and Ctrl+N, Home and End, Page Up and Page Down or the wheel move the highlight. Enter or a click on a row chooses. Esc, Ctrl+Alt+A again or a click outside the list closes it. Alt+Shift+A still switches the half under it. Like Alt+A, it needs Option to send Meta on a Mac. The theme is saved under `theme` in `~/.config/t3term/settings.json`, and t3term reads it at start.

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

# A built-in theme from settings.json, read at start: Grove dark's canvas and sidebar. The sidebar has the keyboard, so its bar takes Grove's focus color. LD keeps T3 Code's green.
$T3V resize 200 50 && S="$RUN/home/.config/t3term/settings.json"
printf '{"theme": "grove"}\n' > "$S" && $T3V restart
G=$($T3V capture grove-dark | grep 'ans$') && grep -q '48;2;27;40;33m' "$G" && grep -q '48;2;33;54;43m' "$G" && ! grep -q '48;2;10;10;10m' "$G"
grep -q '38;2;105;214;154m.\[48;2;[0-9;]*m▎' "$G" && grep -q '38;2;5;223;114m' "$G"

# Alt+Shift+A keeps the theme and saves the appearance beside it. Grove light's focus marks, its action color on the send button, and Open in T3 Code's green.
$T3V keys M-A M-A && $T3V wait --text "Appearance: Light"
grep -q '"theme": "grove"' "$S" && grep -q '"appearanceMode": "light"' "$S"
$T3V click --text "Mixed: every state" && $T3V wait --text "/  Mixed: every state"
$T3V keys --literal draft && $T3V wait --text "│ draft"
$T3V keys M-b && $T3V wait --text "Pull requests 11"
$T3V keys Tab Tab Right && $T3V wait --text "#62" --text "9 open · 11 linked"
GL=$($T3V capture grove-light-list | grep 'ans$') && grep -q '38;2;0;122;85mOpen' "$GL"
$T3V keys Down Enter && $T3V wait --text "← List" --text "Repository"
GL=$($T3V capture grove-light | grep 'ans$') && grep -q '48;2;243;247;244m' "$GL" && grep -q '48;2;226;237;231m' "$GL" && ! grep -q '48;2;252;252;252m\|48;2;250;250;250m' "$GL"
grep -q '38;2;27;125;80mPull requests 11' "$GL" && grep -q '38;2;27;125;80m← List' "$GL" && grep -q '38;2;27;125;80m.\[48;2;[0-9;]*m ◨ ' "$GL"
grep -q '38;2;255;250;255m.\[48;2;143;100;16m ↑' "$GL"                  # Grove light's action color and the text on it

# The other four built-in themes in dark, then t3-code and an older id t3term doesn't know, which draw T3 Code's colors.
printf '{"theme": "t3-chat"}\n' > "$S" && $T3V restart && C=$($T3V capture t3-chat | grep 'ans$') && grep -q '48;2;31;26;36m' "$C"
printf '{"theme": "ocean"}\n' > "$S" && $T3V restart && C=$($T3V capture ocean | grep 'ans$') && grep -q '48;2;23;33;43m' "$C"
printf '{"theme": "ember"}\n' > "$S" && $T3V restart && C=$($T3V capture ember | grep 'ans$') && grep -q '48;2;41;30;26m' "$C"
printf '{"theme": "iris"}\n' > "$S" && $T3V restart && C=$($T3V capture iris | grep 'ans$') && grep -q '48;2;29;25;41m' "$C"
printf '{"theme": "t3-code"}\n' > "$S" && $T3V restart && C=$($T3V capture t3-code | grep 'ans$') && grep -q '48;2;10;10;10m' "$C" && grep -q '38;2;52;107;241m.\[48;2;[0-9;]*m▎' "$C"
printf '{"theme": "t3-grove"}\n' > "$S" && $T3V restart && C=$($T3V capture old-id | grep 'ans$') && grep -q '48;2;10;10;10m' "$C" && ! grep -q '48;2;27;40;33m' "$C"

# Ctrl+Alt+A, which tmux sends as C-M-a, opens Change theme over the model menu and closes the menu. A run is going, for the Ctrl+X check below.
$T3V click --text "Mixed: every state" && $T3V wait --text "/  Mixed: every state"
$T3V keys --literal draft && $T3V wait --text "│ draft"
$T3V run thr-mixed running && $T3V wait --text "Working (1)"
$T3V keys M-m && $T3V wait --text "Search models"
$T3V keys C-M-a && $T3V wait --text "│ Change theme" --text "│ T3 Code             Current" --text "↑↓ choose · Enter select · Esc close" --gone "Search models"
T=$($T3V capture theme-list | grep 'ans$') && grep -A6 '│ Change theme' "${T%.ans}.txt" | grep -o 'T3 Code\|T3 Chat\|Grove\|Ocean\|Ember\|Iris' | paste -sd, - | grep -qx 'T3 Code,T3 Chat,Grove,Ocean,Ember,Iris'
grep -q '48;2;38;38;38mT3 Code ' "$T"                                     # the highlight starts on the first row
grep -q '38;2;27;125;80m.\[48;2;243;247;244m ● .*38;2;105;214;154m.\[48;2;27;40;33m ● ' "$T"   # Grove's dots: each half's focus color on its canvas

# The list takes a paste, a letter, Ctrl+B and Ctrl+X. Down Down and Enter draw Grove at once, save it and keep the draft.
$T3V paste --text pasted && $T3V keys x C-b C-x Down Down && $T3V wait --text "│ Change theme" --idle 300
$T3V status --json | grep -q '"commandsRefused": 0'                      # Ctrl+X sent no interrupt
$T3V keys Enter && $T3V wait --gone "│ Change theme" --gone "pasted" --gone "draftx" --gone "Ctrl+B sidebar" --text "│ draft"
C=$($T3V capture list-grove | grep 'ans$') && grep -q '48;2;27;40;33m' "$C" && ! grep -q '48;2;10;10;10m' "$C"
grep -q '"theme": "grove"' "$S" && grep -q '"sidebarHidden": false' "$S"

# Reopened, Grove is Current and the highlight is on the first row again. Each choice below lands on another theme, so a key that did nothing shows as the wrong canvas.
$T3V keys C-M-a && $T3V wait --text "│ Grove               Current"
$T3V keys End Up Enter && $T3V wait --gone "│ Change theme"
C=$($T3V capture list-ember | grep 'ans$') && grep -q '48;2;41;30;26m' "$C"
$T3V keys C-M-a && $T3V wait --text "│ Ember               Current"
$T3V keys PageDown PageUp j j j j k Enter && $T3V wait --gone "│ Change theme"
C=$($T3V capture list-ocean | grep 'ans$') && grep -q '48;2;23;33;43m' "$C"
$T3V keys C-M-a && $T3V wait --text "│ Ocean               Current"
$T3V keys PageDown Enter && $T3V wait --gone "│ Change theme"
C=$($T3V capture list-iris | grep 'ans$') && grep -q '48;2;29;25;41m' "$C"
$T3V keys C-M-a && $T3V wait --text "│ Iris                Current"
$T3V keys End Home C-n C-n C-p Enter && $T3V wait --gone "│ Change theme"
C=$($T3V capture list-t3-chat | grep 'ans$') && grep -q '48;2;31;26;36m' "$C"
grep -q '"theme": "t3-chat"' "$S"

# The wheel and a click. T3 Code also heads the sidebar, so the click starts looking at column 40.
$T3V keys C-M-a && $T3V wait --text "│ T3 Chat             Current"
$T3V click --text "Ocean" --button wheel-down --repeat 2 && $T3V wait --idle 300
$T3V keys Enter && $T3V wait --gone "│ Change theme"
C=$($T3V capture list-wheel | grep 'ans$') && grep -q '48;2;27;40;33m' "$C"
$T3V keys C-M-a && $T3V wait --text "│ Grove               Current" --idle 300
$T3V click --text "T3 Code" --from-col 40 && $T3V wait --gone "│ Change theme"
C=$($T3V capture list-click | grep 'ans$') && grep -q '48;2;10;10;10m' "$C" && grep -q '"theme": "t3-code"' "$S"

# Esc, Ctrl+Alt+A and a click outside close it and choose nothing.
$T3V keys C-M-a && $T3V wait --text "│ Change theme" && $T3V keys Down Escape && $T3V wait --gone "│ Change theme"
$T3V keys C-M-a && $T3V wait --text "│ Change theme" && $T3V keys Down C-M-a && $T3V wait --gone "│ Change theme"
$T3V keys C-M-a && $T3V wait --text "│ Change theme" --idle 300 && $T3V click --at 150 40 && $T3V wait --gone "│ Change theme"
grep -q '"theme": "t3-code"' "$S"

# Alt+Shift+A switches the half under the open list, and a choice draws in that half.
$T3V keys C-M-a && $T3V wait --text "│ Change theme"
$T3V keys M-A M-A && $T3V wait --text "Appearance: Light" --text "│ Change theme"
$T3V keys Down Down Enter && $T3V wait --gone "│ Change theme"
C=$($T3V capture list-grove-light | grep 'ans$') && grep -q '48;2;243;247;244m' "$C" && ! grep -q '48;2;27;40;33m' "$C"
grep -q '"theme": "grove"' "$S" && grep -q '"appearanceMode": "light"' "$S"
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
- t3term reads `theme` from settings.json only at start, so a recipe that writes the file then runs `t3v restart`. A file written without `appearanceMode` starts dark. The Change theme list needs no restart.
- tmux sends `C-M-a` as Esc then Ctrl+A, which t3term reads as Ctrl+Alt+A. Wait for `│ Change theme` before the next key: a key sent before the list draws goes to the pane under it.
- `T3 Code` heads the sidebar too, so a click on the list's T3 Code row needs `--from-col`. A click on the list right after it opens does nothing until a frame draws it, so wait first.
- The `Appearance:` toast replaces the list's keys in the status line until the next key.
- The sidebar's `▎` takes the focus color only while the sidebar has the keyboard, as it does right after a start. Once a thread opens, the composer has it and the bar draws muted.
- The approval and question panels' tints follow the theme too, but the fake server serves no request to show them.
