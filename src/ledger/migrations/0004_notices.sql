-- Migration 4: the notices (section 15 of docs/design.md). A gate qa with notify = true that
-- passes records one notice for the issues of its report that do not block. A notice never
-- stops a ticket. It waits until the person reads it: `read` holds that time. `issues` is a
-- JSON list of {"id", "severity", "title"}. One turn gives at most one notice for each
-- document, so that a second pass over the same turn records nothing new.

CREATE TABLE notices (id INTEGER PRIMARY KEY, ticket TEXT, step TEXT, turn TEXT, gate TEXT,
                      doc TEXT, issues TEXT, note TEXT, created TEXT, read TEXT,
                      UNIQUE (turn, doc));
