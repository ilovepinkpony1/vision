# Vision: design

This document is the specification of Vision. Code and tests follow it. If the code and this document disagree, fix one of them in the same change.

## 1. What Vision does

Vision takes a ticket from a tracker to a pull request that is ready to merge. Named agents do the work in Claude Code sessions. Each agent has a role, a model, and a memory. A person gives the ticket, approves the plan, and merges.

Vision does four things that a plain agent loop does not do:

- It asks questions as cards, and it keeps each answer with its reason.
- It answers a later question from that memory, so that the person does not get the same question again.
- It keeps the credentials out of the sessions. Only the executor changes the tracker and the forge.
- It tests the end of each step itself. The word "done" from an agent proves nothing.

The core knows no project. An adapter gives the steps, the scripts, and the names of one project.

## 2. Terms

- Adapter: the step list and the scripts that know one project.
- Agent: a role with a role file, a model, and its own principles.
- Analyst: the agent that merges the findings of a team into `plan.md`, `acceptance.json`, and its questions.
- Card: one question of an agent, with its choices, its recommendation, and its evidence.
- Batch: the cards of one ask step, created at once and answered as one set.
- Clone: a full copy of the project repo that only Vision uses.
- Curator: the agent that proposes changes to the memory.
- Decision: one answered question with its reason and where it applies.
- End fact: a fact that the runner tests to know that a step is complete.
- Executor: the part of `vision` that holds the credentials and runs each operation.
- Judge: the agent that answers a card, proposes an answer, or sends the card to the person.
- Kind: the class of a ticket, such as bug, feature, or chore. It comes from the issue type through `[kinds]`.
- Ledger: the SQLite store of the evidence.
- Operation: one change outside the machine, with a durable id and a state.
- Principle: one rule that the Curator distilled from decisions, or that the person wrote.
- Supervisor: the process that owns one running session.
- Team: the members of one turn step. Each member is an agent in its own session, and the members run at the same time.
- Turn: one session of Claude Code or of Codex that does one step, or continues one.
- Verb: one kind of operation, such as `git.push`.

## 3. Files on disk

`VISION_HOME` names the state folder. The default is `~/.vision`. Each test sets it to a temporary folder.

```
$VISION_HOME/
  config.toml            the configuration (section 4)
  ledger.sqlite          the ledger (section 5)
  state.json             the snapshot that `vision status` reads
  heartbeat              the time of the last tick, RFC 3339 UTC
  lock                   the lock file of the tick
  off                    the kill switch. Its presence stops each action.
  running.json           the operation or the read that a pass runs now. It is gone when nothing runs.
  jobs/<run-id>/         one job that runs in the background: run.json, answer.json, runner.lock, runner.log
  log/vision.log         one line for each tick action
  log/app.err            what library code prints to standard error while the app runs
  loader.ans             optional: your picture for the loader of the app, at most 240 x 120 cells (section 15.3)
  claude/                CLAUDE_CONFIG_DIR of the sessions, with its own login
  sandbox/               optional: <kind>.json replaces the settings file that Vision renders
  skills/                copies of the skills that the role files name
  agents/                optional: a file here replaces the role file or the schema of that name
    project.md           optional: the standing rules of the project, for each work agent
  journal/<KEY>/<turn-id>.md   one entry for each turn of a ticket (section 16)
  session-journal/<project>/<session-id>.md   a summary of each Claude Code session of the person (section 16). No turn reads it.
  memory/                a git repo (section 9)
    decisions/D-0001.md
    principles/P-001.md
    ARCHIVE.md
  adapters/<name>/       private adapters. They win over the adapters of the repo.
  tickets/<KEY>/
    refine.md plan.md plan.agent.md approved-plan.md answers.md pr.md review.md notes.md
    answer-drafts.json   the drafts of the question form of the app until the send (section 15.3)
    comments-seen.json   the marks of the comments that a finished round of comments read (section 11.5)
    answer-send.json     the answers of one send of the form, only while the send runs
    turns/<turn-id>/
      facts.json         the input of the turn
      stream.jsonl       each event line of the session
      turn.json          the status that the supervisor writes
      settings.json      the Claude Code settings of the session, with its sandbox
      launch.json        the request and the resolved start line of the turn
      role.md            the system prompt file of the session
      supervisor.lock supervisor.log stderr.log
      result.json        the result object of the turn
      inbox/             messages that wait for the session
      inbox/sent/        messages that the session got
```

The file `agents/project.md` holds the standing rules of the project, such as the command that runs one test file. The person writes it. Each work agent gets its text after the common rules of `agents/common.md`. The Judge and the Curator do not get it.

A turn id has the form `<UTC time>-<KEY>-<step>-<n>`, for example `20261116T143000Z-DEMO-12-build-1`. It is safe as a folder name, and it is unique in the ledger. A turn with no ticket uses one of the reserved keys `curator` and `eval`. `vision take` refuses a reserved key.

The repo has this layout:

```
Cargo.toml
src/
  main.rs lib.rs cli.rs          the entry and the command tree
  ctx.rs exit.rs                 the shared context and the exit codes
  paths.rs config/ util/         folders, configuration, time, atomic writes, ids, processes
  init.rs install.rs snapshot.rs the first setup, the system timer, and the format of state.json
  ledger/                        schema, migrations, typed queries
  tick.rs lock.rs status.rs doctor.rs
  turn/                          start line, supervisor, stream parser, contract, inbox, collect
  models.rs                      the model of each agent
  memory/                        files, repo, commands, lint, archive, import
  questions/                     cards, the Judge, answers, decisions
  curator/                       signals, proposals, replay, review
  pipeline/                      steps, states, end facts
  executor/                      verbs, modes, operations, gates
  app/                           the app: model, update, view, loader, worker, terminal
  journal/                       entries, the index in MemPalace, the search, the archive
agents/                          role files and result schemas
adapters/demo/                   an adapter that needs no network
adapters/jira-github/            an adapter for Jira and GitHub
tests/                           integration tests, with a fake `claude`
docs/design.md
```

## 4. The configuration

`vision init` writes this file with the defaults. `vision config` prints the values in use.

```toml
project = "demo"              # the name of the adapter
user = "user"                 # the writer name of the person in the memory
clones = []                   # the clones that Vision owns: absolute paths, or tables with path, env, and adapter

[codex]                       # the Codex CLI, for reader agents whose runner is codex
bin = "codex"
# price = { input = 1.25, output = 10.0 }   # dollars for one million tokens. Without it, a turn on Codex costs 0 and only its tokens count.

[claude]
bin = "claude"
config_dir = "~/.vision/claude"   # a missing key means the folder claude/ of the home. "" means the default login.
env_pass = []                     # more environment names that a session gets
env = {}                          # more variables with a fixed value that a session gets. Never a secret.
attribution = true                # false: no co-author trailer in a commit, no tool line in a pull request
plugins = []                      # folders of Claude Code plugins for each work turn: their skills, with the tool Skill. Never writable in a turn.

[journal]                     # one entry for each turn in journal/, the source of truth, and an index over it
mempalace_url = ""            # the MCP server of MemPalace over HTTP, such as "http://127.0.0.1:8765/mcp". "" means: no index, the search reads the files.
wing = "vision"               # the wing of MemPalace for the entries. The room is the ticket key.
related = 5                   # the entries that a work turn gets in related.md
timeout = 5                   # seconds for one call to MemPalace

[archive]                     # the turn folders and the entries in a local folder or an object store. The aws program does each call to a store with its own credentials.
dir = ""                      # a local folder, such as "~/vision-archive". "" means: no folder. Never together with bucket.
bucket = ""                   # "" means: no bucket. With no folder and no bucket, there is no archive.
prefix = "vision/"            # the start of each key in the bucket
endpoint = ""                 # "" means AWS. The address of another service with the same API, such as "https://store.example.com"
profile = ""                  # the profile of the aws program. "" means its default.
keep_days = 14                # days that a local turn folder stays after the archive confirmed it
region = ""                   # "" means the region of the profile

[sessions]                    # the session files of Claude Code of the person in the archive, and a short summary of each one in the journal
enabled = false               # true needs the archive (archive.dir or archive.bucket)
projects = "~/.claude/projects"   # the folder of the session files. Vision only reads it.
quiet_hours = 24              # a session file counts as finished when it has not changed for this many hours
wing = "claude-sessions"      # the wing of MemPalace for the summaries. The room is the project folder.

[escapes]                     # what the review bot of the team found on our pull request that our own review missed
reviewers = []                # the logins of the review bots, such as ["review-bot[bot]"]. [] means: no check.
priority = "\\*\\*\\[P([0-3])\\]"   # a regular expression whose group 1 is the priority of an inline comment
count = ["0", "1", "2"]       # the priorities that count as an escape when no finding of ours matches
lines = 3                     # a finding of ours matches a comment on the same path within this many lines
verb = "review.read"          # the read verb of the adapter that gives the review and its inline comments
every = 900                   # seconds between two looks at a pull request while the review is missing

[git]                         # the author and committer of each commit of a session
name = ""                     # "" means: "<user> (vision)". A GitHub App bot: "<app>[bot]"
email = ""                    # "" means: an address that names nobody. A bot: "<id>+<app>[bot]@users.noreply.github.com"

[budget]                      # dollars at list price, as Claude Code reports them
reader = 3
worker = 25
tester = 25
ticket = 100
week = 300

[time]                        # minutes before the supervisor kills a turn
reader = 20
worker = 120
tester = 120

[limits]                      # no new turn above these parts of the subscription windows
five_hour = 0.60
seven_day = 0.85

[caps]
review_rounds = 3
ci_fix = 2
qa_fix = 2
merge = 3
comments = 3
judge_minutes = 15
jobs_at_once = 2              # the most jobs that run in the background at one time

[judge]
answer_when_sure = true       # the Judge also answers alone from its own inference
always_ask = ["security", "money", "customer-data", "production", "deploy",
              "delete", "irreversible", "credentials", "external-fact"]

[curator]
replay = 20

[sandbox]                     # what a worker session can reach, next to its clone
allow_read = []               # more folders that it can read, such as a toolchain folder
allow_domains = ["localhost", "127.0.0.1"]
allow_unix_sockets = []

[gates]
max_diff_lines = 3000
sensitive = [".github/", "db/migrate/"]

[app]                         # the full-screen app
sections = []                 # statuses of the tracker whose sections come first in MY TICKETS, in this order, such as ["In Progress", "In Review"]. Each other status follows in the order of the queue.

[modes]                       # verb -> hold | shadow | ask | auto. Only promote and demote change them.

[secrets]                     # name -> how the executor gets it. A session never gets one.
# TRACKER_TOKEN = { cmd = "security find-generic-password -s vision-tracker -w" }
# FORGE_TOKEN   = { env = "GH_TOKEN" }

[adapter]                     # a free table. The executor gives it to each adapter script.

# One table for each agent that the person pinned. See section 10.
# [agents.judge]
# model = "claude-fable-5-2"
# effort = "high"
# pinned_at = "2026-11-16T14:30:00Z"
```

Rules:

- A missing key takes its default. An unknown key is an error that names the key.
- `vision` edits the file with `toml_edit`, so that comments and order stay.
- `~` at the start of a path means the home folder.

An entry of `clones` is a path or a table. A table has `path`, and it can have `env` and `adapter`, such as `{ path = "~/work/app-b", env = { WORKSPACE = "app-b" }, adapter = { app_url = "http://localhost:3500/" } }`. The form `[[clones]]`, one table for each clone after the root keys, gives the same list, and it needs no line `clones = []`. The `env` of a clone follows each rule of `claude.env`. The `adapter` table of a clone holds any values except `dir`, because one adapter serves each clone. The loader refuses a table with another key, a table with no path, and two entries with the same real path. Its message names the entry. An entry with no values prints as a path in `vision config`.

The values of a clone apply to each ticket whose row names that clone. A session of such a ticket gets `claude.env`, then the `env` of the clone over it, then the values of the runner. Each job of the ticket with `env = "session"` gets the same. Each call of a script for such a ticket gets the `[adapter]` table with the `adapter` keys of the clone over it. A key of the clone wins. A call with no ticket, such as the queue, and a ticket with no clone get both tables as they are.

`vision doctor` has one line `clone` for each entry of `clones`. It fails a folder that does not exist and a folder that is not the top folder of a git work tree. It also fails a worktree whose git folder is gone. The line names the path and, for a worktree, "worktree of <main checkout>". It also names the keys of the `env` and of the `adapter` of the clone. A name can be in the `env` of one clone and missing from another clone and from `claude.env`. Then the line of that other clone has a warning, because a session there runs without the name.

`escapes.priority` is a regular expression of a small syntax: characters, `.`, `\d`, `\w`, `\s` and their opposites, classes such as `[0-3]`, groups `( )` and `(?: )`, `|`, `*`, `+`, `?`, `{n,m}`, `^`, `$`, and `(?i)` at the start. It must have a group. A search that takes too many steps finds nothing.

`[agents.NAME] runner` is `claude` or `codex`, and `fallback = { model, effort }` names the Claude model that runs the agent when Codex fails. Only a reader agent runs on Codex. The Judge and the Curator always run on Claude.

`[agents.NAME]` can also name an agent that the adapter defines (section 11.2). The pin of the person wins over the values of the adapter, as it wins over the defaults of Vision.

## 5. The ledger

SQLite in WAL mode, with a busy timeout of 5 seconds. `PRAGMA user_version` holds the schema version. Each migration is one numbered SQL file that the binary embeds.

```sql
CREATE TABLE tickets    (key TEXT PRIMARY KEY, state TEXT, flags TEXT, clone TEXT, branch TEXT, pr INTEGER,
                         head TEXT, taken TEXT, updated TEXT, counts TEXT);
CREATE TABLE spans      (ticket TEXT, state TEXT, waits_on TEXT, reason TEXT, started TEXT, ended TEXT);
CREATE TABLE turns      (id TEXT PRIMARY KEY, ticket TEXT, agent TEXT, step TEXT, kind TEXT, session TEXT,
                         resumes TEXT, model TEXT, effort TEXT, started TEXT, ended TEXT, status TEXT,
                         reason TEXT, cost_usd REAL, input INTEGER, cache_creation INTEGER, cache_read INTEGER,
                         output INTEGER, denials INTEGER, five_hour REAL, seven_day REAL, role_hash TEXT,
                         principles TEXT, end_fact TEXT);
CREATE TABLE messages   (id INTEGER PRIMARY KEY, turn TEXT, at TEXT, source TEXT, text TEXT, delivered TEXT);
CREATE TABLE questions  (id TEXT PRIMARY KEY, ticket TEXT, turn TEXT, card TEXT, state TEXT, outcome TEXT,
                         basis TEXT, sure INTEGER, proposal TEXT, sources TEXT, answer TEXT,
                         took_proposal INTEGER, flag TEXT, decision TEXT, judge_turn TEXT,
                         created TEXT, shown TEXT, answered TEXT);
CREATE TABLE checks     (ticket TEXT, head TEXT, kind TEXT, result TEXT, command TEXT, seconds INTEGER,
                         cause TEXT, source TEXT, at TEXT);
CREATE TABLE findings   (ticket TEXT, round INTEGER, fingerprint TEXT, priority TEXT, source TEXT,
                         verdict TEXT, reason_code TEXT, fix_commit TEXT, came_back INTEGER);
CREATE TABLE operations (id TEXT PRIMARY KEY, idem TEXT UNIQUE, source TEXT, turn TEXT, ticket TEXT, verb TEXT,
                         args TEXT, mode TEXT, state TEXT, external_id TEXT, started TEXT, ended TEXT, result TEXT);
CREATE TABLE outcomes   (op TEXT REFERENCES operations(id), kind TEXT, check_at TEXT, checked_at TEXT,
                         state TEXT, detail TEXT);
CREATE TABLE human      (id INTEGER PRIMARY KEY, at TEXT, ticket TEXT, kind TEXT, object TEXT, value TEXT,
                         reason TEXT, seconds INTEGER);
CREATE TABLE memory_use (turn TEXT, file TEXT, version TEXT, applied INTEGER, how TEXT);
CREATE TABLE model_changes (id INTEGER PRIMARY KEY, at TEXT, agent TEXT, model TEXT, effort TEXT,
                         old_model TEXT, old_effort TEXT, source TEXT);
CREATE TABLE review_rows(round TEXT, comment_id INTEGER, reviewer TEXT, verdict TEXT, fix INTEGER,
                         verdict_final TEXT, fix_final INTEGER, PRIMARY KEY (round, comment_id));
CREATE TABLE events     (id INTEGER PRIMARY KEY, at TEXT, actor TEXT, ticket TEXT, kind TEXT, detail TEXT,
                         external_id TEXT UNIQUE);
CREATE TABLE audits     (id INTEGER PRIMARY KEY, week TEXT, target TEXT, mark TEXT, reason TEXT, at TEXT);
CREATE TABLE health     (at TEXT, test TEXT, ok INTEGER, detail TEXT);
```

Migration 2 adds `five_hour_resets` and `seven_day_resets` to `turns`. They hold the reset time of each limit reading. The limit gate ignores a reading after its reset time, so that an old reading never blocks new turns.

Migration 3 adds `title` to `tickets`. `vision take` writes the title that the queue of the tracker gives, so the status view keeps the title after the ticket leaves the queue. A ticket from before migration 3 has no title. The app then shows the title of the queue while the queue lists the ticket.

Migration 4 adds the table `notices (id, ticket, step, turn, gate, doc, issues, note, created, read)`. A gate qa with `notify = true` that passes writes one row in the transaction of the end fact, with its issues as JSON. One turn gives at most one row for each document. `read` holds the time when the person read it.

Migration 5 adds the table `journal (turn, ticket, path, hash, signature, head, written, filed_hash, filed, archived, archive_bytes, entry_sent, removed)`. One row holds the entry of a turn, what MemPalace holds of it, and what the object store holds. An entry waits for the index while `filed_hash` differs from `hash`. No row and no entry is ever deleted.

Migration 6 adds the table `sessions (path, project, session, main, hash, size, mtime, key, archived, archive_bytes, entry, summary_hash, summary_of, filed_hash, filed)`. One row holds one session file of Claude Code, with `path` relative to `sessions.projects`. It records what the archive holds of the file, the summary of a main file, and what MemPalace holds of that summary. A run reads a file again only when its size or its change time differs from its row. No row is ever deleted.

Migration 7 adds `path`, `line`, `title`, `turn`, and `track` to `findings`, and four tables:

