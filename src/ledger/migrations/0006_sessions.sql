-- Migration 6: the archive of the sessions of Claude Code of the person (section 16 of
-- docs/design.md). One row for each session file that the archive knows.
--
-- `path` is the file relative to the folder of sessions.projects, such as
-- <project>/<session>.jsonl or <project>/<session>/subagents/agent-<id>.jsonl. `session` is the
-- id of the session that the file belongs to, and `main` is 1 for the file of the session
-- itself. `hash` and `size` describe the content that the archive holds, and `mtime` is the
-- change time of the file at that copy, in nanoseconds since 1970: a later run reads the file
-- again only when its size or its change time differ.
--
-- `key` is the key in the archive, `archived` the time of the confirmation, and
-- `archive_bytes` the size of the gzip file. `entry` is the summary of a main file, relative
-- to the home, with the hash of its text in `summary_hash` and the content hash that it was
-- built from in `summary_of`. `filed_hash` is the hash of the text that MemPalace holds, with
-- the time in `filed`. No row is ever deleted, and Vision never changes a session file.

CREATE TABLE sessions (path TEXT PRIMARY KEY, project TEXT NOT NULL, session TEXT NOT NULL,
                       main INTEGER NOT NULL, hash TEXT, size INTEGER, mtime INTEGER, key TEXT,
                       archived TEXT, archive_bytes INTEGER, entry TEXT, summary_hash TEXT,
                       summary_of TEXT, filed_hash TEXT, filed TEXT);
CREATE INDEX sessions_session ON sessions (session);
