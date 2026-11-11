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
- Card: one question of an agent, with its choices, its recommendation, and its evidence.
- Clone: a full copy of the project repo that only Vision uses.
- Curator: the agent that proposes changes to the memory.
- Decision: one answered question with its reason and where it applies.
- End fact: a fact that the runner tests to know that a step is complete.
- Executor: the part of `vision` that holds the credentials and runs each operation.
- Judge: the agent that answers a card, proposes an answer, or sends the card to the person.
- Ledger: the SQLite store of the evidence.
- Operation: one change outside the machine, with a durable id and a state.
- Principle: one rule that the Curator distilled from decisions, or that the person wrote.
- Supervisor: the process that owns one running session.
- Turn: one Claude Code session that does one step, or continues one.
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
  running.json           the job or the operation that a pass runs now. It is gone when nothing runs.
  log/vision.log         one line for each tick action
  log/app.err            what library code prints to standard error while the app runs
  loader.ans             optional: your picture for the loader of the app (section 15.3)
  claude/                CLAUDE_CONFIG_DIR of the sessions, with its own login
  sandbox/               optional: <kind>.json replaces the settings file that Vision renders
  skills/                copies of the skills that the role files name
  agents/                optional: a file here replaces the role file or the schema of that name
    project.md           optional: the standing rules of the project, for each work agent
  memory/                a git repo (section 9)
    decisions/D-0001.md
    principles/P-001.md
    ARCHIVE.md
  adapters/<name>/       private adapters. They win over the adapters of the repo.
  tickets/<KEY>/
    refine.md plan.md plan.agent.md approved-plan.md answers.md pr.md review.md notes.md
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
clones = []                   # absolute paths of the clones that Vision owns

[claude]
bin = "claude"
config_dir = "~/.vision/claude"   # a missing key means the folder claude/ of the home. "" means the default login.
env_pass = []                     # more environment names that a session gets
env = {}                          # more variables with a fixed value that a session gets. Never a secret.
attribution = true                # false: no co-author trailer in a commit, no tool line in a pull request

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

Each time is RFC 3339 in UTC. Each JSON value is stored as text. The ledger records the evidence from the first ticket, because Vision cannot read later what it did not record.

## 6. The tick

`vision` is one binary with no daemon. A timer of the system runs `vision tick` each minute. One tick does these steps in order:

1. Take the lock. If a different tick holds it, exit with code 0.
2. Write the heartbeat. If the `off` file exists, write the snapshot and exit.
3. Reconcile each operation in the state `executing` or `uncertain`.
4. Collect each turn that ended. Write its row, its result file, and the limit reading.
5. For each ticket, read the facts of its step and test the end fact.
6. Select the next action of each ticket: start a turn, run a job, run an operation, set a flag, or nothing.
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
- `vision open KEY` attaches to the running turn. It prints the events as readable lines, and it sends each line that the person types. Ctrl-C leaves the view, and the turn continues.
- If no turn runs, `vision open KEY` pauses the ticket and starts `claude --resume <session id>` in the clone, in the terminal.

A message file is JSON: `{"text": "...", "source": "user", "at": "..."}`. Write it to a temporary name first, and then rename it. The ledger keeps each message in `messages`.

### 7.6 The result object

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