- `gate_rounds (turn, ticket, step, gate, doc, round, head, passed, failing, critical, findings, at)`, with the key (turn, doc). One row holds one gate document of one turn, with its round among the turns of that kind of gate in the ticket.
- `finding_tracks (id, ticket, fingerprint, path, line, severity, title, first_round, last_round, came_back, noticed)`. One row follows one finding of the gate cr across rounds.
- `escape_checks (id, ticket, pr, reviewer, pass_turn, head, passed_at, looked, review_id, submitted, checked, comments, counted, escapes)`, unique on (ticket, reviewer, pass_turn).
- `escapes (id, check_id, path, line, priority, title, url)`.

A row of `findings` with the source `cr` is one finding of one round of a gate cr. A row with the source `escape` is one comment of a review bot that no finding of ours matched. No comment text is stored beyond its title, masked and cut to 200 characters.

Migration 8 adds indexes and changes no row: `turns (ticket, step)`, `turns (status)`, `events (ticket)`, `events (kind)`, `questions (ticket)`, `questions (state)`, `human (ticket)`, `operations (ticket)`, `operations (state)`, `spans (ticket)`, and `checks (ticket)`. A read of one ticket searches an index, so its cost does not grow with the finished tickets. A reader that makes several reads for one screen runs them in one read transaction, so each read sees the same state of the ledger.

The counts of a ticket also hold its kind, the members of a team step whose end facts held, the documents of the read verbs of each member, and the number of turns on Codex before a fallback. No column changes for them. A span with `waits_on` `skipped` marks a step that its `when` skipped. The questions of an ask step have the turn `ask:<KEY>:<step>:<n>`.

Each time is RFC 3339 in UTC. Each JSON value is stored as text. The ledger records the evidence from the first ticket, because Vision cannot read later what it did not record.

## 6. The tick

`vision` is one binary with no daemon. A timer of the system runs `vision tick` each minute. One tick does these steps in order:

1. Take the lock. If a different tick holds it, exit with code 0.
2. Write the heartbeat. If the `off` file exists, write the snapshot and exit.
3. Reconcile each operation in the state `executing` or `uncertain`.
4. Collect each turn that ended. Write its row, its result file, and the limit reading. Collect each job of the background that ended, and start the jobs that wait for a place (section 11.1). Then give the slots to the tickets that need one (section 11.3).
5. For each ticket, read the facts of its step and test the end fact.
6. Select the next action of each ticket: start a turn, ask for a job, run an operation, set a flag, or nothing.
7. If its mode and the limits allow the action, do it.
8. Write the snapshot. Release the lock.

A tick is safe to run two times. A crash at any line leaves a state that the next tick repairs.

## 7. Turns

### 7.1 Three kinds

| | Reader | Worker | Tester |
| --- | --- | --- | --- |
| Tools | Read, Grep, Glob, Task | Reader tools, and Edit, Write, Bash | The same as a worker |
| Working folder | The clone, or a folder of the turn | The clone | The clone |
| Can change | Nothing | Files and local commits in the clone | Only its run folder |
| Network | None | Only the local services of the clone | The site of its env |
| Credentials | None | None | The QA login of its env |

No session holds a credential for the tracker or the forge. No session can push, open a pull request, deploy, or change the tracker.

### 7.2 The session view

A session never reads `VISION_HOME`. Before a turn, the runner copies the inputs into the folder `.vision/` of the working folder. In a clone, `.git/info/exclude` lists `.vision`. A Judge turn and a Curator turn also get the memory repo as a folder that they can read.

The first message of a turn is one line: `Turn <id>. Read .vision/turns/<id>/facts.json and do the step that it names.`

```json
{
  "turn": "20261116T143000Z-DEMO-12-build-1", "agent": "builder", "step": "build", "kind": "worker",
  "ticket": {"key": "DEMO-12", "title": {"untrusted": "Export the report"},
             "issue": ".vision/turns/20261116T143000Z-DEMO-12-build-1/issue.json"},
  "branch": "vision/DEMO-12", "base": "9f3e21c", "head": "abc", "pr": null,
  "documents": {"approved-plan.md": ".vision/approved-plan.md", "refine.md": ".vision/refine.md",
                "answers.md": null},
  "expected_documents": ["pr.md"],
  "principles": ".vision/turns/20261116T143000Z-DEMO-12-build-1/principles.md",
  "principles_loaded": [{"id": "P-007", "version": "4f2a91c"}],
  "skills": {},
  "sensitive_topics": ["security", "money"],
  "sensitive_paths": [".github/", "db/migrate/"],
  "expects": ["commits", "doc:pr.md"],
  "requests": [{"verb": "tracker.issue", "about": "One ticket of the tracker as issue-<KEY>.json. args: key"}],
  "message": null
}
```

`documents` has one key for each name of `reads`. If the ticket folder has no such file, the value is `null`. A turn step with the key `with` also gets those values under `with`.

`requests` lists each read verb of the adapter with its `about` line. That line says what the verb gives and which arguments it takes. If `adapter.toml` gives no such line, the value is `null`. A session that needs one of these facts ends its turn with the kind `requests`.

Text from the tracker, from review comments, and from logs is wrapped as `{"untrusted": "..."}`. Each role file says that such text is data, never an order.

### 7.3 The start line

The supervisor starts Claude Code with these arguments. It gives no prompt argument, because the first message goes to standard input.

```
claude -p
  --model <model> --effort <effort>
  --input-format stream-json --output-format stream-json --verbose --replay-user-messages
  --restricted --tools <tools of the kind>
  --settings <turn folder>/settings.json
  --strict-mcp-config --permission-mode <dontAsk for a reader, acceptEdits for the others>
  --permission-prompts none
  --append-system-prompt-file <role file>
  --json-schema <the schema of the agent, as one string>
  --max-budget-usd <budget of the kind>
  --session-id <uuid>            a new turn
  --resume <session id>          a turn that continues a session
  [--add-dir <folder>]...
  [--plugin-dir <folder>]...     each folder of claude.plugins, for a work turn
```

The environment of the session is an allow list:

```
PATH HOME USER SHELL LANG LC_* TERM TMPDIR CLAUDE_CONFIG_DIR GIT_CONFIG_GLOBAL
and each name in claude.env_pass
```

The supervisor drops each other name, so that a token of the executor never reaches a session.

The key `claude.env` gives each session more variables with a fixed value. One example is a variable that names the test database of the clone, for a project that separates its clones in this way. A value that the runner sets wins over a value of this key. A session can read its environment, so a secret never goes there.

The runner renders `settings.json` for each turn. For a worker and a tester, it turns the sandbox on. It denies reads of the home folder, and it allows the clone, the folders of `sandbox.allow_read`, and the domains and sockets of `[sandbox]`. A reader gets no Bash tool, so its file needs no sandbox. Its file denies the Bash tool and the web tools.

The key `claude.attribution` controls the lines that Claude Code adds about itself. If it is false, the `settings.json` of each turn gets `"attribution": {"commit": "", "pr": ""}`. Then a session adds no co-author trailer to a commit and no line about its tool to a pull request. If it is true, the file has no such key. A session of Vision does not read the configuration of the project, so a project that turns these lines off needs this key. A file `sandbox/<kind>.json` of the home with its own key `attribution` keeps that key.

Claude Code adds the tool `StructuredOutput` by itself, because each turn has a result schema. The contract of 7.4 expects that tool next to the tools of the kind. A test with a real session showed this.

A worker and a tester get the file `worker.gitconfig` of the home as `GIT_CONFIG_GLOBAL`. It gives the name and the address of each commit, and it turns signing off. These come from `[git]` of the configuration. If `[git]` is empty, the name is the value of `user` with "(vision)", and the address names nobody. For a GitHub App bot, the name is `<app>[bot]`, and the address is `<id>+<app>[bot]@users.noreply.github.com`. GitHub then links each commit to the bot. `vision doctor` names the author in its line `git`. It also names an empty global ignore file and an empty global attributes file. Without these two keys, git looks for both files in the home folder of the person. The sandbox does not let git read them, so each git command prints a warning.

Claude Code asks for an approval for some commands also in the sandbox. Two examples are a git command with the option `-c` and a command with a variable in front of it. A turn has nobody who can approve, so Claude Code denies such a command. So the `settings.json` of a worker and of a tester also has a rule that allows each Bash command. The sandbox stays the limit, because that file refuses each run outside it.

A real test in `tests/real_claude.rs` shows both facts. Each of its calls asks for a run with no sandbox. The session still cannot read the home folder, write there, or reach an outside site. `agents/common.md` tells each agent the plain form of a commit, and the message after a failed fact `clean` or `commits` says it again.

In a git worktree, `.git` is a file. Git keeps the index, the objects, and the refs in the common git folder of the main checkout. The working folder of a worker or a tester can be a worktree. Then the rendered `settings.json` lists that common folder in `allowRead` and `allowWrite` of the sandbox. It lists the `config` and the `hooks` of that folder in `denyWrite`. Git runs what these two name outside the sandbox, so Claude Code keeps them closed in the `.git` folder of a working folder too. A normal clone and a file `sandbox/<kind>.json` of the home get no such entries. The probe of `tests/real_claude.rs` showed it in a worktree. `git status`, `git add`, and `git commit` work. A write to the work tree of the main checkout, to a hook, or to the configuration is denied.

Each entry of `claude.plugins` is a Claude Code plugin folder, which has `.claude-plugin/plugin.json`, or a folder of such folders. If the list names a folder, each work turn (a reader, a worker, or a tester of a step) gets one `--plugin-dir` for each folder, and its tool list ends with `Skill`. The contract of 7.4 names `Skill` too. The Judge and the Curator get no plugin.

A skill reads the files that it links, and a worker runs its scripts. So a turn can read each plugin folder and never write it. The runner gives the turn each folder, its real path, and the real path of each link in a skill folder that leads out of the plugin folder, as `--add-dir`. The runner also adds them to the read allow list of the sandbox of a worker and a tester. The `settings.json` of each work turn denies `Edit` and `Write` on each of these folders, such as `Edit(//<folder>/**)`, and the sandbox of a worker and a tester lists them in `denyWrite`. A file `sandbox/<kind>.json` of the home gets the same deny rules.

A folder of `claude.plugins` that does not exist or that holds no plugin refuses the turn. `vision doctor` names each plugin and the number of its skills in its line `plugins`, and fails such a folder. With an empty list, there is no such line.

A work turn of a ticket also gets the journal folder of the home as `--add-dir`, in the read allow list of its sandbox, and with the deny rules of a plugin folder. It is the one folder of the home that a session reads, and the session never writes it. The Judge, the Curator, and a turn of `curator` or `eval` do not get it. The facts file has `related`, the path of `related.md` in the session view, or null, and `journal`, the real path of the journal folder.

A reader turn of an agent whose runner is `codex` starts `codex exec --json --skip-git-repo-check --ignore-user-config -C <view> -c default_permissions="vision_reader" -c permissions.vision_reader.extends=":read-only" -c 'permissions.vision_reader.filesystem={...}' -m <model> -c model_reasoning_effort=<effort> --output-last-message <file> -`, with the prompt on standard input. The prompt holds the role file, the form of the result object, and the first line of a Claude turn. A resume uses `codex exec resume ... <thread> -` with the same keys. The effort `max` is `xhigh` for Codex. The configuration of the person for Codex stays out, and Codex uses its own login, whose folder Vision never reads.

The permission profile `vision_reader` reads the view folder, the extra folders of the turn, and `sandbox.allow_read`. It hides `VISION_HOME`, `~/.ssh`, `~/.aws`, `~/.config/gh`, `~/.claude`, `~/.gnupg`, `~/Library/Keychains`, `~/.netrc`, `~/.docker`, `~/.kube`, and `~/.codex`. Its commands have no network and write nothing. Before each turn on Codex, the supervisor runs `codex sandbox` with the same profile, with no model call: a command must read `/etc/hosts` and must not read a canary file with a random token in the turn folder. Any other result fails the turn with `codex cannot hide private folders on this machine`, and the fallback runs.

The profile `vision_reader` also reads each folder of `claude.plugins`, with its real path and the targets of its links. A more specific readable folder opens a plugin under a hidden folder such as `~/.claude`, and the rest of that folder stays hidden. A turn on Codex has no tool Skill, so its prompt has a section `# Skills` that names the file `SKILL.md` of each skill as `<plugin>:<skill>: <path>`, and the agent reads it.

The supervisor keeps the events of Codex in `codex.jsonl` and writes the same lines as a Claude turn into `stream.jsonl`: the thread as the session, each message, each command as a Bash call, and the tokens. So the collector, the journal, and the live view read one form. When Codex is missing, fails, reports a usage limit, a login problem, or a refusal, or gives no valid result after one resume, the same step or member runs again with Claude on its `fallback` model, in a fresh session, with the event `fallback`. With no `fallback`, the ticket gets the flag `needs-you`. `vision open` does not work on a turn on Codex, and a message to a running turn on Codex waits for the next turn.

The core serves two read verbs in the clone, with no adapter call and no token: `git.log { path, from?, to?, limit? }` gives the commits that touched a path or its lines `from` to `to`, with their messages, at most `limit` (20 by default, 200 at most), and `git.blame { path, from, to }`. A path that is absolute, starts with `-`, or holds `..` is refused. Each answer is masked and cut at 200 KB.

### 7.4 The supervisor

`vision turn-run <turn-id>` is a hidden command. The tick writes the `turns` row first, and then it starts the supervisor as a detached process in its own session and process group. The supervisor finds the ticket of the turn in that row. It lives as long as the turn. It does these things:

1. Write `turn.json` with the state `starting`, its pid, and the time.
2. Start `claude` with pipes for input and output. Write the first message as one line.
3. Copy each output line to `stream.jsonl`. Read each line as an event.
4. At the `init` event, compare the tools, the permission mode, and the MCP servers with the expected lists. On a difference, kill the group and end with the reason `contract`.
5. Each second, read `inbox/`. Write each waiting message to the session as one line. Then move the file to `inbox/sent/`.
6. At a `result` event, wait 2 seconds for a late message. If none came, close the input. The session then exits by itself.
7. At the time limit of the kind, kill the process group and end with the reason `timeout`.
8. Write `result.json` and the final `turn.json`: the state `ended`, the exit code, and the reason.

The reasons are `result`, `error`, `timeout`, `contract`, `killed`, and `crash`. A later tick reads `turn.json` and the stream. A supervisor holds a lock on `supervisor.lock` while it lives. If `turn.json` says `running` and no process holds that lock, the tick reads `turn.json` again. The supervisor writes its end before it frees the lock. So the second read finds the end of a supervisor that ended between the two reads. If `turn.json` still says `running`, the tick sets the reason `crash`. The tick does not trust a process id, because a different process can get the same id later.

A row of `turns` has one of four statuses: `running`, `done`, `retried`, and `failed`. `[time]` gives the time limit of a kind in minutes. A caller of the launch API can give a limit in seconds, as the Judge does with `caps.judge_minutes`.

One input line has this form:

```json
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"..."}]}}
```

The parser reads these events and ignores each other type:

```
{"type":"system","subtype":"init","session_id":"...","tools":["Read"],"permissionMode":"dontAsk","mcp_servers":[]}
{"type":"rate_limit_event","rate_limit_info":{"rateLimitType":"seven_day","utilization":0.75,"resetsAt":1794412800,"status":"allowed_warning"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"..."},{"type":"tool_use","name":"Read","input":{}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"...","content":"..."}]}}
{"type":"result","subtype":"success","is_error":false,"result":"...","structured_output":{},"session_id":"...",
 "total_cost_usd":0.0014,"num_turns":11,"permission_denials":[],
 "usage":{"input_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":0}}
```

### 7.5 A message into a running turn

The person can type into a running turn. A test on Claude Code 2.1.321 showed that the session takes a new input line at its next tool step.

- `vision tell KEY "TEXT"` writes one message file to the inbox of the running turn. If no turn runs, it stores the text for the next turn of the ticket.
- `vision tell` writes the row in `messages`, the event `told` of the person, and the message file in one transaction. If the message file cannot be written, the transaction rolls back. So no row and no line exist for a message that did not arrive. The timeline and `vision log` show each message once, at the time of its row. A message that Vision writes for the person, such as a correction, has its own event and no `told` event.
- `vision open KEY` attaches to the running turn. It prints the events as readable lines, and it sends each line that the person types. Ctrl-C leaves the view, and the turn continues.
- If no turn runs, `vision open KEY` pauses the ticket and starts `claude --resume <session id>` in the clone, in the terminal.

A message file is JSON: `{"text": "...", "source": "user", "at": "..."}`. Write it to a temporary name first, and then rename it. The ledger keeps each message in `messages`.

### 7.6 The result object

`notes` is an optional list. Each note has `kind` (problem, cause, fix, dead-end, command, or fact), `text`, and optional `files`, `commit`, and `cites`, the ids of the journal entries that the turn reused. A result with no notes stays valid.

Claude Code tests the output of a turn against a JSON schema. `agents/turn.schema.json` is the schema of each work turn:

```json
{"type": "object", "required": ["kind", "summary"], "additionalProperties": false, "properties": {
  "kind": {"enum": ["result", "questions", "requests"]},
  "summary": {"type": "string"},
  "documents": {"type": "array", "items": {"type": "object", "required": ["name", "content"],
    "properties": {"name": {"type": "string"}, "content": {"type": "string"}}}},
  "questions": {"type": "array", "items": {"type": "object",
    "required": ["text", "context", "options", "evidence", "kind"],
    "properties": {
      "text": {"type": "string"}, "context": {"type": "string"}, "example": {"type": "string"},
      "options": {"type": "array", "minItems": 2, "items": {"type": "object", "required": ["label", "means"],
        "properties": {"label": {"type": "string"}, "means": {"type": "string"}}}},
      "recommended": {"type": ["integer", "null"]}, "why": {"type": "string"},
      "evidence": {"type": "string"},
      "kind": {"enum": ["product", "architecture", "scope", "code"]},
      "area": {"type": "array", "items": {"type": "string"}},
      "topics": {"type": "array", "items": {"type": "string"}}}}},
  "requests": {"type": "array", "items": {"type": "object", "required": ["verb", "args"],
    "properties": {"verb": {"type": "string"}, "args": {"type": "object"}}}},
  "applied": {"type": "array", "items": {"type": "object", "required": ["id", "how"],
    "properties": {"id": {"type": "string"}, "how": {"type": "string"}}}},
  "candidates": {"type": "array", "items": {"type": "object", "required": ["when", "make_sure", "because"],
    "properties": {"when": {"type": "string"}, "make_sure": {"type": "string"}, "because": {"type": "string"}}}}}}
```

