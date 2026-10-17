# Transcript

## Sub-features

- **Code fences.** A line that starts with three or more backticks or tildes, after any spaces or tabs, opens a code panel. Its first row is a label with the first word of the fence's info string, so ```` ```rust ignore ```` labels the panel `rust`, and each code row starts a column in, on the panel's background. Only a line of the same mark, at least as long, with nothing after it but spaces or tabs closes the panel. So a four-backtick fence can hold a three-backtick example, and a run with text after it is a row of code. Other whitespace counts as text, as CommonMark has it: a run followed by U+00A0 is a row of code, and the rows after it stay in the panel. Backticks with another backtick later in the line are inline code and open no panel. A fence never closed runs to the end of the reply.

## How to get to it (user POV)

Open a thread from the sidebar with a click, or with ↑ or ↓ and Enter. Its messages draw in the transcript, the newest at the bottom. PgUp and PgDn scroll it from anywhere, and so does the wheel over it. With the transcript focused (Esc from the composer), ↑ and ↓ scroll it, and g and G go to the top and the bottom.

## Driving it with t3v

Scenario `links` at 200x50. Each block opens the thread whose last reply holds its text, as the scenario's docstring says.

```bash
$T3V start

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

- Put backticks in single quotes on a recipe line. In double quotes bash runs them as a command.
- A `.txt` capture shows a panel only by its rows: the label, then each code row a column further in than the prose. `t3v render` shows the panel's background.
- The inline parser draws ```` ```x`y``` ```` as `xy`, where CommonMark shows x`y in code. The fences change left that as it was.
- The run followed by U+00A0 looks like a closing fence on screen. The fixture writes the U+00A0 as an escape. Where that run closes the panel, `- still in the panel` draws as the prose bullet `• still in the panel`.
- The reply is thr-legacy's last item, so at 200x50 all of it shows when the thread opens. On a shorter screen, PgUp reaches its first rows.