An agent never asks in the text of its answer. It ends its turn with cards. A card has the question, the reason for it, an example, two or more choices, a recommendation, and the evidence. `kind` is `product`, `architecture`, `scope`, or `code`. `topics` names each sensitive topic that the question touches. `area` names the part of the product in one to three nouns, and the decision file keeps it. `recommended` is the index of a choice, and the first choice has the index 0.

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
vision needs                         the cards and the approvals that wait, one at a time
vision answer ID --choice N | --text "T" [--why "W"] [--when "C"]
vision answer ID --needless code|ticket|memory|no-matter
vision answers [--days N]            the answers that the Judge gave alone, newest first
vision correct ID "RIGHT ANSWER" [--why "W"]
```

- `needs` shows one card, reads one key or one line, and then asks for two short lines: why, and where the decision applies. Both can stay empty.
- `--choice N` counts from 1, as the card shows the choices to the person. The index in the output of the Judge counts from 0.
- `correct` writes a decision of the person that replaces the answer of the Judge. The old file gets the status `superseded`. If the ticket still runs, its next turn gets the correction as a message.
- A decision with no reason is weaker. The Judge uses it only for a question with the same words and the same area.
- `answer` writes the `human` row of an answer in the same ledger transaction that sets the question to `recording`. The commit of the decision file comes after it. If the decision file cannot be written, the row goes again. So a crash after that commit never loses the row, and the next tick ends the record. `correct` writes its mark and its `human` row in one transaction too.

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
vision model set AGENT MODEL [--effort E]
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
| judge | `claude-fable-5-2` | high | It can answer alone, so a wrong answer costs the most. Its turns are short. |
| curator | `claude-fable-5-2` | high | It runs seldom, and its proposals change each later answer. |

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

The core knows five kinds of step: a turn, a job, a human action, a wait, and the end. The steps of a project are data in `adapter.toml`. The core has no word that names a project, a tracker, or a forge.

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

Each script reads one JSON object from standard input and prints one JSON object. Exit code 0 means done. Exit code 2 means that the script refused the call. Each other code means a failure. Only the executor calls a script. The call object always has `config`, the `[adapter]` table of the configuration. It also always has `base_ref`, the `base` of `adapter.toml`, such as `origin/main`. So the base branch has one place.

The environment of a script has only `PATH`, `HOME`, `LANG`, and `TMPDIR`. `bin/observe` and `bin/act` also get each secret of `[secrets]` as an environment name. `bin/job` gets no secret, because a job can run code that an agent wrote. A job that only reads the tracker or the forge sets `secrets = true`. The executor masks each secret value in an answer before it stores the answer.

```
observe  {"what": "queue", "config": {}}
      -> {"tickets": [{"key": "DEMO-12", "title": "...", "status": "To Do", "eligible": true, "reason": ""}]}
observe  {"what": "ticket", "key": "DEMO-12", "branch": "vision/DEMO-12", "pr": 7, "clone": "/path", "config": {}}
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

- `state` of an `act` answer is `confirmed`, `failed`, or `uncertain`.
- `set` can give new values for the ticket row: `pr`, `head`.
- `documents` of a job go to the ticket folder. The documents of an `act` answer with the state `confirmed` go there too. A later turn can read them. The runner masks each secret value in them. A script cannot write a document that the person or the pipeline owns, such as `plan.md` or `issue.json`.
- A reconcile call also has `external_id` and `started`. `external_id` is the id that the first answer stored, or `null`. `started` is the time of the first call.
- A read verb changes nothing outside. A turn can ask for it, and the documents of its answer go to that turn.
- If `act` ends with a different exit code, a signal, a time limit, or bad output, the operation becomes `uncertain`. The outside state is unknown then.
- The `cause` of a check is `code`, `infra`, `flaky`, `env`, `not ours`, or `unknown`.
- `ci.state` is `green` or `red` for a run that ended. Each other word means that no run ended at that commit. For each pair of commit and result, the runner writes one row in `checks` with the kind `ci`.
- Each run of a job also writes one row in `checks` with the kind `job` and the source `job:<name>`. Its result is `pass`, `fail`, or `error`. The end fact `job:<name>` reads that row at the last commit.
- A job with arguments writes the command `<name> <hash>` in that row. The hash is the first 16 hex characters of the SHA-256 of its arguments as JSON with sorted keys. An end fact with arguments reads only a row with the same command. So the job runs again after its arguments change at the same commit.

### 11.2 The steps

```toml
name = "demo"
branch = "vision/{key}"       # the branch of a ticket
base = "origin/main"          # the ref that a new branch starts from
# scripts = "../other-adapter"   optional: the folder whose bin/ answers the calls

[[step]]
id = "refine"
kind = "turn"                 # turn | job | human | wait | end
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
after = ["git.push", "pr.open"]
next = "review"

[[step]]
id = "review"
kind = "turn"
agent = "reviewer"
turn = "worker"
before = ["job:second-review"]    # jobs that run first. Their documents become inputs.
reads = ["approved-plan.md", "second-review.md"]
expects = ["clean", "doc:review.md", "job:proof"]
after = ["git.push"]
cap = "review_rounds"         # a key of [caps]. It counts each entry into the step.
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
```