- `result`: the work of the step is done. The runner then tests the end fact itself.
- `questions`: the turn needs answers. The cards go to the Judge (section 8). After the answers, a new turn resumes the same session.
- `requests`: the turn needs more facts. The executor serves a closed list of read verbs, and a new turn resumes the session.
- `documents`: the files that the step makes, such as `plan.md`. The runner writes each one to the ticket folder. A step accepts only the names that it expects.

A turn that ends with no valid result object gets one resume with the message `continue`. The same rule applies to the reasons `error` and `crash`. It does not apply to `timeout`, `contract`, and `killed`. A second failure sets the flag `needs-you`.

The first input line of a turn is always the line of 7.2. A message for a resumed turn, such as the answers or `continue`, goes into the key `message` of its facts file.

## 8. Questions and the Judge

### 8.1 The card

An agent never asks in the text of its answer. It ends its turn with cards. Each card stops the ticket, so an agent asks a card only for a point that passes the card test of `agents/common.md`. The point changes the architecture or a behavior that a user sees. No source gives an answer: not the ticket, the code, the docs, the principles, the journal, the answers, or a command. Two or more choices are sound, and a wrong choice costs rework or harm. The agent decides each other point and writes the decision with its reason. A change that the ticket does not ask for is a follow-up, never a card. One step asks at most 3 cards. A card has the question, the reason for it, an example, two or more choices, a recommendation, and the evidence. `kind` is `product`, `architecture`, `scope`, or `code`. `topics` names each sensitive topic that the question touches. `area` names the part of the product in one to three nouns, and the decision file keeps it. `recommended` is the index of a choice, and the first choice has the index 0.

An agent asks only what the code, the files, the memory, and a command cannot answer.

### 8.2 The Judge

The Judge is a reader turn. It gets the cards, the ticket folder, and the memory. Its facts file also names `person`, the writer name of the person from the configuration key `user`. For each card it gives one outcome:

| Outcome | Basis | What the person sees |
| --- | --- | --- |
| `answer` | `decision` or `principle`: a recorded file covers the question, with the same conditions | Nothing now |
| `answer` | `inference`: the Judge is sure, from the code, the ticket, and the plan | Nothing now. The answer is in the list of `vision answers`. |
| `propose` | The Judge has a likely answer, but it is not sure | The card with the proposal and its evidence. One key accepts it. |
| `ask` | No answer, sources that disagree, or a topic of the always-ask list | The card with the recommendation of the agent |

`agents/judge.schema.json` is the schema of its output:

```json
{"type": "object", "required": ["answers"], "additionalProperties": false, "properties": {
  "answers": {"type": "array", "items": {"type": "object",
    "required": ["card", "outcome", "basis", "sure", "reason"],
    "properties": {
      "card": {"type": "string"},
      "outcome": {"enum": ["answer", "propose", "ask"]},
      "choice": {"type": ["integer", "null"]},
      "text": {"type": "string"},
      "basis": {"enum": ["decision", "principle", "inference", "none"]},
      "sources": {"type": "array", "items": {"type": "string"}},
      "sure": {"type": "boolean"},
      "topics": {"type": "array", "items": {"type": "string"}},
      "reason": {"type": "string"}}}}}}
```

An ask step (section 11.2) creates the cards of its documents as one batch. The Judge sees them as it sees the cards of a turn. When each card has an answer, the answers go to `answers.md`, and the ticket moves to `changed` or to `same`. A member of a team never asks a card: it writes the open questions that pass the card test into its findings, with a proposal for each other open point, and the analyst decides or asks.

### 8.3 The rules in code

The runner applies these rules to the output of the Judge. A rule in code always wins over the output.

1. If a topic of the card or of the answer is in `judge.always_ask`, the outcome is `ask`.
2. An `answer` with the basis `decision` or `principle` must name at least one source. Each source must exist and be active. If not, the outcome becomes `propose`.
3. If `sure` is true and `judge.answer_when_sure` is true, an `answer` with the basis `inference` stays an answer. If not, the outcome becomes `propose`.
4. An `answer` or a `propose` must name a choice or a text. If not, the outcome becomes `ask`.
5. If the Judge gives no outcome in `caps.judge_minutes`, the card goes to the person.

Each answer becomes one decision file. An answer of the Judge has the writer `judge`, its basis, its sources, and its reason. The asking session gets the answer with a note that names the writer and the sources.

### 8.4 The person

```
vision needs [--json] [--read ID]    the cards, the approvals, and the notices that wait, one at a time
vision answer ID --choice N | --text "T" [--why "W"] [--when "C"]
vision answer ID --needless code|ticket|memory|no-matter
vision answer --file FILE            the answers of several cards: each one is recorded, or none
vision answers [--days N]            the answers that the Judge gave alone, newest first
vision correct ID "RIGHT ANSWER" [--why "W"]
```

- `needs` shows one card, reads one key or one line, and then asks for two short lines: why, and where the decision applies. Both can stay empty.
- `needs` also lists each notice that waits, with its issues. `needs --read ID` marks the notice ID, such as `N-3`, as read. A notice that was read before stays as it is.
- `--choice N` counts from 1, as the card shows the choices to the person. The index in the output of the Judge counts from 0.
- `correct` writes a decision of the person that replaces the answer of the Judge. The old file gets the status `superseded`. If the ticket still runs, its next turn gets the correction as a message.
- A decision with no reason is weaker. The Judge uses it only for a question with the same words and the same area.
- `answer` writes the `human` row of an answer in the same ledger transaction that sets the question to `recording`. The commit of the decision file comes after it. If the decision file cannot be written, the row goes again. So a crash after that commit never loses the row, and the next tick ends the record. `correct` writes its mark and its `human` row in one transaction too.
- `answer --file FILE` reads a JSON object with one entry for each card id, such as `{"Q-0001": {"choice": 2, "why": "W", "when": "C"}, "Q-0002": {"text": "T"}, "Q-0003": {"needless": "code"}}`. `choice` counts from 1. A needless entry takes no `why` and no `when`.
- `answer --file` records each answer as `answer` does for one card: a decision file, the row of the card, one `human` row, and the event. It records each answer of the file, or none. If one card no longer waits or one answer is not valid, nothing is recorded, and the error names that card and the reason.
- `answer --file` starts each answer in one ledger transaction, and the decision files go to the memory in one commit. If the commit fails, each card waits for the person again. A second transaction then ends each record, as for one card. If the second transaction fails, each answer is still saved. The memory has its decision file, its row waits in `recording`, and a tick ends the record after two minutes. A needless mark is final at once. The error starts with `Vision saved each answer of the set` and names the decision files.
- `needs --json` gives the asking id of each card in `turn`: the id of the asking turn, or the batch id of an ask step.

## 9. The memory

### 9.1 Files

The memory is a private git repo at `$VISION_HOME/memory`. Each change is one commit. The author of the commit is the writer: the person, `judge`, `curator`, or `import`. The first commit of `vision init` has the author `vision`.

A decision file:

```markdown
---
id: D-0142
at: 2026-11-16T14:32:00Z
by: user                 # user | judge | import
basis: answer            # answer | decision | principle | inference
ticket: DEMO-12
step: plan
asked_by: builder
kind: product            # product | architecture | scope | code
area: [reports, exports]
status: active           # active | superseded | retired
sources: []              # for an answer of the Judge: the ids that it applied
supersedes: null
---
## Question
Must the Export button make a CSV file or an XLSX file?
Choices: CSV | XLSX

## Answer
CSV

## Why
The finance team imports CSV into their tool.

## Applies when
A report export for an internal user.
```

A principle file:

```markdown
---
id: P-007
agent: judge             # the agent that loads it, or "all"
kind: product
area: [exports]
status: active           # candidate | active | retired
by: user                 # the person who accepted or wrote it
evidence: [D-0142, D-0167]
uses: {used: 6, agreed: 5, corrected: 0}
---
## Rule
When an internal user exports a report, make sure that the file is CSV.

## Because
The finance team imports CSV into their tool, and no export so far needed formulas.

## Not when
A customer asks for a formatted file.
```

The front matter is a small subset of YAML: one key on each line, with a string, a number, `null`, a flat list, or a flat map. The parser keeps unknown keys. `kind` can be `null`. A retired file gets the key `retired_at` and a section `## Retired` with the reason. An imported decision keeps a stable hash in `import_hash`, so that a second import adds nothing.

### 9.2 Commands

```
vision memory [QUERY] [--kind K] [--area A] [--status S]   list and search
vision memory show ID              one file, with its evidence and its uses
vision memory edit ID              open it in $EDITOR, lint it, and commit
vision memory teach AGENT "TEXT"   write a principle directly. It is active at once.
vision memory retire ID "WHY"      retire it. The file stays, and no turn loads it.
vision memory lint                 test the format of each file
vision memory import FILE          read past answers from a JSONL file as decisions
vision memory review [--accept N|all [--answer TEXT] | --reject N|all [--why TEXT]]
                                   accept, edit, or reject each change of a proposal
vision memory log                  the history
vision memory revert COMMIT        make a commit that undoes one commit
```

The person can also edit the files with any tool. The next tick reads the change.

`--answer TEXT` goes with `--accept N` for a question of the Curator. The command refuses it for any other change, and for more than one question at a time. The next run of the Curator reads the answer. Some questions name exactly one decision as evidence, and the section `## Why` of that decision is empty. For such a question, the answer also becomes that section, as a commit of the person in the memory repo. The review says so before the person answers, and the interactive review does the same.

### 9.3 The life of a principle

1. Candidate. The Curator proposes it from at least 2 decisions of 2 different tickets.
2. Active. The person accepts it in `vision memory review`. A principle that the person writes or teaches is active at once.
3. Used. Each turn records the principles that it got. Each result lists the principles that it applied, and how.
4. Retired. The person retires it, or the Curator proposes that after a correction.

Only people teach the memory. A decision with the writer `judge` never counts as evidence for a principle. It never outranks a decision of the person. An imported decision is a past answer of the person, so it counts as evidence.

### 9.4 The Curator

The Curator is a reader turn. It runs after each merged ticket and one time each week. `vision curate` starts it by hand. For a merged pull request, the pipeline records the event `ticket-merged`, and a later tick starts the turn. Two Curator turns never run at the same time. The Curator reads these signals from the ledger and the memory:

- The new decisions, and the decisions with no reason.
- Each proposal of the Judge that the person changed, and each correction.
- The difference between the plan that the agent wrote (`plan.agent.md`) and the plan that the person approved (`approved-plan.md`). The person can edit `plan.md` before the approval, so the runner keeps the first text of the agent in `plan.agent.md`.
- The cards that the person marked as needless.
- The review comments of people on the pull requests, and the verdict of each one.
- Two active files that disagree.
- Each change of an earlier proposal that the person accepted or rejected, with the reason of the person.

The ledger keeps the last signal as `human` rows of the kinds `proposal-accept` and `proposal-reject`.

Its output is a list of changes: a new candidate, an edit, a merge of two files, a retirement, or a question for the person. `agents/curator.schema.json` is the schema. Each change has an `op` and a `reason`:

- `add`: `path` is `principles/NEW-<n>.md`, and `content` is the full file. The runner gives the final id.
- `edit`: `path` names an existing file, and `content` is its full new text.
- `merge`: `path` and `content` give the file that stays, and `removes` lists the paths that it replaces. The runner retires each replaced file. It deletes no file, because a later file can name it as a source.
- `retire`: `target` is the id of the file. The runner sets its status to `retired`.
- `question`: `question` is the text for the person.

For a new principle, the runner sets `id`, `status` to `candidate`, and `by` to `curator`. A real session gives a looser text than the format asks for. The runner repairs three faults before it tests a change:

- An evidence item with text after its id keeps only the id.
- A new principle with no `agent` key gets `agent: all`. A new principle with no `kind` key gets `kind: null`.
- A file text with no last line end gets one.

The runner drops each change that fails the test, and `vision curate` prints the reason.

`evidence` lists the decision ids that support a change. The runner writes the changes to a branch `proposal/<date>-<n>` of the memory repo. Three rules protect the memory:

- No line is lost. Each line that a proposal removes or rewrites goes to `ARCHIVE.md` word for word, with the commit and the reason. The runner tests this in code, and it rejects a proposal that loses a line.
- A replay shows the effect. The Judge answers the last `curator.replay` questions that the person answered, with the proposed memory. The review shows how many answers now agree with the person and how many no longer agree. The replay hides the decision file of each answer that it replays, because the Judge has to find that answer again without it. In the proposed memory, each candidate of the proposal counts as active, so that the replay shows its effect.
- A person decides. Only a change that the person accepts reaches the main branch.

The replays run over several ticks, so that no tick waits for one. The tick that applies the result of a Curator turn builds the proposal branch and starts the first replay, with the proposed memory. A later tick reads it and starts the replay with the memory of today. A stored result of the same memory, model, and cards serves in its place. A tick kills a replay turn that runs longer than `caps.judge_minutes`, and that replay counts as timed out.

The proposal record keeps the state of the replays after each step. The start of a replay turn writes its claim in the same transaction as its turn row. So a crash at any point leaves a state that the next tick repairs, with no second proposal and no lost replay. The next tick also adopts a proposal branch that a crash left with no record.

While the replays run, `vision memory review` says so in one line, and the person can still accept or reject each change. `vision needs` marks such a proposal with the text `(its replays still run)`. `vision curate` waits for the replays for at most two times `caps.judge_minutes` plus two minutes. It holds the lock of the tick only for the short step after a replay turn ended.

## 10. Models

Each agent has a model and an effort level. The person sets them, and Vision never changes them by itself.

```
vision model                         each agent: model, effort, source, and the recommendation
vision model set AGENT MODEL [--effort E] [--runner claude|codex]
vision model reset AGENT             go back to the default of Vision
vision model recommend [AGENT]       the evidence for each model that the agent used
vision model eval AGENT --models A,B [--n N]    replay past work with each model
```

- `set` writes `[agents.<agent>]` with `pinned_at` to the configuration. The value stays until the person changes it. A new version of Vision never replaces a pinned value.
- `MODEL` is a full model id or one of the short names `fable`, `opus`, `sonnet`, `haiku`. Effort is `low`, `medium`, `high`, `xhigh`, or `max`.
- Each turn row records the model and the effort that it used. Each change is a row in `model_changes`.
- `vision model` prints a table that fits 100 columns: the agent, the model, the effort, and the source. Under each row, one line gives the reason of the default, and one line gives the recommendation. For a pinned agent, the first line names the default that the pin replaced.

The defaults, with the reason for each one:

| Agent | Model | Effort | Reason |
| --- | --- | --- | --- |
| scout | `claude-fable-5-2` | high | A wrong reading of the ticket costs the full build. Its turns are short. |
| builder | `claude-opus-5-5` | xhigh | It writes the code and uses the most tokens. This model costs half as much as the top model. |
| reviewer | `claude-opus-5-5` | max | The review is the last gate before a person reads the code. |
| tester | `claude-opus-5-5` | high | A false pass sends a bug to a person. |
| shepherd | `claude-opus-5-5` | high | It gives verdicts on review comments and fixes the valid ones. |
| merger | `claude-opus-5-5` | high | It resolves the merge conflicts of a deploy. A wrong merge breaks an env that the whole team uses. |
| judge | `claude-fable-5-2` | high | It can answer alone, so a wrong answer costs the most. Its turns are short. |
| curator | `claude-fable-5-2` | high | It runs seldom, and its proposals change each later answer. |
| repro | `claude-opus-5-5` | high | It must show the bug with a failing spec. A false reproduction sends the fix the wrong way. |
| cause | `gpt-6.1-sol` on Codex | high | A second model finds the cause on its own. Its fallback is `claude-opus-5-5` high. |
| history | `claude-sonnet-5-5` | medium | It summarizes why the code is as it is. A stronger model adds little. |
| points | `claude-opus-5-5` | high | It tests each point of a feature against the code. |
| scope | `claude-opus-5-5` | high | It finds each place that a chore touches. |
| analyst | `claude-opus-5-5` | xhigh | Its plan decides the build, so it checks each claim against the code. |
| critic | `gpt-6.1-sol` on Codex | xhigh | A second model reads a wide plan. Its fallback is `claude-opus-5-5` high. |
| critic-narrow | `gpt-6.1-sol` on Codex | high | A narrow plan needs a lighter read. Its fallback is `claude-opus-5-5` high. |

An adapter can define more agents in `[agents.<name>]` of its `adapter.toml` (section 11.2). `vision model` lists them after the agents of Vision, in the order of their names, with the source `adapter`. Each JSON row has `defined_by` (`vision` or `adapter`), `default_model`, and `default_effort`. `set` pins an agent of the adapter as it pins an agent of Vision, and `reset` goes back to the values of the adapter. When the adapter does not load, `vision model` lists the agents of Vision and says why on standard error.

`--runner codex` moves a reader agent to Codex. Without `--runner`, a model whose name starts with `claude-` runs on Claude, and another model keeps the runner of the agent.

`recommend` reads the ledger. For each agent and each model with at least 10 turns, it prints these numbers:

- The number of turns, and the cost of one turn at list price.
- The part of the turns whose end fact held at the first try.
- For the Judge: the corrections of the person, and the proposals that the person changed.
- For the Builder: the review findings, the red CI rounds, and the QA bugs for each ticket.

These numbers come from fixed places of the ledger:

- `turns.end_fact` is `held` or `failed:<fact>`. The pipeline writes it.
- A correction is a row of `human` with the kind `correct` and the question id as its object.
- A changed proposal is a question with the outcome `propose` and `took_proposal` set to 0.
- A QA bug is a row of `findings` with the source `qa`.
- A red CI round is a row of `checks` with the kind `ci` and the result `red`.

The recommendation is the cheapest model whose quality numbers are not worse than the best model by more than 5 points. With fewer than 10 turns for a second model, the command says that the default stays. `eval` gives that evidence on request: it replays past work with each model and prints the same table. The Judge replay uses the questions that the person answered.

## 11. The pipeline and the adapter

The core knows eight kinds of step: a turn, a job, a human action, a wait, an ask, a check, an act, and the end. The steps of a project are data in `adapter.toml`. The core has no word that names a project, a tracker, or a forge.

### 11.1 The adapter folder

