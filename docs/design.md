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
  log/vision.log         one line for each tick action
  claude/                CLAUDE_CONFIG_DIR of the sessions, with its own login
  sandbox/               optional: <kind>.json replaces the settings file that Vision renders
  skills/                copies of the skills that the role files name
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
plan_approval = "always"

[claude]
bin = "claude"
config_dir = "~/.vision/claude"   # a missing key means the folder claude/ of the home. "" means the default login.
env_pass = []                     # more environment names that a session gets

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
  "branch": "vision/DEMO-12", "base": "9f3e21c",
  "documents": {"plan": ".vision/approved-plan.md", "refine": ".vision/refine.md",
                "answers": ".vision/answers.md"},
  "principles": ".vision/turns/20261116T143000Z-DEMO-12-build-1/principles.md",
  "skills": {},
  "expects": ["commits", "doc:pr.md"],
  "message": null
}
```

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

The runner renders `settings.json` for each turn. For a worker and a tester, it turns the sandbox on. It denies reads of the home folder, and it allows the clone, the folders of `sandbox.allow_read`, and the domains and sockets of `[sandbox]`. A reader gets no Bash tool, so its file needs no sandbox. Its file denies the Bash tool and the web tools.

Claude Code adds the tool `StructuredOutput` by itself, because each turn has a result schema. The contract of 7.4 expects that tool next to the tools of the kind. A test with a real session showed this.

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

The reasons are `result`, `error`, `timeout`, `contract`, `killed`, and `crash`. A later tick reads `turn.json` and the stream. A supervisor holds a lock on `supervisor.lock` while it lives. If `turn.json` says `running` and no process holds that lock, the tick sets the reason `crash`. The tick does not trust a process id, because a different process can get the same id later.

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
vision memory review               accept, edit, or reject each change of a proposal
vision memory log                  the history
vision memory revert COMMIT        make a commit that undoes one commit
```

The person can also edit the files with any tool. The next tick reads the change.

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
- A replay comes first. The Judge answers the last `curator.replay` questions that the person answered, with the proposed memory. The review shows how many answers now agree with the person and how many no longer agree. The replay hides the decision file of each answer that it replays, because the Judge has to find that answer again without it. In the proposed memory, each candidate of the proposal counts as active, so that the replay shows its effect.
- A person decides. Only a change that the person accepts reaches the main branch.

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

Each script reads one JSON object from standard input and prints one JSON object. Exit code 0 means done. Exit code 2 means that a gate refused the call. Each other code means a failure. Only the executor calls a script. The call object always has `config`, the `[adapter]` table of the configuration. A script gets each secret of `[secrets]` as an environment name.

```
observe  {"what": "queue", "config": {}}
      -> {"tickets": [{"key": "DEMO-12", "title": "...", "status": "To Do", "eligible": true, "reason": ""}]}
observe  {"what": "ticket", "key": "DEMO-12", "branch": "vision/DEMO-12", "pr": 7, "clone": "/path", "config": {}}
      -> {"issue": {}, "tracker_status": "In Progress",
          "pr": {"number": 7, "draft": true, "head": "abc", "merged": false, "closed": false, "conflict": false,
                 "approved_by": [], "changes_requested": false, "comments_waiting": 0},
          "ci": {"head": "abc", "state": "green"}, "envs": {"staging": {"head": "abc"}}}
act      {"verb": "git.push", "op": "op-id", "key": "DEMO-12", "clone": "/path", "branch": "vision/DEMO-12",
          "head": "abc", "args": {}, "config": {}}
      -> {"state": "confirmed", "external_id": "...", "detail": "...", "set": {"pr": 7}}
act      {"verb": "git.push", "op": "op-id", "reconcile": true, ...}
      -> the same object. The script only reads the outside state, and it never acts again.
job      {"job": "proof", "key": "DEMO-12", "clone": "/path", "base": "9f3e21c", "head": "abc", "args": {}, "config": {}}
      -> {"ok": true, "detail": "...", "documents": [{"name": "ci-log.txt", "content": "..."}],
          "checks": [{"kind": "spec", "result": "pass", "command": "...", "seconds": 12, "cause": ""}]}
```

- `state` of an `act` answer is `confirmed`, `failed`, or `uncertain`.
- `set` can give new values for the ticket row: `pr`, `head`.
- `documents` of a job go to the ticket folder. A later turn can read them.
- The `cause` of a check is `code`, `infra`, `flaky`, `env`, `not ours`, or `unknown`.

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
expects = ["commits", "clean", "doc:pr.md", "job:proof"]
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
every = 60                    # seconds between two observe calls

[[step]]
id = "done"
kind = "end"

[verbs."git.push"]
mode = "auto"                 # the mode at the start. [modes] of the configuration wins.
leaves = true                 # text or code leaves the machine, so the gates run first

