# Alacritty recordings

These recordings are copied unchanged from `tests/ref/<name>/alacritty.recording` in the alacritty_terminal 0.26.0 crate, part of [Alacritty](https://github.com/alacritty/alacritty) by Christian Duerr, Joe Wilm and the Alacritty contributors. They are licensed under the Apache License 2.0, in [LICENSE-APACHE](LICENSE-APACHE), not under t3term's MIT license.

`src/terminal_emulator/tests.rs` feeds each one to `TerminalEmulator` and to a bare alacritty_terminal `Term`, and checks that both end in the same state. Their sizes come from each recording's `size.json`.

| Recording | Columns | Rows |
| --- | --- | --- |
| colored_underline | 157 | 40 |
| csi_rep | 105 | 29 |
| history | 105 | 29 |
| sgr | 139 | 35 |
| tab_rendering | 116 | 63 |
| vim_simple_edit | 80 | 24 |
| wrapline_alt_toggle | 139 | 35 |
| zerowidth | 139 | 35 |