```
adapters/<name>/
  adapter.toml        the steps, the verbs, the jobs, the envs
  bin/observe         print the facts of the queue or of one ticket
  bin/act             run one verb
  bin/job             run one job, such as the proof
  roles/              role files that replace agents/<role>.md
```

Vision looks for the adapter of `project` first in `$VISION_HOME/adapters/`, and then in the folder `adapters/` next to the binary or in the repo. `adapter.dir` in the configuration names a different folder.

An `adapter.toml` can name a different folder for its scripts with the key `scripts`. A private adapter then holds only its own step list, and it uses the scripts of a public adapter.

The value `builtin:<name>` names an adapter that is part of the binary, such as `builtin:jira-github`. The executor then starts the binary as `vision adapter-script <name> <observe|act|job>`, a hidden command. The call gets the same input, environment, time limit, and exit codes as a script of `bin/`. A private adapter with `scripts = "builtin:jira-github"` holds only its `adapter.toml`.

Each script reads one JSON object from standard input and prints one JSON object. Exit code 0 means done. Exit code 2 means that the script refused the call. Each other code means a failure. Only the executor calls a script. The call object always has `config`, the `[adapter]` table of the configuration. It also always has `base_ref`, the `base` of `adapter.toml`, such as `origin/main`. So the base branch has one place. A call of `act` also has `git`, with `name` and `email`: the identity of the commits of a session, for a verb that makes a commit, such as `deploy.merge`. git also needs that identity to start a merge, and it cannot guess one on a host whose name has no domain.

The environment of a script has only `PATH`, `HOME`, `LANG`, and `TMPDIR`. `bin/observe` and `bin/act` also get each secret of `[secrets]` as an environment name. `bin/job` gets no secret, because a job can run code that an agent wrote. A job that only reads the tracker or the forge sets `secrets = true`. The executor masks each secret value in an answer before it stores the answer.

A job with `env = "full"` gets the environment of the tick instead. A variable with the name or the value of a secret stays out of it, unless the job has `secrets = true`. Only a job with `command` can have `env = "full"`. A turn never gets this environment. A job with `command` gets its table of `adapter.toml` in `table`: `command`, `doc`, `env`, `secrets`, and `timeout`. The built-in adapter runs the command (section 11.5). A script of `bin/job` gets the same table, and it decides what to do.

```
observe  {"what": "queue", "config": {}}
      -> {"tickets": [{"key": "DEMO-12", "title": "...", "status": "To Do", "eligible": true, "reason": ""},
                      {"key": "DEMO-14", "title": "...", "status": "In Review", "eligible": false,
                       "reason": "...", "branch": "feature/demo-14", "pr": 9}]}
observe  {"what": "ticket", "key": "DEMO-12", "branch": "vision/DEMO-12", "pr": 7, "clone": "/path",
          "seen": ["inline:501:501"], "config": {}}
      -> {"issue": {}, "tracker_status": "In Progress",
          "pr": {"number": 7, "draft": true, "head": "abc", "merged": false, "closed": false, "conflict": false,
                 "approved_by": [], "changes_requested": false, "comments_waiting": 0},
          "ci": {"head": "abc", "state": "green"}, "envs": {"staging": {"head": "abc"}}}
act      {"verb": "git.push", "op": "op-id", "key": "DEMO-12", "clone": "/path", "branch": "vision/DEMO-12",
          "head": "abc", "pr": 7, "args": {}, "config": {}}
      -> {"state": "confirmed", "external_id": "...", "detail": "...", "set": {"pr": 7}}
act      {"verb": "git.push", "op": "op-id", "reconcile": true, "external_id": null,
          "started": "2026-11-16T14:30:00Z", ...}
      -> the same object. The script only reads the outside state, and it never acts again.
act      {"verb": "tracker.issue", "read": true, "key": "DEMO-12", "args": {}, ...}
      -> {"detail": "...", "documents": [{"name": "links.json", "content": "..."}]}
job      {"job": "proof", "key": "DEMO-12", "clone": "/path", "branch": "vision/DEMO-12", "base": "9f3e21c",
          "head": "abc", "pr": 7, "args": {}, "config": {}}
      -> {"ok": true, "detail": "...", "documents": [{"name": "ci-log.txt", "content": "..."}],
          "checks": [{"kind": "spec", "result": "pass", "command": "...", "seconds": 12, "cause": ""}]}
```

- The call object always has `config`: the `[adapter]` table of the configuration, with the `adapter` keys of the clone of the ticket over it.
- `state` of an `act` answer is `confirmed`, `failed`, or `uncertain`.
- `set` can give new values for the ticket row: `pr`, `head`.
- `documents` of a job go to the ticket folder. The documents of an `act` answer with the state `confirmed` go there too. A later turn can read them.
- A `failed` answer can have `"conflict": true`: the verb failed on a merge conflict that an agent can resolve. Its documents go to the ticket folder too. A wait step with `conflict` then sends the ticket to the step that resolves it. The runner masks each secret value in them. A script cannot write a document that the person or the pipeline owns, such as `plan.md`, `issue.json`, or `comments-seen.json`.
- A reconcile call also has `external_id` and `started`. `external_id` is the id that the first answer stored, or `null`. `started` is the time of the first call.
- A read verb changes nothing outside. A turn can ask for it, and the documents of its answer go to that turn.
- If `act` ends with a different exit code, a signal, a time limit, or bad output, the operation becomes `uncertain`. The outside state is unknown then.
- The `cause` of a check is `code`, `infra`, `flaky`, `env`, `not ours`, or `unknown`.
- `ci.state` is `green` or `red` for a run that ended. Each other word means that no run ended at that commit. For each pair of commit and result, the runner writes one row in `checks` with the kind `ci`.
- Each run of a job also writes one row in `checks` with the kind `job` and the source `job:<name>`. Its result is `pass`, `fail`, or `error`. The end fact `job:<name>` reads that row at the last commit.
- A job with arguments writes the command `<name> <hash>` in that row. The hash is the first 16 hex characters of the SHA-256 of its arguments as JSON with sorted keys. An end fact with arguments reads only a row with the same command. So the job runs again after its arguments change at the same commit.

A job runs in the background, because a job such as the proof can run for many minutes. A pass holds the lock of the tick, and each action of the person and each other ticket waits for that lock. So a pass never waits for a job:

1. A pass asks for a run. The run gets the folder `jobs/<run-id>/` with `run.json`. That file holds the call of `bin/job` with no secret, and the phase `queued`.
2. The pass starts `vision job-run <run-id>` as a detached process for each queued run that has a place. At most `caps.jobs_at_once` runs run at a time, and one run for each ticket. Each ticket has its own clone, so two runs never share a work tree.
3. The runner holds the lock `runner.lock` while it lives. It reads the secrets, runs the script, and writes the masked answer to `answer.json`.
4. A later pass collects the run. It stores the checks, the documents, and the row of the kind `job`, as above. It stores one event whose external id is the mark of the run, and it removes the folder.

The mark names who waits for the outcome. The end facts of a turn have one mark for each turn, job, and set of arguments. A `before` entry has one mark for each visit and entry. A job entry and a job step have one mark for each commit. While a run of a mark waits or runs, the ticket waits too, and the app and `vision status` name the job. The engine starts a pass when a run ends.

`vision stop KEY` and `vision off` stop the runs. A runner kills the process group of its script and stores no outcome, so the job runs again when the ticket goes on. A runner that dies without an answer, as when the Mac restarts, runs one more time. A second death is a failure of the job with the cause `infra`.

### 11.2 The steps

```toml
name = "demo"
branch = "vision/{key}"       # the branch of a ticket
base = "origin/main"          # the ref that a new branch starts from
# scripts = "../other-adapter"   optional: the folder whose bin/ answers the calls, or builtin:<name>

[[step]]
id = "refine"
kind = "turn"                 # turn | job | human | wait | ask | check | act | end
agent = "scout"
turn = "reader"               # reader | worker | tester
reads = ["issue.json"]        # documents of the ticket folder that the session gets
expects = ["doc:refine.md", "doc:description.md"]
after = ["tracker.describe"]  # verbs that run when each end fact holds
next = "plan"

[[step]]
id = "plan-ready"
kind = "human"
action = "approve-plan"
next = "build"

[[step]]
id = "build"
kind = "turn"
agent = "builder"
turn = "worker"
reads = ["approved-plan.md", "refine.md", "answers.md"]
expects = ["commits", "clean", "doc:pr.md", { job = "proof", args = { plan = "{doc:approved-plan.md}" } }]
next = "review"               # the commits stay in the clone until open-pr

[[step]]
id = "review"
kind = "turn"
agent = "reviewer"
turn = "worker"
before = ["job:second-review"]    # jobs that run first. Their documents become inputs.
reads = ["approved-plan.md", "second-review.md"]
expects = ["clean", "doc:review.md", "job:proof"]
cap = "review_rounds"         # a key of [caps]. It counts each entry into the step.
next = "open-pr"

[[step]]
id = "open-pr"
kind = "act"                  # no turn: the verbs of run run in order
run = ["git.push", { verb = "pr.open", args = { text = "{doc:pr.md}" } }]
next = "ci"

[[step]]
id = "ci"
kind = "wait"
start = ["ci.run"]            # verbs that run at the entry
watch = "ci.state"            # a path into the observe answer
on = { green = "ready", red = "fix-ci" }
wait = ["none"]               # the values that mean "not yet". Each other value sets needs-you.
limit = 3600                  # seconds that one visit can watch before it sets needs-you
every = 60                    # seconds between two observe calls

[[step]]
id = "done"
kind = "end"

[verbs."git.push"]
mode = "auto"                 # the mode at the start. [modes] of the configuration wins. The default is ask.
leaves = true                 # text or code leaves the machine, so the gates run first
timeout = 120                 # seconds for one call. The default is 300.

[verbs."tracker.move"]
once = true                   # one run for each ticket and its arguments, at any commit. The default is false.

[verbs."tracker.issue"]
read = true                   # it changes nothing outside, so it has no mode. A turn can ask for it.
about = "One ticket of the tracker as issue-<KEY>.json. args: key"   # one line for the facts file

[jobs.proof]
timeout = 1800

[jobs.comments]
secrets = true                # this job only reads the forge. Never set it for a job that runs project code.

[jobs.regression]
command = "~/bin/check-regression"   # a shell command of the person
doc = "regression.md"                # the document that gets its standard output
env = "plain"                        # or "full": the environment of the tick
```

An entry of `after`, `before`, or `start` is a name, or a table with arguments:

```toml
after = ["git.push", { verb = "deploy", args = { env = "staging" } }]
before = [{ job = "app-start", args = { env = "local" } }]
with = { env = "staging" }    # a turn step: these values go into the facts file under "with"
findings = "review"           # a turn step: each finding of its documents becomes a row with this source
on = { findings = "fix-qa", none = "ready" }   # a turn step with findings: it replaces next
rounds = 3                    # a turn step: the turns of one visit for failed end facts. The default is 2.
fail = { qa = "fix-qa" }      # a turn step: a gate that fails on a blocking item goes to this step at once
extra_rounds = 2              # a turn step with a gate: more turns while the loop converges (section 11.3)
from = "09:00"                # a wait step: the visit starts at the first such local time after its entry
```

A text argument of an entry can hold `{doc:NAME}`. The runner puts the text of that document of the ticket folder in its place. If the document does not exist, the ticket gets the flag `needs-you`, and the verb does not run.

An entry of `expects` can also be a table: `{ job = "<name>", args = {...} }`. Its arguments follow the same `{doc:NAME}` rule, with one difference. A missing document gives an empty text and no flag, and the job decides what an empty text means.

A turn step with the key `findings` can have `on` in the place of `next`. This `on` has exactly the keys `findings` and `none`. The loader refuses `on` without `findings`, `on` together with `next`, and a target that is not a step.

A turn step can have `rounds`: the number of turns that one visit gets for failed end facts, from 1 to 20. The default is 2, the first turn and one resume. `rounds = 1` allows no resume.

A turn step with a gate can have `fail`, a table from a gate to a step. A key is the kind of the gate, such as `qa`, or its end fact, such as `gate:qa:qa.md`, for a step with two gates of one kind. The loader refuses a key that names no gate of `expects`, a key that names an end fact of another kind, such as `commits` or `job:proof`, and a target that is not a step.

A turn step with a gate can have `extra_rounds`, from 0 to 10, with a default of 0. `rounds + extra_rounds` is at most 20. The loader refuses `extra_rounds` on a step with no gate.

A job can have `env = "session"`: the environment of a work session. It holds the names of section 7.3, the names of `claude.env_pass`, and the values of `claude.env` with the `env` of the clone of the ticket over them, and no variable that has the name or the value of a secret. Such a job cannot have `secrets = true`.

A job that a built-in adapter knows by its name can have keys of its own, such as the commands of the precheck (section 11.5). The loader tests them. The call of `bin/job` gets them in `table`, with `timeout`.

An adapter can define its own agents:

```toml
[agents.cr-codex]
role = "roles/cr-codex.md"    # in the adapter folder; the default is roles/<name>.md
model = "gpt-6.1-sol"         # required
effort = "high"               # the default is high
runner = "codex"              # the default is claude
fallback = { model = "claude-opus-5-5", effort = "high" }
reason = "A second model reads the diff on its own."
```

A step and a team member name an agent of Vision or an agent of `[agents]`. The role file of an agent of the adapter comes after `agents/common.md`, as the role file of each work agent does. The loader refuses these:

- an agent that is neither an agent of Vision nor in `[agents]`
- a name of `[agents]` that is an agent of Vision: its role file changes with `roles/<name>.md`, and its model with `vision model set`
- a role path that is absolute or holds `..`, and a role file that exists neither in the adapter folder nor in the scripts folder
- a `fallback` without `runner = "codex"`
- an agent on Codex that a step makes a worker or a tester, or that no step makes a reader

A turn step can have `on = { commits = "<step>", none = "<step>" }` in the place of `next`, with exactly these two keys. The loader refuses it together with `next`, together with the key `findings` in `on`, on a step with `team`, and with a target that is not a step.

The adapter can name kinds and a width:

```toml
[kinds]
map = { Bug = "bug", Story = "feature", Task = "chore" }   # the issue type -> the kind
default = "feature"

[width]
doc = "plan.md"               # the document whose section ## Files lists the changed files
files = 3                     # this many files or more is wide
paths = ["db/**"]             # a listed file that matches is wide
```

The kind comes from `issue.type` of the first `observe` after `take`: an exact match, then a match in any case, then `default`. `vision kind KEY NAME` changes it. A plan is wide when its section `## Files` lists `files` files or more, or a file that matches `paths`. Else it is narrow.

A step can have `when`, such as `when = ["bug"]` or `when = ["wide"]`. Its words are kinds, `wide`, and `narrow`. Kinds are ORed, widths are ORed, and the two groups are ANDed. A step whose `when` does not hold is skipped in the same tick, with the event `skipped <step>: <why>`. A width word whose document does not exist yet sets the flag `needs-you`. The end step and a wait step have no `when`.

The adapter can name the rules of a continue, after its steps:

```toml
[[resume]]
when = { "pr.merged" = true }
step = "done"

[[resume]]
when = { "review.state" = ["comments", "changes"] }
step = "comments"
back = true
```

The `when` of a rule differs from the `when` of a step. It maps a path of the answer of `observe`, the same paths as `watch`, to a value or a list of values. If each path gives one of its values, the rule holds. A value is a string, true, or false. Write a number as a string, such as "0". A missing path and a null give `none`. The loader refuses a step that the adapter does not have, a missing or empty `when`, an empty list, and another type of value. A rule can name the end step. The rules run only at a continue (section 11.3).

A rule can have `back = true`, and the default is false. Such a rule can also suggest a step before the stored step, a move back. One example is a move from `ready-to-merge` to `comments` after a late comment. One path of `when` comes from Vision and not from `observe`: `head.state`. It compares the head of the open pull request with the branch of the clone, after the fetch of a continue. Its values are none, same, forward, ahead, behind, apart, dirty, elsewhere, and unread. None means that no pull request is open. Forward means that the pull request has later commits, such as a push by hand. The loader also refuses another value of `head.state` and a value of `back` that is not true or false. It refuses a key other than `when`, `step`, and `back` too.

A turn step can have `team` in the place of `agent`, `turn`, and `expects`:

```toml
team = [
  { agent = "repro",   turn = "worker", expects = ["doc:findings-repro.md", "clean"], when = ["bug"] },
  { agent = "cause",   turn = "reader", expects = ["doc:findings-cause.md"], when = ["bug"] },
  { agent = "history", turn = "reader", expects = ["doc:findings-history.md"], when = ["bug"] },
]
```

Each member whose `when` holds starts in the same tick, in its own session, and has its own `rounds`. A team has at most one worker or tester, and a member never reads a document that another member expects. A member that ends with `questions` is refused, and it writes them under `## Open questions` in its findings. When each member is done, the `after` verbs run and the ticket moves to `next`. A member that uses its rounds sets the flag `needs-you`. `vision retry` starts again only the members whose end facts did not hold. A new entry into the step gives each member `answers.md`.

An ask step creates the cards of its documents as one batch (section 8.2):

```toml
[[step]]
id = "questions"
kind = "ask"
docs = ["questions-analyst.json", "questions-critic.json"]
on = { changed = "amend", same = "reproduced" }
```

A missing document counts as no card. A document that is not an array of cards sets the flag `needs-you`. A card whose question repeats an earlier one is dropped. The ticket moves to `changed` when an answer is not the recommended choice, when a card has no recommendation, or when the person wrote an answer. Else, and when there is no card, it moves to `same`.

An act step runs verbs and no turn. Its keys are `run`, `next`, `cap`, and `when`. `run` names one verb or more, each in the form of an entry of `after`. The loader refuses each other key, a missing or empty `run`, a job, a verb that is not in `[verbs]`, a read verb, and a `next` that is not a step. The shipped adapters use the act step `open-pr` to push the branch and open the pull request after the review, so that nothing leaves the machine before the review holds.

A check step reads one document:

```toml
[[step]]
id = "reproduced"
kind = "check"
doc = "plan.md"
line = "Reproduced: yes"
stop_line = "Decision: drop"
when = ["bug"]
next = "plan-ready"
fail = "investigate"
cap = 2
```

A line equal to `stop_line` sets the flag `needs-you`: the person chose to drop the ticket, and runs `vision stop KEY`. A line equal to `line` moves the ticket to `next`. Else the ticket moves to `fail`, which counts against `cap`.

A wait step can have `from`, a time of day such as `"09:00"` in the local time of the machine.

