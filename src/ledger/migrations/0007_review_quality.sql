-- Migration 7: the quality of the review (sections 5 and 11 of docs/design.md): each finding of
-- a gate cr followed across the rounds of a ticket, the counts of each gate of a turn, and the
-- escape checks against the review bot of the team.
--
-- `findings` gets the place and the title of a finding. A row of the source `cr` is one
-- finding of one round of a gate cr: `path` and `line` come from its first line, `title` from
-- the line after it, and `track` names its row of `finding_tracks`. A row of the source
-- `escape` is one comment of a review bot that no finding of ours matched. `turn` is the turn
-- whose document held the finding. Each title is masked and cut to 200 characters.
--
-- `gate_rounds` has one row for each gate of a turn whose end facts were tested: `round` counts
-- the rounds of that kind of gate in the ticket from 1, `failing` is the number of items that
-- fail the gate (NULL when the gate cannot read each item of its document), and `critical` is 1
-- when a failing item is CRITICAL (cr) or critical (qa). `passed` is 1 when the gate held, at
-- the head `head`.
--
-- `finding_tracks` has one row for each finding that the rounds of a ticket follow. Two
-- findings of two rounds are the same when their fingerprints are equal, or when their paths
-- are equal and their lines are at most 3 apart. `came_back` counts the rounds where the finding
-- showed again after a round of the same gate where it was gone. `noticed` is the time of the
-- notice that this finding keeps coming back.
--
-- `escape_checks` has one row for each look at the first review of a review bot after a pass
-- of our gate cr at the head `head`. `review_id` is NULL until that review exists, and
-- `checked` holds the time of the check. `escapes` has one row for each comment of that review
-- of a counted priority that no finding of ours matched. No comment text is stored beyond its
-- title.

ALTER TABLE findings ADD COLUMN path TEXT;
ALTER TABLE findings ADD COLUMN line INTEGER;
ALTER TABLE findings ADD COLUMN title TEXT;
ALTER TABLE findings ADD COLUMN turn TEXT;
ALTER TABLE findings ADD COLUMN track INTEGER;

CREATE TABLE gate_rounds (turn TEXT NOT NULL, ticket TEXT NOT NULL, step TEXT, gate TEXT NOT NULL,
                          doc TEXT NOT NULL, round INTEGER NOT NULL, head TEXT, passed INTEGER,
                          failing INTEGER, critical INTEGER, findings INTEGER, at TEXT,
                          PRIMARY KEY (turn, doc));
CREATE INDEX gate_rounds_ticket ON gate_rounds (ticket, gate, round);

CREATE TABLE finding_tracks (id INTEGER PRIMARY KEY, ticket TEXT NOT NULL, fingerprint TEXT NOT NULL,
                             path TEXT NOT NULL, line INTEGER, severity TEXT, title TEXT,
                             first_round INTEGER NOT NULL, last_round INTEGER NOT NULL,
                             came_back INTEGER NOT NULL DEFAULT 0, noticed TEXT);
CREATE INDEX finding_tracks_ticket ON finding_tracks (ticket);

CREATE TABLE escape_checks (id INTEGER PRIMARY KEY, ticket TEXT NOT NULL, pr INTEGER, reviewer TEXT NOT NULL,
                            pass_turn TEXT NOT NULL, head TEXT, passed_at TEXT, looked TEXT,
                            review_id INTEGER, submitted TEXT, checked TEXT, comments INTEGER,
                            counted INTEGER, escapes INTEGER,
                            UNIQUE (ticket, reviewer, pass_turn));

CREATE TABLE escapes (id INTEGER PRIMARY KEY, check_id INTEGER NOT NULL REFERENCES escape_checks(id),
                      path TEXT, line INTEGER, priority TEXT, title TEXT, url TEXT);
CREATE INDEX escapes_check ON escapes (check_id);
