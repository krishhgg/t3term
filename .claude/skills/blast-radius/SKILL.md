---
name: blast-radius
description: "Find what a t3term change could break outside its diff, and prove the one fact it's safe because of by running real code, not by writing it up. Use for \"blast radius of X\", \"what could this break\", a small diff you don't trust, or step 5 of verify-pr."
disable-model-invocation: true
---

# Blast radius

Find what a change breaks somewhere else, before it lands. Listing callers isn't the job, because `rg` does that in a second. The job is the breakage a search won't show.

Adapted from pstack's blast-radius skill (github.com/cursor/plugins, MIT) for this repo.

## Don't trust your own writeup

A writeup that sounds right reads the same whether or not it is. So find the one or two facts the change's safety rests on and prove them by running code.

For each fact, get it as far down this list as is cheap, and say where it stopped:

1. You said so. Worth nothing alone.
2. You pointed at the line: a real `file:line` here, in T3's TypeScript source, or in a crate's source at the version `Cargo.lock` pins (`~/.cargo/registry/src/`).
3. You walked the failure step by step and it can't happen.
4. You ran it: a `cargo test` that calls the real code and fails loudly if you're wrong.
5. You reproduced it in the running app with verify-t3term.

Step 4 is usually one focused test in the module's `#[cfg(test)]` block, run with `cargo test --locked <name>`. Step 5 is a `t3v` drive.

## Steps

1. **Read the change.** The diff, the symbols it adds, changes and removes, and what it now does differently, including what the diff doesn't spell out. `gh pr view N` and `git log -p` for the history.
2. **Find the one fact.** Most risky-looking changes are safe because of one fact, like "this match arm only runs for events of the open thread". If it holds, most of the risk clears at once. Spend your time here.
3. **Look where search stops.** In t3term the usual places are:
   - **The wire.** Field names and enum strings T3 sends and expects, from T3's TypeScript source at the pin in `docs/parity.md`. A rename on our side compiles fine and silently drops the field (serde `default`, `Option`).
   - **Files on disk.** `~/.config/t3term/settings.json` keys and old values an older build wrote, the saved environments file, the Keychain entry's service and account names.
   - **The CLI contract.** README.md's commands, flags, `--json` fields and exit codes, which scripts depend on.
   - **The terminal.** What crossterm decodes from a key (Alt as Esc, Option as Meta on macOS), what the terminal does with an escape sequence, wide and joined characters, resizes between two frames.
   - **Time and order.** What runs between two frames, after a reconnect, when two streams deliver in the other order, when a watch closes before it answers.
   - **Cost.** A new timer, a redraw nothing caused, work that grows with the transcript (`idle-cost` in `.greptile/config.json`).
   - **Crates.** The behavior of ratatui, crossterm and tokio at the versions `Cargo.lock` pins, not the latest docs.
4. **Be honest about each risk.** Give it a real chance and a real cost. Keep the ones you confirmed. List the ones you checked and cleared separately. Cite a real `file:line`. A search that finds nothing is still an answer. Never make up a caller or an API.
5. **Prove the one fact.** Write the test or the drive, run it and paste what happened.
6. **For a big or wide change, ask more than one model.** Different models catch different real bugs. Merge their answers and say which model raised what.

## What to hand back

- **What it does.** What changed, including the part that isn't obvious.
- **The one fact it's safe because of.** State it, say which step of the list you got it to, and show the proof, or write "unproven".
- **Risks.** How each breaks, its `file:line`, how likely and how bad, and how to check it. Paste the proof for the ones that matter.
- **Cleared.** What you checked and why it's fine.
- **Before you merge.** The cheapest test or drive that catches the real bug, including the one you wrote.

Write it plainly, cite real code, and keep anything private out of what goes on GitHub.