A wait step can have `interrupt`, such as `interrupt = { "review.state" = { comments = "comments", changes = "comments" } }`. It maps a path of the answer of `observe` to a table from a value to a step. Each look of the step reads the interrupts first, from the same answer, and then `watch`. If the value of a path names a step, the ticket moves there at that tick. With no interrupt that fires, `watch` and `on` decide. The move counts as an entry of its step, as a move of `on` does. The event `step` has `how` = `interrupt <path> = <value>`, and the keys `interrupt` and `value`. With several paths, Vision reads them in their sorted order, and the first value that names a step wins. A look comes after `from` and after the start verbs, so a deploy that the step starts runs first. The loader refuses `interrupt` on another kind of step, an empty table, a key that is not a path, and the path `head.state`. It also refuses a target that is not a string, a step that does not exist, and the step itself. A path that the answer does not have gives none, so its interrupt never fires. The answer of `observe` of the adapter for Jira and GitHub always holds `review.state`, so an interrupt on it costs no extra call.

A wait step can have `conflict`, such as `conflict = "merge-staging"`. When a start verb fails and its answer names a merge conflict, the ticket moves to that step at the next tick. No flag is set. The move counts as an entry of its step. The event `step` has `how` = `conflict <verb>`, the key `conflict` with the verb, and the key `reason` with the detail of the failure. A merge conflict wins over an interrupt. With no `conflict`, the failure sets the flag `needs-you`, as each other failure does. The loader refuses `conflict` on another kind of step, a step that does not exist, and the step itself.

A job can have `command`, a shell command of the person, with `doc` and `env`. The loader refuses `doc` and `env` without `command`, a `doc` that is not a plain file name, and a `doc` that the person or the pipeline owns, such as `plan.md`.

A verb with `once = true` runs one time for a ticket and its arguments, at any commit. The loader refuses `once` on a read verb and on a verb with `leaves`, because the gates judge what leaves at each commit. A read verb can have `about`, one line of up to 300 characters.

The runner reads three documents of a turn by their form:

- With the key `findings`, each finding of a document of the step becomes a row in `findings`. `agents/reviewer.md` and `agents/tester.md` give the form.
- `verdicts.json` gives one row in `review_rows` for each review comment. `agents/shepherd.md` gives the form.
- `plan.md` is also copied to `plan.agent.md` one time, so that a later edit of the person shows as a difference.

A step can expect these end facts:

- `doc:<name>`: the result has that document, and it is not empty.
- `commits`: the branch has a commit that it did not have at the entry into the step.
- `clean`: the work tree of the clone is clean.
- `job:<name>`: the job passes at the last commit. The table form gives the job its arguments.
- `human:<action>`: the person did that action.
- `{ gate = "cr", doc = "cr.md", net_max = 4 }`: code reads the report of a code review in the document of the result. Each finding is a numbered item whose first line is `N. [SEVERITY][BLOCKING|FOLLOW-UP] path:line`, and that has its own line `Net: +N`. The gate holds when no finding has a Net above `net_max` and no finding has the tag BLOCKING.
- `{ gate = "qa", doc = "qa.md", block = ["critical", "high", "medium"], notify = true }`: code reads a QA report. Each issue is a heading with an id such as `ISSUE-003`, and a field of its entry whose label ends with `severity` gives critical, high, medium, or low. The gate holds when no issue has a severity of `block`.

A missing `doc` is `<gate>.md`, a missing `net_max` is 4, a missing `block` is each severity but low, and a missing `notify` is false. Only a turn step can have a gate. The document of a gate is an expected document of the step.

A gate fails when its document is missing or empty, when a finding has no readable first line or no line `Net:`, when a line `Net:` belongs to no finding, when an issue has no readable severity, or when a QA report states no severity at all. A failed gate is the failed end fact `gate:<kind>:<doc>`. Its message names each failing finding with its number, its first line, and its Net, or each blocking issue with its id, its severity, and its title.

The count table of a QA report checks the issues. If the table counts more issues of a blocking severity than the entries show, the gate fails, because an entry that the gate cannot read can hide a blocking issue. Any other difference only adds a note. In a report with a section Issues or Findings, only a heading in that section starts an issue.

### 11.3 How a ticket moves

The ticket row holds the id of its step in `state`, its flags, and its counters. One tick moves a ticket at most one step.

- Entry of a turn step. The runner runs each `before` entry in order: a job or a verb. A job runs in the background (section 11.1), and the visit waits for its outcome. A job that does not pass sets the flag `needs-you`. A verb must reach the state `confirmed`. The visit remembers each entry that is done, so each one runs one time in a visit. The runner then asks `observe` for the ticket and writes `issue.json`. It writes `related.md` (section 16), builds the facts file, and starts the turn.
- If each end fact holds and a gate qa has `notify = true`, the issues of its report that do not block become one notice for the person. A notice never stops a ticket and never sets a flag.
- A turn that ends with `questions`. The cards go to the Judge. After each card has an answer, a new turn resumes the session with the answers.
- A turn that ends with `requests`. The executor serves each read verb, and a new turn resumes the session with the files.
- A turn that ends with `result`. The runner writes each expected document to the ticket folder. Then it tests each end fact.
- If an end fact does not hold, a new turn resumes the session with a message that names the fact. A visit gets `rounds` turns for failed end facts. After that many failed turns, the ticket gets the flag `needs-you`. With the default of 2, the visit gets one resume.
- If the failed end fact is a gate with a route in `fail`, and the gate found a blocking item in its document, the ticket moves at once to the step of the route: no resume, no flag, and no `after` verb. A blocking item is a finding that fails the gate cr, or an issue of a blocking severity. The move counts as an entry for the `cap` of that step. The document of the gate stays in the ticket folder, because the runner writes each expected document before it tests the end facts. The step of the route reads it through `reads`. A gate that fails only because its document is missing or cannot be read goes around again, up to `rounds`.
- `rounds` and `cap` count different things. `rounds` counts the failed turns of one visit, and each resume continues the same session. `cap` counts the entries into a step from `next`, `on`, or `fail`, so it limits the loops between steps.
- A wait step with `from` does nothing before the first moment after its entry with that local time: no `start` verb and no `observe`. With `from = "09:00"`, an entry at 23:00 starts at 09:00 the next day, an entry at 08:00 starts at 09:00 the same day, and an entry at 09:30 starts at 09:00 the next day. The visit keeps that moment in `opens`. `vision retry KEY` keeps the moment, so a retry never waits for a later day. The limit counts from the first look, which comes at the first pass after that moment. The column NOW of `vision status` and the app say `waits until 09:00`.
- If each end fact holds, the `after` verbs run in order. After each one has the state `confirmed`, the ticket moves to `next`.
- At the entry of an act step, each tick requests the first verb of `run` that is not `confirmed`. The verb takes the path of an `after` verb: its mode, the gates, the idempotency key, and the `{doc:NAME}` rule. After each verb has the state `confirmed`, the next tick moves the ticket to `next`, and this move counts as an entry for the `cap` of the target. A verb that a mode keeps back, that a gate refuses, or that fails sets the flag `needs-you`. A verb in the mode `ask` waits for `vision approve`. An entry above the `cap` of the act step sets the flag `needs-you`, and no verb runs.
- The commit is part of the idempotency key. So a new entry into an act step at a new commit runs each verb again, and `pr.open` answers with the pull request that is open. At the same commit, a verb that is `confirmed` does not run again.
- After `rounds` failed turns, a step with `extra_rounds` gets one more turn at a time, up to `extra_rounds` more, while the loop converges. The loop converges when the gate of the failed end fact fails on fewer items in the last turn than in the turn before it, and no failing item is CRITICAL (cr) or critical (qa). A turn whose document the gate cannot read gives no number, so the loop does not converge. Otherwise the ticket gets the flag `needs-you`, and the reason says whether the extra rounds are used or the loop does not converge. The event `extra-round` of each extra turn says why it was granted.
- When the end facts of a turn are tested, each gate of the step that has a document gets a row of `gate_rounds`. Each finding of a gate cr with a place gets a row of `findings` with the source `cr` and a fingerprint: its path and a hash of its title in lower case, with no digits and no punctuation. Two findings of two rounds are the same when their fingerprints are equal, or when their paths are equal and their lines are at most 3 apart. A finding that comes back two times after a round where it was gone becomes one notice for the person, `this finding keeps coming back after its fix`, with its first line, and a problem note in the journal entry of that turn.
- The resume after a failed job names its documents, such as `.vision/precheck.md`.
- A turn step with `on = { commits, none }` moves after its end facts hold and its `after` verbs are confirmed. It moves to `commits` when the head of the branch differs from the head at the entry into the step, else to `none`. A visit with no recorded head goes to `commits`. The move counts as an entry for the `cap` of the target. Its event `step` has `how` (`on commits` or `on none`), `way`, `entry_head`, and `head`. A step of this form that its `when` skips goes to `none`.
- A turn step with `on` moves by its findings. If the documents of its last turn hold a kept finding, the ticket moves to the step of `findings`. If they hold none, it moves to the step of `none`.
- A step with a `cap` counts each entry that comes from `next` or `on`. One entry above the cap sets the flag `needs-you` with the open items. An entry by `vision retry` or `vision take` does not count.
- A wait step first runs its `start` verbs. After each one has the state `confirmed`, it runs `observe` each `every` seconds, and it moves on the value of `watch`.
- The value of `watch` is a text. A path that the answer does not have gives `none`, and a `null` gives `none` too. If the step has `wait`, a value that is in neither `on` nor `wait` sets the flag `needs-you` at once.
- If a wait step has `limit`, a visit that watches for more than `limit` seconds sets the flag `needs-you`. The reason names the last value. The time counts from the first `observe` of the visit. `vision retry KEY` enters the step again and starts a new limit.
- A human step waits for its command. `vision needs` lists it. The one action is `approve-plan`.
- The end step frees the clone. If the pull request is merged, it records the event `ticket-merged`.

Each visit of a step is one row in `spans`. Its key `waits_on` says what the ticket waits for: `agent:<name>`, `job:<name>`, `person:<action>`, `watch:<path>`, `verbs:<verb>,<verb>` for an act step (such as `verbs:git.push,pr.open`), or `end`.

The column NOW of `vision status` says the same thing for people. After a turn that ended well, it names the first `after` verb that is not `confirmed`, with its state. Two examples are `runs git.push` and `waits for your approval of review.reply`. Before a turn, it names the first `before` entry that is not done. The text `next: a turn of <agent>` means that a turn is the next action. On an act step, it names the first verb of `run` that is not `confirmed`, with its state, such as `next: git.push` or `runs git.push`. After each verb is `confirmed`, it says `its verbs are done, so the next tick moves it to the next step`.

A ticket can have four flags. A ticket with a flag starts no new turn, no job, and no operation.

- `needs-you`: a rule stopped the ticket. `vision retry KEY` clears the flag and enters the step again. `vision retry KEY --step STEP` enters the step STEP instead, with the rules of a plain retry: the same refusals, a new visit that does not count for the cap of the step, and the same flags cleared. It refuses a step that the adapter does not have, the end step, and a ticket whose turn runs. Its event says that the person moved the ticket, from which step to which step, and the first turn of a turn step gets a message that says so. If the turn of the step ended well and an `after` verb stopped the ticket, `vision retry KEY` starts no new turn. It only requests the `after` verbs again. On an act step, `vision retry KEY` starts no turn either. It requests the verbs of `run` that are not `confirmed`.
- `paused`: the person took over the session with `vision open KEY`. `vision retry KEY` clears the flag and enters the step again. The new turn resumes the session, and its message says that the person worked in the session. `vision retry KEY --step STEP` continues it at STEP after the rules of a continue.
- `waits-for-limit`: a limit of section 4 refused a turn. The tick clears the flag after the limit allows a turn again.
- `stopped`: `vision stop KEY` killed the running turn and freed the clone. The branch stays in that clone. `vision take KEY` continues the ticket, and the next paragraphs say where. A stopped ticket asks for nothing until it goes on: `vision needs`, `vision status`, and the app leave out its flags, its plan, its writes, and its questions. Its notices stay, and so does the flag `paused`, because after `vision open` Vision asks whether the ticket goes on. The flags stay on the ticket, so that its continue card can name the reason.

A continue of a stopped or paused ticket reads the facts first. It calls `observe` one time, and it compares the head of the pull request with the clone (see below). Then it reads the rules of the adapter in order (section 11.2). A rule that holds and names the stored step ends the search: the facts confirm the step where the ticket is, and nothing is suggested. A rule that holds and names a later step is the suggestion. A later step is one that the step list has later, and the links of the steps lead there. In a loop, the step list decides, and the targets of `interrupt` count as links. A rule that holds and names an earlier step, or a step with no order, is the suggestion only with `back = true`. Without `back`, the search skips the rule and reads the next one. If no rule applies, nothing is suggested, and the reason names the rules that held but named an earlier step. A rule never moves a ticket by itself. The person takes the suggestion or picks another step. A plain retry, a tick, and a turn run no rule. With no `[[resume]]` table, a continue runs no `observe`, and nothing changes.

`vision take KEY` of a stopped ticket starts the stored step again. If a rule suggests another step, the command changes nothing. It prints the suggestion, its reason, and the two commands, and it exits with code 1. `vision take KEY --step STEP` continues a stopped ticket at STEP, and `vision retry KEY --step STEP` does the same for a paused ticket. At another step, this is the move of `vision retry --step`, and the move clears the stop too. A move to the end step needs a rule that suggests it at that moment.

`vision retry KEY --step STEP --new-branch` moves the ticket to STEP on a new branch. One use is an old branch made by hand that is no base for the work. The new branch has the name of the `branch` pattern of the adapter, and Vision makes it from `base` after a fetch. Without `--step`, the ticket enters its current step again. The old branch stays in git and on the remote. The row drops the number of its pull request, because that pull request belongs to the old branch. The command refuses a ticket that is on the branch of the pattern already. It also refuses a work tree with changes, and a branch of that name in the clone or on the remote. The first turn of a turn step gets a message that names the new branch, in a new session. As a plain retry, the command ends a pause. The event `branch` has `from` and `to`.

A plain `vision take KEY`, the line of a continue, and the continue card list each fact that Vision read, in words, in a fixed order. The order is the tracker status, the pull request, the review state, CI, each env, and the head state. The pull request gives its number, draft or ready, merged, closed, and a conflict. The review state comes with the comments that wait and a change request. Each env says whether it holds the head and whether it runs it. A suggestion that goes back says so. One example is "This goes back from ready-to-merge: 1 review comment waits". A call of `observe` that fails gives no facts.

If the pull request is open, a continue compares its head with the branch in the clone. If the clone does not have that commit, Vision fetches first. The head can be a later commit of the branch, such as a push of the person from another checkout. Then the branch moves forward to it. This move is a fast-forward, and Vision never resets a branch. The row of the ticket then takes that commit as its head. In four cases nothing moves: the fetch fails, the commit is still missing after the fetch, the branches went apart, or the work tree has changes. Then the ticket gets the flag `needs-you` with the reason and the fix, and the next continue clears that flag. If the row of the ticket has no number of a pull request, the continue writes the number of the open pull request.

A continue that read the rules writes the event `resumed` and a row of `human` with the kind `resume`. The event names the stored step, the chosen step, the suggestion, whether the person took it, and the head that the continue took. `vision log` and the tab You say it in words. The event also has `back`, which is true for a suggestion that goes back, and `facts`, the list of the facts in words. For a move back that the person took, `vision log` says "back from <step>".

A `failed` operation is final for its key. `vision retry KEY` frees the key, so that the verb can run again at the same commit. If an operation of the ticket is `uncertain`, `vision retry KEY` refuses, because the reconcile step must settle it first.

`vision take KEY` needs a slot: a free clone of `clones`, or the slot of a ticket that only watches (see "Slots and the queue" below). It fetches, and it makes the branch from `base`. A clone holds one ticket at a time. A clone can be a git worktree, and the take uses the clones in their order. Git lets one worktree hold a branch at a time. If another worktree holds the branch of the ticket, the take stops, names that worktree, and says how to free the branch. A continue of a stopped ticket does the same.

A ticket can be in progress outside Vision: someone made a branch with its key by hand, and maybe opened a pull request. The queue gives such a ticket as not eligible, with its `branch`, and with `pr` for an open pull request. The branch of the pull request wins over another branch with the key. If git cannot take the text as the name of a branch, Vision drops the branch of that ticket.

A plain `vision take KEY` of such a ticket calls `observe` one time, with that branch and that pull request. Then it reads the rules from the first step of the path. It prints where the ticket is, each fact, and the suggestion with its command. It changes nothing, and it exits with code 1. Vision has no clone of the ticket yet, so the facts have no head state. A rule that names the end step suggests nothing, because Vision takes no ticket that is done.

`vision take KEY --step STEP` takes such a ticket at STEP, in a free clone, after a fetch. A branch that the clone does not have starts at the remote branch. A local branch that is behind the remote branch moves forward to it, and a local branch with its own commits keeps them. A local branch that went apart from the remote branch stops the take, and nothing moves. The take refuses the end step and a step that the adapter does not have. The steps before STEP do not run.

The row of the ticket gets the branch, the number of the pull request, and the head. The event `take` has `outside`, `pr`, `facts`, and `suggestion`. From then on, Vision holds the ticket as any other ticket.

Slots and the queue. Each clone of `clones` is a slot. A ticket that is not stopped holds the slot that its row names. A wait step with no `start` entry only watches. A wait step whose `start` entries are all `confirmed` also only watches. At such a step, the ticket waits for the outside world: a review, CI, a deploy, or a time. It needs no work tree, so it can give its slot to another ticket. Then it is in the background: its row names no clone, and Vision still watches it at each tick with the head of its row.

If another ticket needs a slot and no slot is free, an idle ticket gives its slot. A ticket is idle in this case: it holds a slot, it has no flag, no turn of it runs, and its step only watches. Its work tree must be clean, and its head must be on the remote, so that no commit stays only in that clone. Vision finds the head on the remote in three ways. The pull request of the last look has it, or a remote branch of the clone holds it. Else a fetch of the branch can bring it. Of the idle tickets, the one that began to watch first gives its slot first. Its row then names no clone, and its head is the head of the clone. The event `slot` has `gave`, the clone, and `to`, the ticket that gets it.

If the row of a ticket names no clone and its step does not only watch, the ticket needs a slot. One example is a ticket in the background whose review moves it to a turn step. The tickets that need a slot form a queue with two groups. First come the tickets that started, in the order of the entry into their step. Then come the takes that wait for their first slot, in the order of the take. A ticket with another flag than `waits-for-slot` is not in the queue, because it waits for the person.

