-- Migration 1: the schema of section 5 of docs/design.md.
-- Each time is RFC 3339 in UTC. Each JSON value is stored as text.

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
