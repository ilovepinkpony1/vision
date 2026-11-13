-- Migration 5: the journal and the archive (section 16 of docs/design.md). One row for each
-- turn that has a journal entry or a turn folder that the archive knows.
--
-- `path` is the entry file, relative to the home, such as journal/DEMO-12/<turn>.md. `hash` is
-- the SHA-256 of its text, and `signature` is the hash of the facts of the ledger that the
-- entry shows, so that a tick writes the entry again only when one of them changed. `head` is
-- the last commit of the turn that the first write saw.
--
-- `filed_hash` is the hash of the text that MemPalace holds, or NULL. An entry waits for the
-- index while `filed_hash` differs from `hash`.
--
-- `archived` is the time when the object store confirmed the archive of the turn folder, with
-- its size in `archive_bytes`. `entry_sent` is the hash of the entry text that the object store
-- holds. `removed` is the time when the local turn folder was deleted. A journal entry and a
-- row are never deleted.

CREATE TABLE journal (turn TEXT PRIMARY KEY, ticket TEXT, path TEXT, hash TEXT, signature TEXT,
                      head TEXT, written TEXT, filed_hash TEXT, filed TEXT, archived TEXT,
                      archive_bytes INTEGER, entry_sent TEXT, removed TEXT);