[jobs.proof]
timeout = 1800
```

An entry of `after`, `before`, or `start` is a name, or a table with arguments:

```toml
after = ["git.push", { verb = "deploy", args = { env = "staging" } }]
before = [{ job = "app-start", args = { env = "local" } }]
with = { env = "staging" }    # a turn step: these values go into the facts file under "with"
```

A step can expect these end facts:

- `doc:<name>`: the result has that document, and it is not empty.
- `commits`: the branch has a commit that it did not have at the start of the turn.
- `clean`: the work tree of the clone is clean.
- `job:<name>`: the job passes at the last commit.
- `human:<action>`: the person did that action.

### 11.3 How a ticket moves

The ticket row holds the id of its step in `state`, its flags, and its counters. One tick moves a ticket at most one step.

- Entry of a turn step. The runner runs each `before` job. It asks `observe` for the ticket and writes `issue.json`. It builds the facts file, and it starts the turn.
- A turn that ends with `questions`. The cards go to the Judge. After each card has an answer, a new turn resumes the session with the answers.
- A turn that ends with `requests`. The executor serves each read verb, and a new turn resumes the session with the files.
- A turn that ends with `result`. The runner writes each expected document to the ticket folder. Then it tests each end fact.
- If an end fact does not hold, one new turn resumes the session with a message that names the fact. After a second failure, the ticket gets the flag `needs-you`.
- If each end fact holds, the `after` verbs run in order. After each one has the state `confirmed`, the ticket moves to `next`.
- A step with a `cap` counts each entry. One entry above the cap sets the flag `needs-you` with the open items.
- A wait step runs `observe` each `every` seconds, and it moves on the value of `watch`.
- A human step waits for its command. `vision needs` lists it.

The flags are `needs-you`, `paused`, `waits-for-limit`, and `stopped`. A ticket with a flag starts no new turn. `vision retry KEY` clears `needs-you` and enters the step again. `vision stop KEY` kills the running turn, frees the clone, and keeps the branch.

`vision take KEY` needs a free clone of `clones`. It fetches, and it makes the branch from `base`. A clone holds one ticket at a time.

The proof is a job. It runs the changed tests at the last commit, and they must pass. It runs the same tests on the base commit, and they must fail. A plan can declare the proof `green` for a change with no new behavior. Any new commit voids the proof.

### 11.4 The demo adapter

`adapters/demo` needs no network. Its tracker is a JSON file. Its forge is a bare git repo with a JSON file of pull requests. Its CI is a script of the project. The integration tests use it with the fake `claude`. A person can use it with the real one to try Vision on a toy repo.

## 12. The executor

No session can call the executor. It runs after a rule in code, after a typed command of the person, or after a turn whose end fact holds.

- Each verb has a mode: `hold`, `shadow`, `ask`, or `auto`. `hold` sets the flag `needs-you`. `shadow` records what the verb intends, does nothing, and sets the flag. `ask` waits for `vision approve KEY VERB COMMIT`. Only `vision promote` and `vision demote` change a mode.
- Each operation gets a row with the state `pending` and an idempotency key before it runs. The key is a hash of the ticket, the verb, the last commit, and the arguments. The states are `pending`, `executing`, `confirmed`, `failed`, and `uncertain`.
- At each tick, the executor reconciles each operation in the state `executing` or `uncertain`. It calls the script with `reconcile` set to true. An `uncertain` operation never runs again by itself.
- Before a verb with `leaves` runs, the gates test what leaves the machine. They refuse text or a diff that contains a secret of `[secrets]`. They refuse a diff above `gates.max_diff_lines`. They hold a diff that touches a path of `gates.sensitive` that the approved plan does not name.
- A human operation names its object with an id that cannot change: `approve-plan KEY HASH`, `answer ID`, `approve KEY VERB COMMIT`. A stale id is an error.
- One wrong operation moves its verb back to `ask`. The person marks an operation as wrong with `vision demote VERB`.

## 13. Commands

```
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

## 14. Rules for the code

- Rust, stable toolchain, one binary crate with a library. No async runtime. No daemon.
- Use only the crates of the list below. Ask before a new crate.
- Run `git` as a child process. Do not link a git library.
- Write each state file to a temporary name and rename it.
- Exit codes: 0 done, 1 error, 2 a gate refused, 3 not built yet.
- Each command that prints a table also takes `--json`.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` must pass.
- A test never calls the real `claude`, the network, or the real home folder. `tests/bin/fake-claude` plays a session from a script file. `VISION_CLAUDE_BIN` and `VISION_HOME` point a test at the fake and at a temporary folder.
- No name of a company, a person, or a private project goes into the repo.

```
clap  serde  serde_json  toml  toml_edit  rusqlite (feature "bundled")
anyhow  thiserror  time  uuid  sha2  libc  fs2
```