At each tick, before the end facts, Vision gives the slots in the order of the queue. If the clone that a ticket held last is free, the ticket takes it. Else it takes the first free clone, else the slot of an idle ticket. A ticket that started switches the clone to its branch at the head of its row. If the clone does not have the branch, Vision makes it at that head, after a fetch. If the branch in the clone points to another commit, the ticket gets the flag `needs-you`, and the slot goes to the next ticket. A take that waits does the work of the take in that clone. The event `slot` has `took`, the clone.

A ticket that gets no slot has the flag `waits-for-slot`, and its reason gives the place in the queue, such as "2nd in line". The flag stops each action of the ticket. If the ticket gets a slot, the tick clears the flag. `vision needs` does not list it, because the person has nothing to do. The column NOW of `vision status` and the app show the place in the queue. For a ticket in the background, they show "in the background".

`vision take KEY` puts the ticket at the end of the queue and gives the slots at once. If the ticket gets a slot, the take prints the clone, as before. Else the take prints the place of the ticket in the queue, and it exits with code 0. The row of a take that waits has the step, the branch, the flag, and no clone, and the event `queued` has its place. `vision stop` takes a ticket out of the queue. A stopped ticket can hold no slot, or another ticket can hold its clone. Then a continue takes a free slot or the slot of an idle ticket. If no slot is free, a take that never started goes back to the end of the queue. Each other continue then stops with the reason.

The proof is a job. It runs the changed tests at the last commit, and they must pass. It runs the same tests on the base commit, and they must fail. Any new commit voids the proof.

The proof gets the approved plan in `args.plan`. A line `Proof: green` in the plan declares the whole proof green, for a change with no new behavior. The changed tests then only have to pass at the last commit. A line `Proof: green <path>` declares one test file green, and the proof does not run that file on the base commit.

### 11.4 The demo adapter

`adapters/demo` needs no network. Its tracker is a JSON file. Its forge is a bare git repo with a JSON file of pull requests. Its CI is a script of the project. The integration tests use it with the fake `claude`. A person can use it with the real one to try Vision on a toy repo.

Its path is the reference path: `refine`, `plan`, `plan-ready`, `build`, `review`, `open-pr`, `ci`, `ready`, `comments`, `ci-review`, `ready-to-merge`, and `done`. The commits of `build` and `review` stay in the clone. Then the act step `open-pr` runs `git.push`, and `pr.open` with the text of `pr.md`. So nothing leaves the machine before the review. `fix-ci` and `comments` push their own commits with `after`. A red CI run goes to `fix-ci` and back to `ci`. After the step `comments`, a commit of the Shepherd goes through CI again in the wait step `ci-review` before the merge. At a commit where CI already ran, `ci.run` is the operation of that commit, so it does not run again. Each loop goes through a turn step with a cap, and each wait step has `wait` and `limit`.

The three demo adapters have the same rules of a continue. They suggest `done` for a merged pull request or the status Done. A change request gives `comments`, and a push by hand gives `ci-review`, both with `back = true`. An open and ready pull request gives `comments`, and an open draft gives `ci`.

`adapters/demo-investigate` uses the scripts of `adapters/demo` and starts with the investigation: `refine`, `investigate` (a team by kind), `analyze`, `critic` (wide) or `critic-narrow` (narrow), `questions` (ask), `amend`, `reproduced` (check, bugs only), `plan-ready`, and then the rest of the demo path from `build`. The fake `codex` of the tests stands in for Codex.

`adapters/demo-review-team` uses the scripts of `adapters/demo` and reviews with a team. `review-lanes` is a team of three readers that the adapter defines: `cr-correctness` and `cr-design` on Claude, and `cr-codex` on Codex with a fallback. Each writes `findings-<lane>.md` in the form of the gate cr. In `review-challenge`, each lane reads the findings of the two other lanes and writes `challenge-<lane>.md`, one line `<lane> F<n>: agree|disagree: <evidence>` for each finding. In `review`, the Reviewer with `roles/reviewer.md` judges each finding with its votes, fixes the kept ones, and writes `cr.md` for the gate cr and `cr-fixed.md`. A commit of the lead sends the ticket back to `review-lanes`. No commit sends it to `open-pr`, which pushes the branch and opens the pull request, and then to `ci`. The cap of `review-lanes` is 2, so its third entry needs the person.

### 11.5 The adapter for Jira and GitHub

`adapters/jira-github` connects Vision to Jira Cloud and GitHub. The binary answers its calls, because its `adapter.toml` says `scripts = "builtin:jira-github"`. The adapter runs `git`, and `vision app-token` runs `openssl`. The comment block at the top of its `adapter.toml` documents each key of `[adapter]`, each secret, and the path of a ticket.

The path has these parts:

1. The Scout refines the ticket, and `tracker.describe` writes the new description to Jira. The answer keeps the old description as a backup document.
2. The Scout writes the plan, and the person approves it.
3. `tracker.move` moves the issue to the status of work. The Builder builds, and the proof runs. The commits stay in the clone.
4. The Reviewer reviews the diff. A second review command is optional.
5. The Tester does QA on the local app. A kept bug goes to `fix-qa`, then to the review, and then to QA again.
6. When QA holds with no kept bug, the act step `open-pr` runs `git.push` and then `pr.open`. `pr.open` opens a draft pull request with the text of `pr.md`.
7. `ci.run` starts the CI workflow. After a red run, `ci.rerun` runs the failed jobs one more time. After a second red run, the Builder fixes the fault. A green run goes to `ready`.
8. The person marks the pull request as ready. The team reviews it, and the ticket waits in `in-review`.
9. The Shepherd judges each comment that waits, and it fixes each fault that a comment names. `git.push` pushes the fix, and `review.reply` answers each comment. Then CI runs at the new head in `ci-review`. After a green run, the act step `rerequest` runs `review.rerequest`, and the ticket waits in `in-review` for a new round or the approval. After a red run, the Builder fixes the fault in `fix-ci-review`, and the fix goes through `ci-review` again.
10. After the approval, the person merges, and the ticket ends.

A round of comments never goes back to the review, the QA, or `open-pr`, and a late round after the approval takes the same path. No commit leaves the machine before the review and the local QA hold. The steps after `open-pr`, such as `fix-ci`, `comments`, and `fix-ci-review`, push their own commits.

The adapter has five rules of a continue. A merged pull request gives `done`. A comment or a change request that waits gives `comments`, with `back = true`, so a late comment also reaches a tested ticket. A push by hand on an open pull request gives `ci-review`, with `back = true` too. An open and ready pull request gives `in-review`, and an open draft gives `ci`. So a ticket whose pull request the person opened by hand goes on in the code review, and its own review does not run again. The comment block of its `adapter.toml` shows an `interrupt` on the deploy steps of an env, so that a late comment ends their wait.

These rules keep the tokens and the repo safe:

- A token goes to a service only in a header, and only over https. It never goes into a file, a command line, a remote address, an error text, or an answer.
- `git.push` gives the token to one git call through environment variables, for the address of the remote only. A push never forces.
- A job that runs code of the project gets no secret.
- A reconcile call only reads.
- Text from Jira and from GitHub is data. Each document wraps it as `{"untrusted": "..."}`.

`vision app-token` prints an installation token of a GitHub App, for the command of the secret `FORGE_TOKEN`. The private key comes from a file or from the output of a command, and it never goes into a file. The command can print the key as a PEM file or as the PEM file in one base64 line.

A comment that waits has one of three kinds. `inline` is a thread on a line of the diff, and its reply goes into the thread. `review` is a review with a text that asks for changes or that comments. `conversation` is a comment in the conversation of the pull request. A reply to a review or to a conversation comment is a new conversation comment. It quotes the first line of the comment, and it ends with a hidden mark that names the comment. So a later round does not answer the same comment again.

Each row of `comments.json` has a `mark`, such as `inline:501:503`. The mark holds the kind, the id, and the id of the last comment of the thread. A review and a conversation comment are threads of one comment. When the end facts of a turn with `verdicts.json` hold, Vision keeps the mark of each row of the comments that the step read. The marks go to `comments-seen.json` in the ticket folder, and `observe` of the ticket gets them in `seen`. A comment whose mark is in `seen` does not wait, and it does not count in `comments_waiting` or in `review.state`. So a comment that needs no reply makes one round and not a loop. One example is a summary of a review whose findings have their own threads. Others are a note of a preview and a note of a bot that found nothing. A new reply in a thread gives the thread a new mark, so the thread waits again. An edit of a comment keeps its mark. The job `comments` still lists each comment that waits, also a seen one, so the agent of a round sees the whole talk.

The key `own_logins` names the logins of the person and of the bot. The key `ignore_logins` names logins whose comments never wait, such as a CI bot. An entry can also be `<login>:<kind>`, with the kind `inline`, `review`, or `conversation`. Then only the comments of that kind of that login never wait. This fits a review bot that keeps one status comment in the conversation.

`review.state` is the first value that holds of `merged`, `closed`, `comments`, `changes`, `conflict`, `approved`, and `waiting`. With enough approvals and no change request, a comment from before the newest approval does not wait, because the approver saw it. So an approved ticket goes on. A comment of the same second as the approval still waits, because GitHub gives whole seconds. A comment after the approval still waits, such as a finding of a review of a later push. A change request of an approver counts. A change request of another reviewer counts too, unless its login is in `own_logins` or `ignore_logins` skips each of its comments. So another reviewer can stop an approved ticket too. After the fix, `review.rerequest` asks that reviewer for a new review. A GitHub App has a login that ends with `[bot]`. An app reviews each push by itself, and GitHub never lists an app as a requested reviewer. So the change request of an app on an older commit than the head does not give `changes`. The ticket waits until the app reviews the head, and `pr.rereview_by` names the app. `review.rerequest` never asks an app. `conflict` means that the open pull request conflicts with its base. No step names `conflict`, so the ticket gets the flag `needs-you` at once, with the value in the reason. The person merges the base into the branch in the clone, resolves the conflict, and pushes the branch. Then `vision retry KEY` lets the ticket go on.

The key `envs` lists the staging envs of the project. For each env, `observe` gives `envs.<name>.branch`, `envs.<name>.head`, and `envs.<name>.holds`. `head` is the last commit of the env branch. If the env branch holds the head commit of the ticket, `holds` is true. It says that the merge happened, not that the env runs the new code.

`deploy` merges the head into the env branch with the merge API of GitHub. When GitHub answers that the merge conflicts, `deploy` fetches the tip of the env branch into the clone as `refs/vision/deploy/<env>`. It makes the git worktree `.vision/merge/<env>` inside the clone at that tip, with no branch, and it starts the merge there with no commit. It writes `deploy-conflict.md` with the tip, the head, the merge message, and each file with a conflict. Its answer is `failed` with `conflict: true`, so a wait step with `conflict` sends the ticket to Merger.

Merger resolves each conflict in the files of the merge folder. It does not commit, because the sandbox of a turn cannot write the git folder of a worktree. Then `deploy.merge` stages the files with a conflict and commits the merge with the merge message and the identity in `git`. A merge that a person committed by hand goes as it is. `deploy.merge` refuses a file of the list that still holds a conflict marker, and a change to a file that had no conflict. Then it pushes the merge to the env branch. It pushes only a merge whose parents are the tip and the head, so the push is a fast-forward and never a force. It refuses an env branch that moved after the merge started, and the person runs the deploy step again. After a push, the merge folder goes away. When the merge cannot start, such as with no clone, the answer stays a plain failure, and the person merges by hand.

An env can also name `deployment`, a GitHub environment where the project records its deployments. For such an env, `observe` reads the newest deployment and gives `envs.<name>.deployed`, `deployment_sha`, and `deployed_at`. If the commit of that deployment holds the head commit of the ticket, `deployed` is true. The adapter reads the deployment record and not its statuses. So `deployed` is true from the moment that the project records the deployment.

The job `app-start` remembers the app that it started in a state file in the temporary folder. The file holds the process id, the start time of that process as `ps` reports it, the head commit, and the log path. If that app still runs and the head commit changed, the job stops its process group and starts the app at the new head. The job never stops a process whose start time differs from the state file. The job trusts only a plain file of this user that no other user can write. An app that answers but that Vision did not start stays as it is, and the detail says that it can run older code.

`app_restart = false` turns the restart off, for an app that loads new code by itself. `app-<env>.json` names the head of the app in `head`, and `restarted` says whether the job started it again.

The key `test_canary` names test files that pass on the base commit. The proof runs them first in the worktree of the base commit. If one is missing or fails, that worktree cannot run tests. Then the proof does not hold, its check `proof-canary` has the cause `env`, and no changed test runs on the base. Without a canary, a worktree that cannot run tests looks the same as a new test that fails on the base.

A job with `command` in the `adapter.toml` of a project is a command of the person, and the built-in adapter runs it. The command runs with `sh -c` in the clone, or in the adapter folder when the call has no clone. It gets `VISION_KEY`, `VISION_CLONE`, `VISION_BRANCH`, `VISION_HEAD`, and `VISION_BASE`. Exit code 0 passes, and each other code fails. The last 20 lines of standard error are the detail of a failure, with each secret masked. The standard output goes into `doc` after a pass and after a failure. The time limit of the job stops the command too. A command with the name of a job of the adapter, such as `proof`, replaces that job.

The job `precheck` runs the lint, the specs, jest, and the migrations of the project before a push. A step names it in `expects` before `git.push`. Its keys are `lint`, `specs`, `specs_map`, `jest`, `migrations`, `migrations_env`, `cap`, `cap_per_file`, and `cap_max`, as the comment of `adapters/jira-github/adapter.toml` shows. An entry of `lint` or `jest` is a command, or `{ run, globs }`, and a command with no changed file that matches its globs does not run. Each command runs with `sh -c` in the clone, with `env = "session"` and no secret. The default timeout is `cap_max + 6 x cap`.

- A lint offense counts only on a changed line, read as `path:line[:column]: message`. A lint that fails and names no offense is broken, and the job fails.
- `specs_map = "rails"` maps `app/<dir>/<path>.rb` to `spec/<dir>/<path>_spec.rb` and `lib/<path>.rb` to `spec/lib/<path>_spec.rb`, when that file exists. A changed spec file runs itself.
- A red spec in a file that the branch did not change runs again in a worktree of the base commit, after `setup_command` and the files of `test_canary`. If it fails there too, it is not ours, and it passes.
- Each added migration runs the `migrations` commands in version order, with `migrations_env`. Each tracked file that they changed, such as `db/schema.rb`, goes back to the state of the branch.
- The cap is `cap + cap_per_file x the checked files`, at most `cap_max`. When the cap stops the specs or jest, the job passes with the note `unfinished: CI is their only run`. When the cap stops the lint, the job fails as a hang. The reruns on the base and each migration command have `cap` alone.
- The job writes `precheck.md`.

The executor refuses a command of the precheck that opens a shell or a console on a remote machine or a pod, such as `kubectl exec`, `ssh`, or `heroku run`, or that changes the schema of a live database, such as `gh-ost`.

The read verb `review.read` gives the first review of `args.reviewer` that was submitted after `args.after`, with its inline comments, as `review.json`.

`review.resolve` resolves the review thread of each inline comment that `verdicts.json` settles. `review.minimize` hides each comment that it settles: an inline comment, a review, or a conversation comment. A row settles its comment with `settle`: `true` or `"resolved"` (the classifier RESOLVED), or `"outdated"` (OUTDATED). Both verbs take `{ verdicts = "{doc:verdicts.json}" }`, as `review.reply` does, and both use the GraphQL API of GitHub.

These two verbs touch only a thread or a comment of a bot. A bot has a login that ends with `[bot]`, or a login of the key `bot_logins`. Each other comment of a thread must come from a bot or from a login of `own_logins`. The verbs refuse each other item, go on with the rest, and name each refused item in the detail. A missing comment fails the operation after the others. A reconcile reads whether each thread is resolved and each comment is hidden.

`tests/e2e_jira_github.rs` drives the whole path against a fake Jira and GitHub with state (`tests/jira_github_support/world.rs`). A thread of the test serves it on a free localhost port. `tests/adapter_jira_github.rs` tests each verb, read verb, job, and observe call of the built-in adapter against a fake server. No run against the real services is part of the tests.

### 11.6 The escape check

When a ticket has a pull request and our gate cr passed at a head, the tick looks for the first review of each login of `escapes.reviewers` that was submitted after that pass. It looks through the read verb `escapes.verb`, at most once each `escapes.every` seconds, and it reads that review once. Each inline comment whose priority (group 1 of `escapes.priority`) is in `escapes.count` is matched against each finding of every round of the gate cr of the ticket, on the same path within `escapes.lines` lines. A comment with no match is an escape: a row of `escapes`, a row of `findings` with the source `escape`, and a problem note in the journal entry of the turn whose gate passed. A new pass of the gate at a new head opens a new check.

## 12. The executor

No session can call the executor. It runs after a rule in code, after a typed command of the person, or after a turn whose end fact holds.