An entry of `after`, `before`, or `start` is a name, or a table with arguments:

```toml
after = ["git.push", { verb = "deploy", args = { env = "staging" } }]
before = [{ job = "app-start", args = { env = "local" } }]
with = { env = "staging" }    # a turn step: these values go into the facts file under "with"
findings = "review"           # a turn step: each finding of its documents becomes a row with this source
on = { findings = "fix-qa", none = "ready" }   # a turn step with findings: it replaces next
```

A text argument of an entry can hold `{doc:NAME}`. The runner puts the text of that document of the ticket folder in its place. If the document does not exist, the ticket gets the flag `needs-you`, and the verb does not run.

An entry of `expects` can also be a table: `{ job = "<name>", args = {...} }`. Its arguments follow the same `{doc:NAME}` rule, with one difference. A missing document gives an empty text and no flag, and the job decides what an empty text means.

A turn step with the key `findings` can have `on` in the place of `next`. This `on` has exactly the keys `findings` and `none`. The loader refuses `on` without `findings`, `on` together with `next`, and a target that is not a step.

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

### 11.3 How a ticket moves

The ticket row holds the id of its step in `state`, its flags, and its counters. One tick moves a ticket at most one step.

- Entry of a turn step. The runner runs each `before` entry in order: a job or a verb. A job that does not pass sets the flag `needs-you`. A verb must reach the state `confirmed`. The visit remembers each entry that is done, so each one runs one time in a visit. The runner then asks `observe` for the ticket and writes `issue.json`. It builds the facts file, and it starts the turn.
- A turn that ends with `questions`. The cards go to the Judge. After each card has an answer, a new turn resumes the session with the answers.
- A turn that ends with `requests`. The executor serves each read verb, and a new turn resumes the session with the files.
- A turn that ends with `result`. The runner writes each expected document to the ticket folder. Then it tests each end fact.
- If an end fact does not hold, one new turn resumes the session with a message that names the fact. After a second failure, the ticket gets the flag `needs-you`.
- If each end fact holds, the `after` verbs run in order. After each one has the state `confirmed`, the ticket moves to `next`.
- A turn step with `on` moves by its findings. If the documents of its last turn hold a kept finding, the ticket moves to the step of `findings`. If they hold none, it moves to the step of `none`.
- A step with a `cap` counts each entry that comes from `next` or `on`. One entry above the cap sets the flag `needs-you` with the open items. An entry by `vision retry` or `vision take` does not count.
- A wait step first runs its `start` verbs. After each one has the state `confirmed`, it runs `observe` each `every` seconds, and it moves on the value of `watch`.
- The value of `watch` is a text. A path that the answer does not have gives `none`, and a `null` gives `none` too. If the step has `wait`, a value that is in neither `on` nor `wait` sets the flag `needs-you` at once.
- If a wait step has `limit`, a visit that watches for more than `limit` seconds sets the flag `needs-you`. The reason names the last value. The time counts from the first `observe` of the visit. `vision retry KEY` enters the step again and starts a new limit.
- A human step waits for its command. `vision needs` lists it. The one action is `approve-plan`.
- The end step frees the clone. If the pull request is merged, it records the event `ticket-merged`.

Each visit of a step is one row in `spans`. Its key `waits_on` says what the ticket waits for: `agent:<name>`, `job:<name>`, `person:<action>`, `watch:<path>`, or `end`.

The column NOW of `vision status` says the same thing for people. After a turn that ended well, it names the first `after` verb that is not `confirmed`, with its state. Two examples are `runs git.push` and `waits for your approval of review.reply`. Before a turn, it names the first `before` entry that is not done. The text `next: a turn of <agent>` means that a turn is the next action.

A ticket can have four flags. A ticket with a flag starts no new turn, no job, and no operation.

