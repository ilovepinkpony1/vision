-- Migration 3: the title of a ticket (section 5 of docs/design.md). `vision take` writes the
-- title that the queue of the tracker gives, so that the status view and the app show it after
-- the ticket left the queue. A ticket that was taken before this migration has no title.

ALTER TABLE tickets ADD COLUMN title TEXT;
