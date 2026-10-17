# Working on t3term

t3term is a Rust terminal client for the T3 Code server. README.md documents the CLI contract, including the exit codes and `--json` output. `.greptile/config.json` lists the rules reviews apply. docs/parity.md is the feature plan, and docs/sprint.md records the sprint that builds it.

## Who does what

- The user sets scope and merges every pull request. No agent merges a pull request, whether it changes the UI or not.
- GPT-6.1 Sol (xhigh) orchestrates the sprint. It runs all testing and computer use itself, reviews each pull request, gives it its verdict and prepares it for merge.
- Claude Opus 5.5 (xhigh) writes the implementation, the tests and the fixes. It is the only agent that writes code.
- The agent that wrote a pull request never gives it its verdict. Sol does, or a fresh agent on another model that didn't write it.

## Pull requests

- Keep them small and frequent. Open a draft early, push at each checkpoint and mark it ready when the work is done.
- Keep the description short and lead with what the pull request changes. End it with a decision log under these five headings:
  - What I noticed: the problem or gap that started the work.
  - How I confirmed it: the code, contract or run that showed it.
  - Options: the approach chosen and the ones rejected, with why.
  - What review changed: what Greptile, Sol or the user asked for and what changed as a result.
  - Known limits: what the pull request leaves out or doesn't handle.
- Changes to how the TUI looks carry screenshots or recordings. Keep UI and non-UI changes in separate pull requests where practical.
- Greptile reviews every pull request. A pull request is ready for the user when CI passes, every Greptile finding is fixed or answered, and its head commit has a PASS or PASS+NOTES verdict (see Verification).

## Verification

Tests show what each branch of the code does. They don't show that a feature works in the built binary, so a pull request that changes what a user sees or does also gets driven there. The skills under `.claude/skills/` hold the procedures:

- verify-t3term drives the built binary in tmux against a fake T3 server, through `scripts/t3v`. Its `features/` map says how to reach and drive each feature, and `scripts/smoke` runs every recipe in it.
- verify-pr gives a pull request its verdict, PASS, PASS+NOTES or FAIL, from the same drive on its base and its head.
- verify-this proves or disproves one claim with a baseline and a treatment.
- tdd covers regression tests, blast-radius what a change could break outside its diff, and benchmark-checklist any performance number.
- maintain-verify-t3term keeps the feature map true to the app.

A pull request that changes what a user sees or does updates its file under `features/`, or adds one. A bug fix carries a test that fails with the fix reverted. A verdict covers one patch, so a push that changes the patch needs a new verdict. verify-pr says when a rebase keeps it.

## Checks

CI runs these on macOS for every pull request, with Rust pinned to 1.95.0 in `.github/workflows/ci.yml`:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Tests must not read the real `~/.t3`, use the Keychain or reach a real T3 server. `tests/fake_server.rs` drives the RPC client against a fake WebSocket server, and `tests/cli.rs` runs the binary with a temporary HOME and T3CODE_HOME against fake servers on 127.0.0.1. Follow one of those.

CI needs no secrets. Never read `.env` files or print tokens. If a feature ever needs a real secret, request it through tokenstash.