- Each verb has a mode: `hold`, `shadow`, `ask`, or `auto`. `hold` sets the flag `needs-you`. `shadow` records what the verb intends, does nothing, and sets the flag. `ask` waits for `vision approve KEY VERB COMMIT`. Only `vision promote` and `vision demote` change a mode.
- After `vision promote`, the next tick asks again for a write that the mode `hold` or `shadow` kept back. The executor judges it in the mode of now.
- Each operation gets a row with the state `pending` and an idempotency key before it runs. The key is a hash of the ticket, the verb, the last commit, and the arguments. The key of a verb with `once = true` leaves out the commit, so a newer commit does not end its operation. The states are `pending`, `executing`, `confirmed`, `failed`, and `uncertain`.
- At each tick, the executor reconciles each operation in the state `executing` or `uncertain`. It calls the script with `reconcile` set to true. An `uncertain` operation never runs again by itself.
- Before a verb with `leaves` runs, the gates test what leaves the machine. They refuse text or a diff that contains a secret of `[secrets]`, a private key, or a common form of a token. They refuse a diff above `gates.max_diff_lines`. For a ticket that the person took from outside Vision, the size gate and the sensitive gate read only the diff after the commit of the take, while that commit is in the history of the head and the ticket is still on the branch of the take. The forge holds the branch up to that commit already. They hold a diff that touches a path of `gates.sensitive` that the approved plan does not name. `vision approve` lets a held diff go.
- The secrets gate reads the diff from the base to the last commit. It also reads the patch of each commit between them, because a push sends each commit. It reads each file as text, so that an attribute of the repo cannot hide a file.
- The executor refuses an operation with a secret value in its arguments, for each verb.
- A new request of a verb at a newer commit ends the older operation of that verb that still waits. Its state becomes `failed`.
- A human operation names its object with an id that cannot change: `approve-plan KEY HASH`, `answer ID`, `approve KEY VERB COMMIT`. A stale id is an error.
- After a wrong operation, the person moves its verb down the ladder with `vision demote VERB`. Vision never changes a mode by itself.
- Each write verb of the shipped adapters starts in the mode `auto`, so no write waits for an approval. The gates still test each verb with `leaves`. A verb with no `mode` in `adapter.toml` starts in the mode `ask`. `vision demote VERB` moves a verb down the ladder, and `[modes]` of the configuration keeps the new mode.
- `vision approve` runs each operation of the verb that waits at that commit. With `--op ID`, it runs only the operation with that id. If that operation no longer waits at that commit, the command sends nothing.
- `vision approve` exits with code 2 after a gate refuses. It exits with code 1 after an operation fails or stays `uncertain`.
- A job with `env = "full"` gets the environment of the tick, with no variable that has the name or the value of a secret, unless the job has `secrets = true`. Each other script gets the plain environment. The executor passes the table of a job with `command` in the call, so the adapter needs no second reader of `adapter.toml`. At the time limit of a call, the executor kills its process group, and the command of a job runs in that group, so it stops too.
- The executor calls a built-in adapter in the same way as a script of `bin/`: the same environment, process group, time limit, masking, and exit codes. An error text names it as `bin/observe`, `bin/act`, or `bin/job`.

A script that a pass runs can take a while, and the ledger says nothing about a script until the script ends. So the executor writes `running.json` right before it runs a script. This holds for an operation, a read verb, and the check of an uncertain operation. A job runs in the background instead, and its folder in `jobs/` shows it (section 11.1). The file holds the kind, the name, the ticket, the start time, and the process id of the pass. When the script ends, the executor removes the file. A reader trusts the file only while that process holds the lock of the tick. The next tick removes a file that a crash left. The ledger stays the evidence.

## 13. Commands

```
vision                             in a terminal: the app (section 15). If not: the help
vision init                        write the home folder and the configuration
vision config                      print the configuration in use
vision status [--json]             each ticket, its state, its marks, and the limits
vision watch                       the same view, drawn again each 2 seconds
vision prompt                      a short status for the shell prompt
vision tick                        one pass
vision on | off                    the kill switch
vision doctor                      test the setup
vision pick                        the eligible tickets, with the reason for each other one
vision take KEY...                 start a ticket
vision take KEY --step STEP        continue a stopped ticket, or take a ticket in progress outside Vision, at a step
vision kind KEY NAME               set the kind of a ticket; an unknown kind is refused
vision plan KEY                    show the plan
vision approve-plan KEY HASH       approve the plan with that hash
vision approve KEY VERB COMMIT     approve one operation in the mode ask
vision log KEY                     the timeline of one ticket
vision logs [KEY] [--follow]       the events of the running turn
vision open KEY                    attach to the running turn and type, or take over the session
vision tell KEY "TEXT"             send a message to the running turn, or to the next one
vision diff KEY                    the commits of the ticket that are not on the forge
vision stop KEY | retry KEY        stop a ticket, or start its step again
vision retry KEY --step STEP       move a ticket to a step; a paused ticket reads the rules first
vision retry KEY --new-branch      the same on a new branch of the pattern, made from base
vision needs | answer | answers | correct      section 8.4
vision memory ...                  section 9.2
vision curate                      run the Curator now
vision model ...                   section 10
vision promote VERB | demote VERB  move a verb on the trust ladder
vision report [KEY] [--week] [--json]   the measures, and the turns and cost of each agent under By agent; with KEY, the escape checks and escapes of one ticket
vision install | uninstall         the timer of the system
vision app-token ...               print an installation token of a GitHub App, for a secret command
vision journal search "TEXT" [--json] [--limit N]   search the journal (section 16)
vision journal show ID | reindex [--all] | rebuild
vision archive [--dry-run] | fetch TURN-ID | status [--json] | move-to-bucket --from DIR [--dry-run]
vision sessions archive [--dry-run] | search "TEXT" [--json] [--limit N] | fetch SESSION-ID [--to DIR]
```

`vision adapter-script NAME SCRIPT` is hidden. Only the executor calls it, for an adapter with `scripts = "builtin:NAME"`.

A command that is not built yet prints `not built yet: <name>` and exits with code 3.

`vision approve` takes `--op ID`. It then approves only the operation with that id (section 12).

`vision status` names the job or the operation that a pass runs now, in the line `pass     runs <what> for <ticket> since <time>`. `vision status --json` gives it in `running`, or `null`.

`vision report` lists the steps in the order of the step list of the adapter. A step that the adapter no longer has comes after them, by name. `vision report` and `vision status` show a cost above zero and below one cent as `<$0.01`. `vision report --json` gives the order in `steps`.

## 14. Rules for the code

