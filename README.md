# Vision

Vision takes a ticket from a tracker to a pull request that is ready to merge. Named agents do the work in Claude Code sessions. A person gives the ticket, approves the plan, and merges.

Vision is in development. The design is in `docs/design.md`.

## What it does

- It refines the ticket, plans, builds with tests first, reviews its own branch, and fixes red CI runs.
- It asks its questions as cards. It keeps each answer with its reason, and it answers a later question from that memory.
- The memory is a folder of small text files in a git repo. You can edit each file, and an agent proposes rules that you accept or reject.
- No session holds a credential for the tracker or the forge. One executor does each change outside the machine.
- Each agent has a model that you set. The value stays until you change it.

## Build

```sh
cargo build --release
./target/release/vision init
./target/release/vision doctor
```

Vision needs a stable Rust toolchain, git, and Claude Code.

## Credits

The idea of permanent agents with names and areas comes from the article "How I Built the Most AI-Pilled Company in the World". The question card and the rule form "When X, make sure that Y, because Z" follow ideas of the public project `zhuravel/magnum`.