- `needs-you`: a rule stopped the ticket. `vision retry KEY` clears the flag and enters the step again. If the turn of the step ended well and an `after` verb stopped the ticket, `vision retry KEY` starts no new turn. It only requests the `after` verbs again.
- `paused`: the person took over the session with `vision open KEY`. `vision retry KEY` clears the flag and enters the step again. The new turn resumes the session, and its message says that the person worked in the session.
- `waits-for-limit`: a limit of section 4 refused a turn. The tick clears the flag after the limit allows a turn again.
- `stopped`: `vision stop KEY` killed the running turn and freed the clone. The branch stays in that clone. `vision take KEY` starts the ticket again at the same step.

A `failed` operation is final for its key. `vision retry KEY` frees the key, so that the verb can run again at the same commit. If an operation of the ticket is `uncertain`, `vision retry KEY` refuses, because the reconcile step must settle it first.

`vision take KEY` needs a free clone of `clones`. It fetches, and it makes the branch from `base`. A clone holds one ticket at a time.

The proof is a job. It runs the changed tests at the last commit, and they must pass. It runs the same tests on the base commit, and they must fail. Any new commit voids the proof.

The proof gets the approved plan in `args.plan`. A line `Proof: green` in the plan declares the whole proof green, for a change with no new behavior. The changed tests then only have to pass at the last commit. A line `Proof: green <path>` declares one test file green, and the proof does not run that file on the base commit.

### 11.4 The demo adapter

`adapters/demo` needs no network. Its tracker is a JSON file. Its forge is a bare git repo with a JSON file of pull requests. Its CI is a script of the project. The integration tests use it with the fake `claude`. A person can use it with the real one to try Vision on a toy repo.

Its path is the reference path: `refine`, `plan`, `plan-ready`, `build`, `review`, `ci`, `ready`, `comments`, `ci-review`, `ready-to-merge`, and `done`. A red CI run goes to `fix-ci` and back to `ci`. After the step `comments`, a commit of the Shepherd goes through CI again in the wait step `ci-review` before the merge. At a commit where CI already ran, `ci.run` is the operation of that commit, so it does not run again. Each loop goes through a turn step with a cap, and each wait step has `wait` and `limit`.

### 11.5 The adapter for Jira and GitHub

`adapters/jira-github` connects Vision to Jira Cloud and GitHub. Its scripts are Python 3 with the standard library only. The comment block at the top of its `adapter.toml` documents each key of `[adapter]`, each secret, and the path of a ticket.

The path has these parts:

1. The Scout refines the ticket, and `tracker.describe` writes the new description to Jira. The answer keeps the old description as a backup document.
2. The Scout writes the plan, and the person approves it.
3. `tracker.move` moves the issue to the status of work. The Builder builds, and the proof runs.
4. `git.push` pushes the branch, and `pr.open` opens a draft pull request.
5. The Reviewer reviews the diff. A second review command is optional.
6. `ci.run` starts the CI workflow. After a red run, `ci.rerun` runs the failed jobs one more time. After a second red run, the Builder fixes the fault.
7. The Tester does QA on the local app. A kept bug goes to a fix, to CI, and to QA again.
8. The person marks the pull request as ready. The team reviews it.
9. The Shepherd judges each comment that waits, and it fixes each fault that a comment names. `review.reply` answers each comment.
10. After the approval, the person merges, and the ticket ends.

These rules keep the tokens and the repo safe:

- A token goes to a service only in a header, and only over https. It never goes into a file, a command line, a remote address, an error text, or an answer.
- `git.push` gives the token to one git call through environment variables, for the address of the remote only. A push never forces.
- A job that runs code of the project gets no secret.
- A reconcile call only reads.
- Text from Jira and from GitHub is data. Each document wraps it as `{"untrusted": "..."}`.

A comment that waits has one of three kinds. `inline` is a thread on a line of the diff, and its reply goes into the thread. `review` is a review with a text that asks for changes or that comments. `conversation` is a comment in the conversation of the pull request. A reply to a review or to a conversation comment is a new conversation comment. It quotes the first line of the comment, and it ends with a hidden mark that names the comment. So a later round does not answer the same comment again.