- Rust, stable toolchain, one binary crate with a library. No async runtime. No daemon.
- Use only the crates of the list below. Ask before a new crate.
- Run `git` as a child process. Do not link a git library.
- Write each state file to a temporary name and rename it.
- Exit codes: 0 done, 1 error, 2 a gate refused, 3 not built yet.
- Each command that prints a table also takes `--json`.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` must pass.
- A test never calls the real `claude`, the network, or the real home folder. `tests/bin/fake-claude` plays a session from a script file. `VISION_CLAUDE_BIN` and `VISION_HOME` point a test at the fake and at a temporary folder. `tests/bin/fake-aws` plays the aws program, first in PATH under the name `aws`, and `tests/journal.rs` serves a fake MemPalace on a free localhost port.
- Three tests are the exception: `tests/real_claude.rs`, `tests/real_judge.rs`, and `tests/real_e2e.rs`. They are ignored, and they run only with `VISION_REAL_CLAUDE=1` and `--ignored`. `tests/real_e2e.rs` takes `DEMO-12` through the whole demo path with real sessions of the model `haiku`. `tests/real_claude.rs` also has a test for the commit of a worker, a test that the sandbox holds, and a probe. The probe runs the commands of `VISION_PROBE_COMMANDS` in the sandbox of a worker, in the folder of `VISION_PROBE_DIR`.
- No name of a company, a person, or a private project goes into the repo.
- Each error and each warning that the person sees comes through `src/friendly.rs`. It shows what happened in one plain sentence, then what to do, when a rule knows it. The raw text stays below as the details, and the ledger and the log keep it as the evidence. A new shape of reason gets a rule there, with a test.

```
clap  serde  serde_json  toml  toml_edit  rusqlite (feature "bundled")
anyhow  thiserror  time  uuid  sha2  libc  fs2
ratatui (with its crossterm backend, for the app)
ureq (no default features, feature "rustls": blocking HTTP with TLS, for the built-in adapter)
```

## 15. The app

If standard input and standard output are both terminals, `vision` with no arguments opens a full-screen program. Each command of section 13 stays, for scripts and for the timer.

### 15.1 The layers

The app has three layers. The model is plain data. The update takes a message, changes the model, and gives the effects to run. A message is a key, a click or a step of the mouse wheel, a tick of the clock, or the result of a background thread. The view draws the model. The view also gives the parts of the frame that the mouse can hit. The runtime keeps the parts of the last frame in the model after each draw, as it sets the clock before each message. The update and the view read no clock and no file. So each screen has a text snapshot in `tests/snapshots/app/`, and no test needs a terminal.

The data of Home and of the live view comes from the loader, one thread that keeps one connection to the ledger. Each other read runs in a thread of its own, through the functions that the commands use: the rows of `vision pick`, the report, the plan, the commit list, the model table, and the adapter. The cards read the questions, the operations that wait, and the proposals of the Curator in the same way. Each change of the state runs its command as a child process of the same binary, with no standard input. The app shows its own notice for a success, and the text of standard error for a failure. One tested path does each change, no text of a command reaches the screen, and a crash of an action cannot stop the app.

A draft of the question form is not a change of the state. One thread of the runtime writes the drafts file after each draft, in the order of the drafts, with the atomic write of the repo. The loop never waits for this write. A load of the cards starts after each write that came before it. The app ends the writes that wait before it leaves. An action that reads a file gets it from the worker. The worker writes the file in the home folder, runs the child in that folder, and removes the file after the child ends. The send of the question form runs `vision answer --file` in this way.

The update asks the loader for the data of the screen each second. The loader first makes a check that costs little. It reads `PRAGMA data_version`, which changes after a write of another connection to the ledger. It reads the size, the time of the last write, and the inode of `config.toml`, `adapter.toml`, and `off`. It also makes the tests on the disk that a load of the adapter makes. The scripts folder must be a folder, and each role file of `[agents]` must be in its place. It reads the same of each entry of the folder of each ticket that the screen shows. It leaves out `answer-drafts.json`, `answer-send.json`, and their temporary files, because only the question form reads them. It also reads `running.json`, the turns that run with their phase and their minutes, the timer, and the version of the list of proposals. If nothing changed and the last read is less than 15 seconds old, the loader answers with a short message, and the screen keeps its data. Else it reads the data again in one read transaction. The loader keeps the check from before the read, so a change during the read shows at the next look. A read can show a ticket that the check before it did not name. Then the next look reads again if the folder of that ticket has an entry. After a read that fails with an error of SQLite or of the disk, the loader opens a new connection. Another error, such as a ticket that the ledger no longer has, keeps the connection.

Home reads the row and the facts of each ticket in progress only, and the costs of all tickets with one query. The live view reads the rows of its ticket only, and only the events after the last event that it has. When the view closes, the loader drops what it kept for it. The loader parses `config.toml` again only when it changes. It loads the adapter again when `adapter.toml` changes. It also loads it again when a test on the disk gives another answer, such as a role file that went away. It reads the proposals of the memory again after an action or a session ends, when a ref of the memory repo changes, or after one minute. A pass changes the proposals only with a commit, so a pass alone causes no read. A change with no write to the ledger, such as a commit in a clone or the moving numbers of the last 7 days, shows within 15 seconds.

### 15.2 The engine

The worker is one thread that runs one job at a time. A job is a pass or an action. A pass is the child `vision tick`. A pass starts at these times:

- The app starts.
- An action ends.
- A turn ends. This holds for a turn of the Judge and of the Curator too.
- A job of the background ends.
- One minute passed since the last pass.
- A pass changed a ticket, and the next pass has work for a ticket.

One pass does one thing for a ticket: it collects a turn, or moves the ticket to its next step, or starts the next turn. So the last rule starts the next pass at once, and a ticket does not wait a minute between two steps. A pass that changed nothing starts no such pass. At most 12 such passes run in a row, and the pass of the minute starts the count again. So a ticket that cannot move never makes the engine spin.

An action of the person goes before a pass that waits. A pass that finds the lock of a different tick is not an error, so the timer of section 6 can run too. A job of the adapter runs in the background (section 11.1), so a pass waits only for an operation or a read. The screen stays live in that time. The header shows the pass with its time. The state of the ticket names the operation from `running.json`, or its job of the background, such as "the job proof runs · 3:10". The header shows a pass after it ran for 2 seconds. Before that time, the screen does not move for the pass.

The person can pause the engine. Then no pass starts, and actions still run. One exception is the pass that a key of a card asks for (section 15.3): it runs while the engine is paused too. The pause lives only in the memory of the app. The kill switch of section 3 is a different thing: the app shows it and can switch it.

If the person quits the app, a running turn finishes by itself. The next step waits for the next start of the app, or for the timer.

### 15.3 The screens

The loader shows an android while the data loads. Its four stages are real: the home folder, the ledger, the tracker, and the engine. The loader leaves at the first of these events:

- The android appeared, and each stage is done or failed.
- The person pressed a key.
- The tracker stage took more than 10 seconds. The load then continues, and Home shows the queue later.

If the home folder is not set up, the android appears and then stays still. The app then draws a frame only after a key or a change of the window.

If the home folder has the file `loader.ans`, the loader shows that picture in place of the android. The file is text with color codes, as a tool such as `chafa` writes it. The app reads the file as data. Only the codes of color and bold count. Each other escape sequence and each control character goes away. The picture keeps at most 240 columns and 120 rows. A file that is empty, larger than 1 MiB, or not valid UTF-8 counts as no file.

The room for the picture is the rows above the title and the width of the window less 4 columns. A picture that fits the room shows cell for cell. A larger picture shrinks until all of it fits the room. Its columns and its rows shrink by the same factor, so its shape stays. A picture never grows. A picture of only spaces and blocks of quadrants and halves shrinks as pixels. Each cell holds two by two pixels, and each new pixel is the mean color of the pixels that it covers. Each new cell gets the block and the two colors that show its four pixels best, and a part with no color stays with no color. Any other picture, such as text or braille, keeps the nearest cell. The app fits the picture once for each size of the window.

Home lists the tickets in progress and the queue of the tracker. Each ticket in progress shows its place on the path of the adapter, what happens now, its cost, and its time. Enter opens the live view of a ticket in progress, or the start card for a ticket of the queue. On a stopped or paused ticket, Enter opens the continue card. The start card names the clone, the branch, the path, the first agent with its model, and the modes of the write verbs. A refresh keeps the selection on the same ticket.

The box MY TICKETS lists the queue that `queue_jql` asks for, such as each open ticket of the person. Its title line says how many tickets it lists. A ticket that can start shows bright. A ticket that is in progress outside Vision shows bright too, and Enter opens its start card (section 15.3). Each other ticket shows faint, with the reason that it cannot start.

MY TICKETS has one section for each status of the tracker. The statuses of `app.sections` come first, in that order, and each other status follows in the order of the queue. The tickets of one status keep the order of the queue, and the arrows follow the same order. The top line of the box and each line between two sections show the status and its count, such as `In Review · 3`. When the box shows only a part of its rows, its top line names the section of the first row in view. The marks on its border count only tickets.

One state function says what happens to a ticket now, in the words of the app, and Home and the live view both use it. It reads facts that the pipeline gives as data: the turn that runs, the entry of the step that is not done, the wait of the step, and the limit. No text of it names a command.

The live view shows one ticket. The path is on the left, with the time and the cost of each step, and the team is under it. The team ends above the line over the keys. If the team has more agents than rows, the agents that work and the agents that did a turn come first. The last row then says how many more there are. NOW is in the middle, with ACTIVITY under it. The cost, the questions, the marks, the limits, and the engine are on the right. While the view is open, the loader looks for a change of its data each second. The count of the commits that the forge does not have loads again each 10 seconds.

ACTIVITY joins two sources by time, with the newest line at the bottom. The first source is the stream files of the last six turns of the ticket: what the agent says, and the end of a session with an error. A thread reads their new lines four times each second. For a turn that does not run when the view opens, the first read takes only the end of its stream file: the last lines that hold the items that the feed keeps. The feed wraps its items into lines again only after a change of the items, of the width, of the tab, or of the time offset. The feed keeps the numbers of the 12 newest turns. The second source is the timeline of the ledger. A stream line holds no time, so a line that was in the file before the view opened shows no time.

A tool call gets no line. What the turn gets, the start of its session, and its end with a result get no line either, because the lines of the turn in the ledger say the same in words. The count of the tool calls stays in NOW, and in the log of `l`, Enter on a turn shows each of these events. The feed keeps a bounded number of items, and it counts only the items that it shows. A scroll stops the follow, and `G` goes back to the end.

A row of tabs above the feed shows one kind of item, or all of them: All, Agents, Questions, Steps, Writes, Problems, and You. Each tab shows its count, and each item has exactly one kind. Agents has what an agent says, the start and the end of a turn, and the reads that Vision serves. Questions has the cards, their answers, and the Judge. A use of the Judge names the principles that its answers used, such as `the Judge used P-001 and P-002`. A use that names no principle shows only in the log. Steps has the moves between steps, the end facts that held, the documents, and the end of the ticket. Writes has the operations.

Problems has a flag, a session that ended with an error, and a turn or an operation that failed. It also has an end fact that did not hold, a card that was not valid, and a notice. You has the other actions of the person and each message that the person sent, such as `told builder: keep the old export`, or `told the next turn: ...` when no turn ran. The tab stays while the app runs, also for the next live view. A change of the tab shows the end of its list. The scroll and the follow work inside the tab.

These keys act on the ticket of the live view:

- `t` opens a line that sends a text to the running turn, or keeps it for the next turn.
- `n` opens the first thing of the ticket that waits for the person.
- `o` opens the session of the last turn in the same terminal.
- `x` stops the ticket. Enter on a stopped or paused ticket opens the continue card.
- `r` retries a ticket that waits for the person or that is paused, and it reads no rule of a continue.
- `l`, `p`, and `d` open a pager with the log, the plan, or the commits that the forge does not have. The footer names `l` as the log and the tool calls.
- `f` shows the next tab of ACTIVITY, and `F` shows the tab before. The digits 1 to 7 pick a tab.

If the footer is too narrow, its hints change in this order until it fits: `d` and `p` go, the hint of `t` becomes `tell`, `o` and `x` go, and the hint of `l` becomes `log`. So `f` stays at each width of 100 columns or more.

Enter on a stopped or paused ticket opens the continue card. This works on Home, in the live view, on the card of the things that wait, and on the card after a session. The card names the stored step. While `observe` runs, it says "Vision reads the facts of KEY now" with the time of the read so far. Under it, it says what the read asks for. The start card and the live view also say what they read and for how long, while they read. Then it shows the suggestion with its reason, or that Vision found nothing further, or why the check failed. Under it, the card lists each fact that Vision read, after "Vision read:". A suggestion that goes back shows in the color of a warning, with the sentence "This goes back from <step>: <why>". It lists the steps of the path, with the suggestion selected and the stored step marked. The window of the steps keeps the stored step and the selection in view while both fit. The arrows, the wheel, and a click select a step, and Enter continues there. Esc closes the card, and the ticket stays as it is. `v` closes the card and opens the live view of the ticket.

For a stopped ticket, Enter runs `vision take KEY --step STEP`. Then a plain `vision retry KEY` follows at the stored step for a ticket that is also paused or that waits for the person. For a paused ticket, Enter runs `vision retry KEY --step STEP`. A clone that stays behind the remote after the fetch gets a line that says how to fix it. After a failed step of the chain, the card reads the ticket again.

The same card starts a ticket that is in progress outside Vision, with the title "Start KEY". Its head names the branch and the pull request of the ticket. Its read is the read of a plain take (section 11.3), so it compares no branch. No step is the stored step, and the end step is not in the list. With no suggestion, the first step is selected. Enter runs `vision take KEY --step STEP`, and Home then selects the ticket. The card has no `v`, because the ticket has no live view yet.

In the log, Enter on a turn shows the events of that turn. Esc closes a pager, and then the view.

The key `n` opens the first thing that waits for the person as a card. On Home, this is the first thing of all tickets. The questions of one asking turn, or of one ask step, are one thing: the question form. If more than one thing waits, the title says which one this is, such as `2 of 3`. The left and right arrows go to the other ones. Esc closes the card, and the thing keeps waiting. After an answer, the next card shows, or the card closes with a notice.

The live view shows the kind of the ticket. For a team step, NOW shows one line with the state of each member, such as `team: repro done, cause runs, history runs`. The cards of a batch show as one question form, as the cards of a turn do. Models lists the agents that the adapter defines after those of Vision, with the source `adapter`. In the line BY AGENT of Stats, they come behind the label `adapter:`. The start card and the team list of the live view name an agent of the adapter with its model. On an act step, NOW names the write that runs or waits, such as `the write pr.open runs at the next pass`. Its last line names the verbs of the step in their order, such as `No agent works. The step runs git.push, then pr.open.` The path shows an act step as each other step.

Each kind of thing has its card:

- The questions of one asking turn, or of one ask step, show as one form. The paragraphs after this list describe it.
- A plan shows its text and its short hash. `a` approves the full hash that the card showed, and `x` stops the ticket.
- A write in the mode `ask`, or a held diff, shows what it sends. `y` approves it. In the mode `ask`, `a` approves it and moves its verb to the mode `auto`.
- The first write of a verb in the mode `shadow` or `hold` shows the text that the mode kept back. `y` sends it and moves the verb to the mode `ask`. `a` sends it and moves the verb to the mode `auto`. `n` leaves it.
- A rule that stopped a ticket shows its reason. `r` tries the step again, `o` opens the session, and `x` stops the ticket. On an act step, `r` runs the writes of the step again that are not done, and no agent starts.
- A paused ticket takes Enter, which opens the continue card.
- A proposal of the Curator takes Enter. The app then gives the terminal to `vision memory review --proposal NAME`.
- A notice lists the issues that a passing gate qa found and that do not block, each with its id, its severity, and its title. Enter marks it read. A notice stays until the person reads it. It never stops a ticket, never marks the ticket as one that needs the person, and comes after each other thing that waits.

The question form has a page for each question, and a review page after the last one. Its title names the ticket, who asks, and the page, such as `builder asks at the step build · Question 2 of 3`. A row under the title marks each question: `✓` for a draft, `◇` for a proposal of the Judge, and `·` for an open question. A question page shows the question and the draft of the person with its reason. Under them are the proposal of the Judge, why the agent asks, the example, the choices, the recommendation, and the evidence. The form opens on the first question with no draft, or else on the review page.

A key of a question page keeps only a draft, and nothing goes to Vision before the send. A letter takes that choice. Enter takes the draft, else the proposal of the Judge, else the recommended choice. `e` or `t` opens a line for an answer in the words of the person. `k` or `-` marks a needless question and asks where its answer was.

After a choice or a text, a line asks for the reason. Enter keeps the draft with the reason, and Tab keeps it with no reason. A line opens with the old text of the draft. After a draft, the form shows the next question with no draft, or else the review page.

The left and right arrows go through the pages. Past the two ends of a form, they go to the thing before or after it. A form that the arrows reach from the right opens on its review page. A digit opens the question with that number, and `r` or `s` opens the review page.

The review page lists each question in two lines, with its draft and its reason, and marks each question with no draft. The up and down arrows choose a row, and Enter opens its question. `s` sends the form. The send needs a draft for each question. If a question has none, a notice names it, and nothing is sent.

The send is one action: `vision answer --file` with the draft of each question (section 8.4). Each answer is recorded, or none. If Vision refuses an answer, the review page marks that question with the reason, and each draft stays. After the send, the form goes away. If Vision saved each answer but the ledger did not finish their records, the form goes away too. The notice says that Vision saved the answers and that a later pass finishes their records. The draft of each answer stays until its record is final. A notice says who gets the answers: the asking agent at its next pass, or `answers.md` for an ask step.

The drafts stay in `tickets/<KEY>/answer-drafts.json`, in the format of `vision answer --file`. The app writes the file after each change of a draft, so the drafts survive a close of the form and a restart of the app. The send removes the drafts of its questions. If the Judge or another command answers a question of the form, the question leaves the form with a note, and its draft goes. At the first load of the card, each draft whose question has a final answer goes. The note says who answered: the Judge or the person. A draft of an answer whose record waits in `recording` stays.

The view records each part that a click can hit in the same code that draws it. A click on a key hint acts as that key. This holds for each hint of a footer and of a card, such as `n needs` or `enter open`. A hint of more keys, such as `↑↓` or `pgup pgdn`, has a part for each key, and its label is no part. The pagers show `g start` and `G end` as two hints, so that each label is a part.

The header has a back part `‹` and a forward part `›`, as a browser has. The app keeps the places that the person visited. A place is a screen, and for the live view, the pagers on top of it. A key or a click that goes to another place puts the place before it on the back list and empties the forward list. Back goes to the last place of the back list, and forward goes to the place that back left. Each list keeps 50 places.

The header also names each place on the way from Home, such as `◆ VISION ▸ DEMO-12 ▸ log ▸ turn build-1 of 14:24`. A click on a name goes to its place, and the name of the place that shows is no part. A return to a live view opens its pagers again with the loads of their texts. A log waits for the data of the ticket. Leaving Models drops each change that is not pinned, with a notice, as Esc does. A pager that closes after its load, such as the plan of a ticket without a plan, leaves no place in the history.

A click on a row of a list selects the row. A click on the selected row opens it, as Enter does. The lists are the tickets of Home, the agents of Models, the rows of the log, and the rows of the review page. On Models, two clicks on the selected agent pin its change, as Enter does. If the agent has no change, the click says how to make one.

A click on a choice of the question form takes that choice, as its letter does. An open line of the reason keeps its text. A click on a tab of the question row opens that page, and two clicks on the send send the form. A click on a choice of a write card acts as its letter.

A click on a tab of the activity shows that tab. The marks `‹` and `›` show the tab before and the tab after. The mark `▲ N below` acts as `G`. The bar "needs you" of Home acts as `n`. In NOW, the box of a ticket that needs the person acts as its key. On the selected agent of Models, `‹` and `›` act as the arrows, and the effort acts as `e`. A click on the loader skips it.

In the live view, a click on a step of the path that the ticket visited opens the log. The cursor of the log is on the last line that names the step. A click on a line of the activity that belongs to a turn opens the events of that turn. In the right column, a click on the turns opens the log as `l` does. A click on the commits of a ticket with a clone opens them as `d` does. A click on a plan opens it as `p` does.

A click on the mark `▼ N more` of a box of Home or of Models selects the first row below the box. The mark `▲ N more` selects the last row above it.

A click on a row of the keys card closes the card, and the screen acts as that key. So each key of a screen works with the mouse, also a key that a narrow footer leaves out. A row of more keys, such as `l  p  d`, has a part for each key. On Models, the rows of Enter and `u` are no parts, because these keys record a decision. Their hints in the footer arm on the first click.

The wheel acts as the up and down arrows on the part under the pointer. One step scrolls three lines of the activity, of a pager, or of the body of a card. On Home, on Models, and on the review page, one step moves the selection one row.

A part that sends something or records a decision acts only on a second click. The first click arms the part. Its label says `click again`, and nothing else changes. A second click on the same part within 3 seconds acts as its key. A click elsewhere, a key, a paste, a step of the wheel, and the end of the 3 seconds take the arm away. The arm ends on the first message after its time, such as the tick of each second, so no fast loop runs for it. A typed key acts at once, as before. If a different thing replaces the card between the two clicks, the arm goes, so a second click never acts on a thing that the first click did not see.

Enter arms on the cards that start, stop, or retry a ticket, on the continue card, on the kill switch, and on the quit card. On the open card, Enter and `o` arm. On the card of the things that wait, `a` of a plan, `y` and `a` of a write, `r` of a rule, and Enter of a notice arm. The send of the question form, the send of the line of `t`, and Enter and `u` of Models arm too.

A card takes each click, and the parts of the screen under it do not act. A click outside each part does nothing. A click with Shift, Alt, or Ctrl does nothing. While a line is open, a key goes into the line. So the box of NOW and the mark `▲ N below` are no parts while the line of `t` is open. The header, the steps of the path, the lines of the activity, and the links of the right column are no parts then too. So a click cannot drop the text of the line. The keys card of `?` has four more lines: what a click does, which keys need two clicks, what the back and forward parts do, and how to select text.

A key of a card can start a chain. A chain is a list of steps that run one after the other. A step is a command, a pass of the engine, or a check. A check reads the ledger and the configuration while no job runs, and no job starts during the read. The card shows each step with its mark. If a step fails, the chain stops, and the card says why and what is true for the ticket now. If the person quits the app, the rest of the chain does not run.

The foot of a card of the things that wait does not scroll. If a long reason of a failure does not fit, the card shows its first lines and says that the rest is cut. The state and the keys always stay in the window.

The app acts only on the exact thing that the card showed. The approval of a plan names its full hash. The approval of a write names its operation with `--op`. Before it, a check reads that the same operation still waits, with the same commit and the same arguments. If the thing changed, the app approves nothing, and the card shows the new thing with a note.

The key `y` of a first write runs this chain:

1. A check of the mode of the verb and of the step of the ticket.
2. `vision promote`, one or two times, with a check of the mode between them.
3. `vision retry`.
4. A pass.
5. A check that the same write waits.
6. `vision approve`.

If the pass did not ask for the write yet, one more pass runs, up to three passes. The key `a` promotes up to the mode `auto`. Its last check reads that the write ran.

While a turn runs, the card of `o` has a second `o`. It stops the turn with `vision stop` and gives the terminal to `vision open`. The turn that ran is lost, and its session keeps what the agent did. After the session, the card says that the ticket is stopped and paused, and Enter opens the continue card. The next turn then resumes the session of the person.

`m` on Home opens Models. It has one row for each agent: its model, its effort, pinned or the default, and its measured turns with their mean cost. With more agents than fit above the box of the recommendation, the rows show a window that follows the selection. A mark above and a mark below the rows say how many agents are out of view. The left and right arrows change the model, and `e` changes the effort. Enter runs `vision model set`, and `u` runs `vision model reset`. A change that is not pinned has a mark, and Esc drops it. A change holds for the next turn of the agent, not for a turn that runs now.

`s` on Home opens Stats. It shows the cost, the tickets that finished, the questions that got an answer without the person, and the minutes of the person. A table lists the tickets. Bars show the time and the cost of each step, in the order of the adapter. A last line gives the cost of each agent. The cost of a turn with no ticket, such as a turn of the Curator, shows apart, because the report does not count it. `w` switches between the last 7 days and all time. Stats shows the line REVIEW ESCAPES once a review of a review bot was checked: the checked pull requests, the escapes by priority, and the escapes for each checked pull request. A notice of the gate cr can say that a finding keeps coming back after its fix, with its first line.

### 15.4 The terminal

While the app owns the terminal, standard error goes to `log/app.err`. The terminal comes back on each way out: the normal end, an error, a panic, and the signals SIGTERM, SIGHUP, and SIGINT. Ctrl-C is the key `q`. In the line of `t`, it cancels the line as Esc does. A paste arrives as one piece, and only the line of `t` and the lines of a card take it.

For `o`, and for the review of a proposal, the app gives the terminal to a child: `vision open` or `vision memory review`. The thread of the keys stops first, and the app waits for that stop. So the app reads no key of the session. The app ignores SIGINT in that time, so a Ctrl-C ends only the session. Standard error goes back to the terminal for the session. When the session ends, the app puts back the terminal modes of before and drops the input that nobody read. Then it takes the terminal and draws the whole screen again.

The thread of the keys waits in its poll until a key, a paste, an event of the mouse, or a change of the window comes. Before a child gets the terminal, the app sends SIGWINCH to itself. The signal wakes the poll, and the thread confirms its stop. The change of the window that the signal gives has the same size, so the screen does not change.

While the app owns the terminal, the terminal reports each press of a button and each step of the wheel, with the cells of SGR. The app uses the modes 1000 and 1006. It uses no report of a move, so a move of the mouse never wakes the app. Before the app turns on its modes, it turns off each mode of the mouse.

Each way out turns off each mode of the mouse, and so does the hand-over to a child. So `vision open` and `vision memory review` get a terminal with the mouse off. After the child, the app turns on its modes again. With the mouse on, a terminal selects text only with a modifier. Most terminals use Shift, and some use Option.

The thread of the keys sends a message only for a press of the left button and for a step of the wheel. A release, a move, a drag, another button, and an event with a modifier send nothing. A click acts on the parts of the last frame. A change of the window size clears the parts until the next frame. When a message changed the model and a click waits behind it, the loop draws first.

The app needs 100 columns and 28 rows. In a smaller window, it shows the size that it needs. If `COLORTERM` says `truecolor` or `24bit`, the app uses 24-bit color. If not, it uses the nearest of the 256 colors. With `NO_COLOR`, it uses no color, and it draws no android and no picture.

Text from outside is data. A ticket title, a status of the tracker, and the error text of a command lose each control character before they reach the screen. A cut goes by the display width of the text.

## 16. The journal and the archive

Each turn of a ticket leaves an entry in `journal/<KEY>/<turn-id>.md`, so that a later turn finds what an earlier one learned. The entry files are the source of truth. MemPalace is an index over them, and an object store keeps the turn folders. Neither is required.

At the end of each tick, the runner writes the entry of each turn of a ticket that ended in the last 7 days and whose facts changed. The entry has a front matter (the ticket, the step, the agent, the model, the times, the outcome, the head, the files, and the cost) and the sections Summary, Notes, Errors and fixes, Gate results, Answers, and Outcome. Each text passes the masking of `[secrets]`, and a second write gives the same file. Errors and fixes lists at most 20 failed Bash calls of 600 characters each. When a later call of the same command passed, it names the commit between them. An entry whose turn folder is gone is never rewritten.

A new text replaces the drawer of the same source in MemPalace: wing `journal.wing`, room the ticket key, source the entry path. The tick files at most 20 entries. When MemPalace does not answer, the log says so once for each tick, and a later tick or `vision journal reindex` files what waits. The search always asks MemPalace with `candidate_strategy` `union`, because the vectors alone miss an exact token. No raw transcript goes to MemPalace.

A work turn gets the best `journal.related` entries in `related.md`: for its ticket title, its diff or plan paths, and the error lines of its log or gate inputs. The turns of the session that it resumes are left out. With the index off or silent, a plain word search reads the files: more distinct words first, newest first among equals. `agents/common.md` tells each agent to search the journal before it debugs an error or designs a fix, to add notes at the end, and to cite the entries that it reused.

The archive uploads a turn folder as `<prefix><KEY>/<turn-id>.tar.gz` when its ticket ended or stopped, or one day after the turn ended. Each entry goes to `<prefix>journal/<KEY>/<turn-id>.md`. Vision runs the `aws` program, with `--endpoint-url` for another service with the same API, such as Cloudflare R2 or Backblaze B2. The credentials stay with that program, and no session gets them. Only an equal size from `aws s3api head-object` marks a turn archived. The tick uploads at most once an hour and deletes nothing. `vision archive` deletes a local turn folder `keep_days` after the confirmation, and only when its ticket no longer moves. `vision archive fetch TURN-ID` brings a folder back. `vision doctor` has a line `journal` and a line `archive`. Its test of MemPalace asks only for `initialize` and the list of tools.

`archive.dir` and `archive.bucket` together is an error. The archive writes into a local folder when `archive.dir` is set, or to a bucket when `archive.bucket` is set. A folder holds the same keys as paths under it, so a change of the configuration moves the archive later. A copy into the folder goes to a temporary name first and then takes its own name, and only an equal file size confirms it. `vision doctor` names the folder and its size. `vision archive move-to-bucket --from DIR` uploads each file of an old folder to the same key in the bucket of the configuration and confirms its size. Only then does it delete the local file. It leaves out names that start with `.`, deletes the folders that it empties, and keeps the folder itself.

Claude Code keeps one file of JSON lines for each session in `~/.claude/projects/<project folder>/<session id>.jsonl`, and the files of its subagents in `<session id>/subagents/`. It deletes all of them after 30 days. With `sessions.enabled`, Vision copies each `*.jsonl` file that has not changed for `sessions.quiet_hours` hours to `<prefix>sessions/<project folder>/<path>.gz`. It copies the file to a temporary folder first, hashes the copy, and packs it with the `gzip` program. A file that grew, such as a resumed session, goes again under the same key. Vision never writes, moves, or deletes a file of Claude Code. The tick runs this in the hourly run of the archive, with at most 40 files or 1 GB. `vision sessions archive` copies each file that waits. `sessions.enabled` without the archive is an error, and `sessions.wing` must differ from `journal.wing`.

Code builds a summary of each session, with no model call, in `session-journal/<project folder>/<session id>.md`. That folder is outside `journal/`, because a work turn reads `journal/`, and a summary holds the asks of each project of the person. The front matter holds the session id, the project folder, the working folder, the branch, the title, the times, the counts of the messages of the person and of the tool calls, and the key in the archive. The sections are Asks, Files, Errors and fixes, Commits, and Outcome. Asks holds the first line of each message that the person typed, at most 30. A message of the person is a user line with `origin.kind` `human`. Tool results, lines with `isMeta`, summaries of a compaction, task notices, and local commands such as `/clear` are left out. Each text passes the masking of `[secrets]` and of the forms of tokens that the gates detect, and the same masking applies to each turn entry. A summary has at most 20,000 characters. MemPalace files it in the wing `sessions.wing`, with the project folder as the room and the entry path as the source. The raw session text never goes to MemPalace.

`vision sessions search TEXT` asks MemPalace with `union` in that wing, or reads the summary files. Each hit shows the session id, the project, the day, the first ask, and the key. `vision sessions fetch SESSION-ID --to DIR` puts the session file and the files of its subagents back in DIR. It refuses `sessions.projects` and `~/.claude/projects`, and it writes over no file. `vision doctor` has a line `sessions`.
