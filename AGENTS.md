# Working on t3term

t3term is a Rust terminal client for the T3 Code server. README.md documents the CLI contract, including the exit codes and `--json` output. `.greptile/config.json` lists the rules reviews apply. docs/parity.md is the feature plan, and docs/sprint.md records the sprint that builds it.

## Who does what

- The user sets scope and merges every pull request. No agent merges a pull request, whether it changes the UI or not.
- GPT-6.1 Sol (xhigh) orchestrates the sprint. It runs all testing and computer use itself, reviews each pull request, prepares it for merge and writes its walkthrough.
- Claude Opus 5.5 (xhigh) writes the implementation, the tests and the fixes. It is the only agent that writes code.

## Pull requests

- Keep them small and frequent. Open a draft early, push at each checkpoint and mark it ready when the work is done.
- Keep a decision log in the description, in the format of pr-walkthrough's `references/decision-log.md`: what you noticed, how you confirmed it, the options you chose and rejected, what review changed and the known limits.
- Every pull request gets a short walkthrough made with the pr-walkthrough skill (https://github.com/krishhgg/pr-walkthrough) that teaches the Rust it uses. The pages stay on the reader's machine, so never commit them.
- Changes to how the TUI looks carry screenshots or recordings. Keep UI and non-UI changes in separate pull requests where practical.
- Greptile reviews every pull request. A pull request is ready for the user when CI passes and every Greptile finding is fixed or answered.

## Checks

CI runs these on macOS for every pull request, with Rust pinned to 1.95.0 in `.github/workflows/ci.yml`:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Tests must not read the real `~/.t3`, use the Keychain or reach a real T3 server. `tests/fake_server.rs` drives the RPC client against a fake WebSocket server, and `tests/cli.rs` runs the binary with a temporary HOME and T3CODE_HOME against fake servers on 127.0.0.1. Follow one of those.

CI needs no secrets. Never read `.env` files or print tokens. If a feature ever needs a real secret, request it through tokenstash.
