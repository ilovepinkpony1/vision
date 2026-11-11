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

## Use the app

Type `vision` in a terminal, and press Enter. A loader shows while Vision reads the home folder, the ledger, and the queue of the tracker. Then Home lists the tickets in progress and the tickets that can start.

Press Enter on a ticket of the queue to start it. Press Enter on a ticket in progress to open its live view. These keys work on Home:

- `n` opens what waits for you, one card at a time. You answer each card with one key.
- `m` shows the model of each agent, and changes it.
- `s` shows the stats of the last 7 days, or of all time.
- `p` pauses the engine, or starts it again.
- `K` opens the kill switch.
- `?` shows each key.
- `q` quits.

The live view shows the path of the ticket, what happens now, and the work of the agent as it happens. These keys work in the live view:

- `t` sends a line to the agent that works, or keeps it for the next turn.
- `n` opens what waits for you on this ticket.
- `o` opens the session of the last turn in this terminal. The ticket pauses. While a turn runs, `o` on its card stops the turn first. Press Enter afterwards to continue the ticket.
- `x` stops the ticket. Enter starts a stopped ticket again.
- `r` retries a ticket that waits for you.
- `l` shows the log, `p` shows the plan, and `d` shows the commits that the forge does not have yet.
- The arrows scroll the activity, and `G` goes back to its end.
- Esc goes back to Home.

Each thing that waits for you has its card:

- A question takes the letter of a choice, your own words with `e`, or `k` for a question that was needless. Then type a reason, or press Tab to skip it.
- A plan takes `a` to approve it.
- A write that waits takes `y` to approve it.
- The first write of a verb in the mode `shadow` takes `y` or `a`. `y` sends it, and Vision asks you each time for that verb. `a` sends it, and Vision never asks again for that verb.

The app approves only the exact plan or write that the card showed. If it changed, the app approves nothing and shows you the new one.

To see your own picture in the loader, put a text file with color codes at `~/.vision/loader.ans`. A tool such as `chafa` writes such a file.

While the window is open, the app runs `vision tick` each minute and after each action. It also runs the next pass at once while a ticket has work ready. If you quit, a running turn finishes by itself. The next step waits until you open Vision again, or until the timer of `vision install` runs.

The app needs a window of 100 columns by 28 rows. Each command of the sections below still works in a shell.

## Use it with Jira and GitHub

The adapter `adapters/jira-github` connects Vision to Jira Cloud and GitHub. The comment block at the top of its `adapter.toml` documents each key. The tests drive it against a fake server, and no test calls the real services. So start in small steps.

1. Make one clone of your repo that only Vision uses. Add its path to `clones` in `~/.vision/config.toml`.

2. Make your own adapter folder in the home of Vision.

   ```sh
   mkdir -p ~/.vision/adapters/myproject
   cp adapters/jira-github/adapter.toml ~/.vision/adapters/myproject/adapter.toml
   ```

3. Edit the copy. Set `name = "myproject"`, and set `base` to your main branch, such as `origin/main`. Add the line `scripts = "<the path of this repo>/adapters/jira-github"` after `base`. Remove each step that your project does not need, such as `qa`.

4. In the copy, set the mode of each verb that writes to `shadow`. A verb in this mode records what it intends, and it calls nothing.

5. Set `project = "myproject"` in `config.toml`. Fill the tables `[adapter]` and `[secrets]`. A secret is a command or the name of an environment variable, and never a value.

   ```toml
   [secrets]
   JIRA_TOKEN = { env = "JIRA_API_TOKEN" }
   FORGE_TOKEN = { cmd = "gh auth token" }
   ```

   `gh auth token` is your own GitHub login. If your login can bypass the rules of the main branch, a GitHub App is safer. Leave the App out of the bypass list of those rules, so that GitHub refuses each push of the App to that branch. Then push and act as the App, and open each pull request with your login, so that you are its author:

   ```toml
   [git]
   name = "my-app[bot]"
   email = "123456+my-app[bot]@users.noreply.github.com"

   [secrets]
   FORGE_TOKEN = { cmd = "<the path of this repo>/adapters/jira-github/bin/app-token --app-id <client id> --installation-id <id> --key-command 'security find-generic-password -s my-app-key -w'" }
   PR_TOKEN = { cmd = "gh auth token" }
   ```

   The Keychain item holds the private key of the App as a PEM file. If the item holds the PEM file as one base64 line, end the key command with `| base64 -d`. `gh api users/my-app%5Bbot%5D --jq .id` gives the number of the address. Add the bot to `own_logins` in `[adapter]`, so that the comment step never answers a reply of the bot.

6. Run `vision doctor`, and then run `vision pick`. `vision pick` only reads. It lists the tickets of your queue.

7. Find out what the tests of your project need in the sandbox of a worker. The probe test runs your commands in one real session, and it prints each result.

   ```sh
   VISION_REAL_CLAUDE=1 VISION_PROBE_DIR=~/clones/myproject \
     VISION_PROBE_COMMANDS="bin/test tests/one_test.py" \
     cargo test --test real_claude probe -- --ignored --nocapture
   ```

   If a command cannot read a folder, add that folder to `sandbox.allow_read`. If your tests need a variable, set it in `claude.env`. `VISION_PROBE_ALLOW_READ` and `VISION_PROBE_ENV` let you try both values first.

8. Write the standing rules of your project to `~/.vision/agents/project.md`. One example is the command that runs one test file. Each work agent gets this text.

9. Take one ticket with `vision take KEY`. A verb in the mode `shadow` stops the ticket, and `vision needs` says why. Move that verb up with `vision promote VERB`, and then run `vision retry KEY`.

The timer of `vision install` gets the `PATH` of your shell and none of its other variables. For the timer, give each secret as a command.

## Credits

The idea of permanent agents with names and areas comes from the article "How I Built the Most AI-Pilled Company in the World". The question card and the rule form "When X, make sure that Y, because Z" follow ideas of the public project `zhuravel/magnum`.