The key `own_logins` names the logins of the person and of the bot. The key `ignore_logins` names logins whose comments never wait, such as a CI bot. An entry can also be `<login>:<kind>`, with the kind `inline`, `review`, or `conversation`. Then only the comments of that kind of that login never wait. This fits a review bot that keeps one status comment in the conversation.

`review.state` is the first value that holds of `merged`, `closed`, `comments`, `changes`, `conflict`, `approved`, and `waiting`. `conflict` means that the open pull request conflicts with its base. No step names `conflict`, so the ticket gets the flag `needs-you` at once, with the value in the reason. The person merges the base into the branch in the clone, resolves the conflict, and pushes the branch. Then `vision retry KEY` lets the ticket go on.

The key `envs` lists the staging envs of the project. For each env, `observe` gives `envs.<name>.branch`, `envs.<name>.head`, and `envs.<name>.holds`. `head` is the last commit of the env branch. If the env branch holds the head commit of the ticket, `holds` is true. It says that the merge happened, not that the env runs the new code.

An env can also name `deployment`, a GitHub environment where the project records its deployments. For such an env, `observe` reads the newest deployment and gives `envs.<name>.deployed`, `deployment_sha`, and `deployed_at`. If the commit of that deployment holds the head commit of the ticket, `deployed` is true. The adapter reads the deployment record and not its statuses. So `deployed` is true from the moment that the project records the deployment.

The job `app-start` remembers the app that it started in a state file in the temporary folder. The file holds the process id, the start time of that process as `ps` reports it, the head commit, and the log path. If that app still runs and the head commit changed, the job stops its process group and starts the app at the new head. The job never stops a process whose start time differs from the state file. The job trusts only a plain file of this user that no other user can write. An app that answers but that Vision did not start stays as it is, and the detail says that it can run older code.

`app_restart = false` turns the restart off, for an app that loads new code by itself. `app-<env>.json` names the head of the app in `head`, and `restarted` says whether the job started it again.

The key `test_canary` names test files that pass on the base commit. The proof runs them first in the worktree of the base commit. If one is missing or fails, that worktree cannot run tests. Then the proof does not hold, its check `proof-canary` has the cause `env`, and no changed test runs on the base. Without a canary, a worktree that cannot run tests looks the same as a new test that fails on the base.

`tests/e2e_jira_github.rs` drives the whole path against `adapters/jira-github/tests/serve_world.py`. That script is a fake Jira and GitHub with state on a free localhost port. No run against the real services is part of the tests.

## 12. The executor

No session can call the executor. It runs after a rule in code, after a typed command of the person, or after a turn whose end fact holds.

- Each verb has a mode: `hold`, `shadow`, `ask`, or `auto`. `hold` sets the flag `needs-you`. `shadow` records what the verb intends, does nothing, and sets the flag. `ask` waits for `vision approve KEY VERB COMMIT`. Only `vision promote` and `vision demote` change a mode.
- After `vision promote`, the next tick asks again for a write that the mode `hold` or `shadow` kept back. The executor judges it in the mode of now.
- Each operation gets a row with the state `pending` and an idempotency key before it runs. The key is a hash of the ticket, the verb, the last commit, and the arguments. The key of a verb with `once = true` leaves out the commit, so a newer commit does not end its operation. The states are `pending`, `executing`, `confirmed`, `failed`, and `uncertain`.
- At each tick, the executor reconciles each operation in the state `executing` or `uncertain`. It calls the script with `reconcile` set to true. An `uncertain` operation never runs again by itself.
- Before a verb with `leaves` runs, the gates test what leaves the machine. They refuse text or a diff that contains a secret of `[secrets]`, a private key, or a common form of a token. They refuse a diff above `gates.max_diff_lines`. They hold a diff that touches a path of `gates.sensitive` that the approved plan does not name. `vision approve` lets a held diff go.
- The secrets gate reads the diff from the base to the last commit. It also reads the patch of each commit between them, because a push sends each commit. It reads each file as text, so that an attribute of the repo cannot hide a file.
- The executor refuses an operation with a secret value in its arguments, for each verb.
- A new request of a verb at a newer commit ends the older operation of that verb that still waits. Its state becomes `failed`.
- A human operation names its object with an id that cannot change: `approve-plan KEY HASH`, `answer ID`, `approve KEY VERB COMMIT`. A stale id is an error.
- After a wrong operation, the person moves its verb down the ladder with `vision demote VERB`. Vision never changes a mode by itself.
- `vision approve` runs each operation of the verb that waits at that commit. With `--op ID`, it runs only the operation with that id. If that operation no longer waits at that commit, the command sends nothing.
- `vision approve` exits with code 2 after a gate refuses. It exits with code 1 after an operation fails or stays `uncertain`.

