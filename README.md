# Vision

Vision takes a ticket from a tracker to a pull request that is ready to merge. Named agents do the work in Claude Code sessions. A person gives the ticket, approves the plan, and merges.

Vision is in development. The demo path works from the ticket to the merged pull request with real sessions. The adapter for Jira and GitHub is built, and a test drives its whole path against a fake server. No run against the real services happened yet. The design is in `docs/design.md`.

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

## Try the demo

The demo adapter needs no network. Its tracker is a JSON file, and its forge is a bare git repo on your disk. The agents are real Claude Code sessions with your login. One demo ticket costs a few cents with the model `haiku`.

1. Build Vision, and put it on your `PATH`.

   ```sh
   cargo build --release
   export PATH="$PWD/target/release:$PATH"
   vision init
   ```

2. Make a demo world and one clone of its toy project.

   ```sh
   adapters/demo/bin/world new ~/vision-demo/world
   adapters/demo/bin/world clone ~/vision-demo/world ~/vision-demo/clone
   ```

3. Open `~/.vision/config.toml`, and set these values.

   ```toml
   clones = ["~/vision-demo/clone"]

   [claude]
   config_dir = ""        # use your default Claude Code login

   [adapter]
   root = "~/vision-demo/world"
   test_command = "python3 -B -m unittest discover -q"
   ```

4. Give each agent a cheap model for the demo. This step is optional.

   ```sh
   for agent in scout builder reviewer tester shepherd judge curator; do
     vision model set "$agent" haiku --effort low
   done
   ```

5. Make sure that the setup is complete, and take a ticket.

   ```sh
   vision doctor
   vision pick
   vision take DEMO-12
   ```

6. Run `vision tick` each few seconds, or install the timer with `vision install`. `vision status` shows the step of the ticket.

7. Do what `vision needs` shows. It prints the exact command for each thing that waits for you, such as the approval of the plan.

8. Play the team after the pull request is open. Comment, mark it as ready, approve, and merge.

   ```sh
   adapters/demo/bin/world comment ~/vision-demo/world 1 "Please also test a negative result."
   adapters/demo/bin/world ready ~/vision-demo/world 1
   adapters/demo/bin/world approve ~/vision-demo/world 1
   adapters/demo/bin/world merge ~/vision-demo/world 1
   ```

`vision log DEMO-12` shows the timeline of the ticket. `vision report` shows the cost and your minutes.

Vision starts no new turn above the values of `[limits]` in `config.toml`. They are parts of your Claude subscription windows. If the status shows the flag `waits-for-limit`, wait for the reset or raise the value.

## Credits

The idea of permanent agents with names and areas comes from the article "How I Built the Most AI-Pilled Company in the World". The question card and the rule form "When X, make sure that Y, because Z" follow ideas of the public project `zhuravel/magnum`.