A pass can take minutes, and the ledger says nothing about a script until the script ends. So the executor writes `running.json` right before it runs a script. This holds for a job, an operation, a read verb, and the check of an uncertain operation. The file holds the kind, the name, the ticket, the start time, and the process id of the pass. When the script ends, the executor removes the file. A reader trusts the file only while that process holds the lock of the tick. The next tick removes a file that a crash left. The ledger stays the evidence.

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
vision plan KEY                    show the plan
vision approve-plan KEY HASH       approve the plan with that hash
vision approve KEY VERB COMMIT     approve one operation in the mode ask
vision log KEY                     the timeline of one ticket
vision logs [KEY] [--follow]       the events of the running turn
vision open KEY                    attach to the running turn and type, or take over the session
vision tell KEY "TEXT"             send a message to the running turn, or to the next one
vision diff KEY                    the commits of the ticket that are not on the forge
vision stop KEY | retry KEY        stop a ticket, or start its step again
vision needs | answer | answers | correct      section 8.4
vision memory ...                  section 9.2
vision curate                      run the Curator now
vision model ...                   section 10
vision promote VERB | demote VERB  move a verb on the trust ladder
vision report [--week]             the measures
vision install | uninstall         the timer of the system
```

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
- A test never calls the real `claude`, the network, or the real home folder. `tests/bin/fake-claude` plays a session from a script file. `VISION_CLAUDE_BIN` and `VISION_HOME` point a test at the fake and at a temporary folder.
- Three tests are the exception: `tests/real_claude.rs`, `tests/real_judge.rs`, and `tests/real_e2e.rs`. They are ignored, and they run only with `VISION_REAL_CLAUDE=1` and `--ignored`. `tests/real_e2e.rs` takes `DEMO-12` through the whole demo path with real sessions of the model `haiku`. `tests/real_claude.rs` also has a test for the commit of a worker, a test that the sandbox holds, and a probe. The probe runs the commands of `VISION_PROBE_COMMANDS` in the sandbox of a worker, in the folder of `VISION_PROBE_DIR`.
- No name of a company, a person, or a private project goes into the repo.

```
clap  serde  serde_json  toml  toml_edit  rusqlite (feature "bundled")
anyhow  thiserror  time  uuid  sha2  libc  fs2
ratatui (with its crossterm backend, for the app)
```

## 15. The app

If standard input and standard output are both terminals, `vision` with no arguments opens a full-screen program. Each command of section 13 stays, for scripts and for the timer.

### 15.1 The layers

The app has three layers. The model is plain data. The update takes a message, changes the model, and gives the effects to run. A message is a key, a tick of the clock, or the result of a background thread. The view draws the model. The update and the view read no clock and no file. So each screen has a text snapshot in `tests/snapshots/app/`, and no test needs a terminal.

Each read runs in a thread, in the process, through the functions that the commands use. These are the status view, the rows of `vision pick`, the report, the timeline, the plan, the commit list, the model table, and the adapter. The cards read the questions, the operations that wait, and the proposals of the Curator in the same way. Each change of the state runs its command as a child process of the same binary, with no standard input. The app shows its own notice for a success, and the text of standard error for a failure. One tested path does each change, no text of a command reaches the screen, and a crash of an action cannot stop the app.

### 15.2 The engine

The worker is one thread that runs one job at a time. A job is a pass or an action. A pass is the child `vision tick`. A pass starts at these times:

- The app starts.
- An action ends.
- A turn ends. This holds for a turn of the Judge and of the Curator too.
- One minute passed since the last pass.
- A pass changed a ticket, and the next pass has work for a ticket.

One pass does one thing for a ticket: it collects a turn, or moves the ticket to its next step, or starts the next turn. So the last rule starts the next pass at once, and a ticket does not wait a minute between two steps. A pass that changed nothing starts no such pass. At most 12 such passes run in a row, and the pass of the minute starts the count again. So a ticket that cannot move never makes the engine spin.

An action of the person goes before a pass that waits. A pass that finds the lock of a different tick is not an error, so the timer of section 6 can run too. A pass can take minutes, because a job of the adapter runs inside it. The screen stays live in that time. The header shows the pass with its time, and the state of the ticket names the job or the operation from `running.json`.

The person can pause the engine. Then no pass starts, and actions still run. One exception is the pass that a key of a card asks for (section 15.3): it runs while the engine is paused too. The pause lives only in the memory of the app. The kill switch of section 3 is a different thing: the app shows it and can switch it.

If the person quits the app, a running turn finishes by itself. The next step waits for the next start of the app, or for the timer.

### 15.3 The screens

The loader shows an android while the data loads. Its four stages are real: the home folder, the ledger, the tracker, and the engine. The loader leaves at the first of these events:

- The android appeared, and each stage is done or failed.
- The person pressed a key.
- The tracker stage took more than 10 seconds. The load then continues, and Home shows the queue later.

If the home folder has the file `loader.ans`, the loader shows that picture in place of the android. The file is text with color codes, as a tool such as `chafa` writes it. The app reads the file as data. Only the codes of color and bold count. Each other escape sequence and each control character goes away. The picture keeps at most 64 columns and 18 rows. A file that is empty, larger than 256 KB, or not valid UTF-8 counts as no file.

Home lists the tickets in progress and the queue of the tracker. Each ticket in progress shows its place on the path of the adapter, what happens now, its cost, and its time. Enter opens the live view of a ticket in progress, or opens the start card for a ticket of the queue. The start card names the clone, the branch, the path, the first agent with its model, and the modes of the write verbs. A refresh keeps the selection on the same ticket.

One state function says what happens to a ticket now, in the words of the app, and Home and the live view both use it. It reads facts that the pipeline gives as data: the turn that runs, the entry of the step that is not done, the wait of the step, and the limit. No text of it names a command.

The live view shows one ticket. The path is on the left, with the time and the cost of each step, and the team is under it. NOW is in the middle, with ACTIVITY under it. The cost, the questions, the marks, the limits, and the engine are on the right. While the view is open, its data loads again each second. The count of the commits that the forge does not have loads again each 10 seconds.

ACTIVITY joins two sources by time, with the newest line at the bottom. The first source is the stream files of the last six turns of the ticket: what the agent says, and each tool that it calls. A thread reads their new lines four times each second. The second source is the timeline of the ledger. A stream line holds no time, so a line that was in the file before the view opened shows no time. The feed keeps a bounded number of lines. A scroll stops the follow, and `G` goes back to the end.

These keys act on the ticket of the live view:

- `t` opens a line that sends a text to the running turn, or keeps it for the next turn.
- `n` opens the first thing of the ticket that waits for the person.
- `o` opens the session of the last turn in the same terminal.
- `x` stops the ticket, and Enter starts a stopped ticket again.
- `r` retries a ticket that waits for the person, or continues a paused ticket.
- `l`, `p`, and `d` open a pager with the log, the plan, or the commits that the forge does not have.

In the log, Enter on a turn shows the events of that turn. Esc closes a pager, and then the view.

The key `n` opens the first thing that waits for the person as a card. On Home, this is the first thing of all tickets. If more than one thing waits, the title says which one this is, such as `2 of 3`. The left and right arrows go to the other ones. Esc closes the card, and the thing keeps waiting. After an answer, the next card shows, or the card closes with a notice.

Each kind of thing has its card:

- A question takes the letter of a choice. Enter takes the proposal of the Judge, or else the recommended choice. `e` opens a line for an answer in the words of the person. `k` marks a needless question and asks where its answer was. After a choice or a text, a line asks for the reason. Enter saves the answer with the reason, and Tab saves it with no reason.
- A plan shows its text and its short hash. `a` approves the full hash that the card showed, and `x` stops the ticket.
- A write in the mode `ask`, or a held diff, shows what it sends. `y` approves it. In the mode `ask`, `a` approves it and moves its verb to the mode `auto`.
- The first write of a verb in the mode `shadow` or `hold` shows the text that the mode kept back. `y` sends it and moves the verb to the mode `ask`. `a` sends it and moves the verb to the mode `auto`. `n` leaves it.
- A rule that stopped a ticket shows its reason. `r` tries the step again, `o` opens the session, and `x` stops the ticket.
- A paused ticket takes Enter to continue.
- A proposal of the Curator takes Enter. The app then gives the terminal to `vision memory review --proposal NAME`.

A key of a card can start a chain. A chain is a list of steps that run one after the other. A step is a command, a pass of the engine, or a check. A check reads the ledger and the configuration while no job runs, and no job starts during the read. The card shows each step with its mark. If a step fails, the chain stops, and the card says why and what is true for the ticket now. If the person quits the app, the rest of the chain does not run.

The app acts only on the exact thing that the card showed. The approval of a plan names its full hash. The approval of a write names its operation with `--op`. Before it, a check reads that the same operation still waits, with the same commit and the same arguments. If the thing changed, the app approves nothing, and the card shows the new thing with a note.

The key `y` of a first write runs this chain:

1. A check of the mode of the verb and of the step of the ticket.
2. `vision promote`, one or two times, with a check of the mode between them.
3. `vision retry`.
4. A pass.
5. A check that the same write waits.
6. `vision approve`.

If the pass did not ask for the write yet, one more pass runs, up to three passes. The key `a` promotes up to the mode `auto`. Its last check reads that the write ran.

While a turn runs, the card of `o` has a second `o`. It stops the turn with `vision stop` and gives the terminal to `vision open`. The turn that ran is lost, and its session keeps what the agent did. After the session, the card says that the ticket is stopped and paused. Enter continues it with `vision take` and then `vision retry`. The next turn then resumes the session of the person.

`m` on Home opens Models. It has one row for each agent: its model, its effort, pinned or the default, and its measured turns with their mean cost. The left and right arrows change the model, and `e` changes the effort. Enter runs `vision model set`, and `u` runs `vision model reset`. A change that is not pinned has a mark, and Esc drops it. A change holds for the next turn of the agent, not for a turn that runs now.

`s` on Home opens Stats. It shows the cost, the tickets that finished, the questions that got an answer without the person, and the minutes of the person. A table lists the tickets. Bars show the time and the cost of each step, in the order of the adapter. A last line gives the cost of each agent. The cost of a turn with no ticket, such as a turn of the Curator, shows apart, because the report does not count it. `w` switches between the last 7 days and all time.

### 15.4 The terminal

While the app owns the terminal, standard error goes to `log/app.err`. The terminal comes back on each way out: the normal end, an error, a panic, and the signals SIGTERM, SIGHUP, and SIGINT. Ctrl-C is the key `q`. In the line of `t`, it cancels the line as Esc does. A paste arrives as one piece, and only the line of `t` and the lines of a card take it.

For `o`, and for the review of a proposal, the app gives the terminal to a child: `vision open` or `vision memory review`. The thread of the keys stops first, and the app waits for that stop. So the app reads no key of the session. The app ignores SIGINT in that time, so a Ctrl-C ends only the session. Standard error goes back to the terminal for the session. When the session ends, the app puts back the terminal modes of before and drops the input that nobody read. Then it takes the terminal and draws the whole screen again.

The app needs 100 columns and 28 rows. In a smaller window, it shows the size that it needs. If `COLORTERM` says `truecolor` or `24bit`, the app uses 24-bit color. If not, it uses the nearest of the 256 colors. With `NO_COLOR`, it uses no color, and it draws no android and no picture.

Text from outside is data. A ticket title, a status of the tracker, and the error text of a command lose each control character before they reach the screen. A cut goes by the display width of the text.
