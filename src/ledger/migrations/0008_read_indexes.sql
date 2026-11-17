-- Migration 8: the indexes of the reads of one ticket (sections 5 and 15 of docs/design.md).
--
-- The app reads the state of each ticket in progress again after each change, and the live
-- view reads one ticket. Each of these reads names a ticket, a step, a state, or a kind, and
-- each index here serves one of them, so that a home with many finished tickets costs no
-- scan of a whole table. No row changes.

CREATE INDEX turns_ticket_step ON turns (ticket, step);
CREATE INDEX turns_status ON turns (status);
CREATE INDEX events_ticket ON events (ticket);
CREATE INDEX events_kind ON events (kind);
CREATE INDEX questions_ticket ON questions (ticket);
CREATE INDEX questions_state ON questions (state);
CREATE INDEX human_ticket ON human (ticket);
CREATE INDEX operations_ticket ON operations (ticket);
CREATE INDEX operations_state ON operations (state);
CREATE INDEX spans_ticket ON spans (ticket);
CREATE INDEX checks_ticket ON checks (ticket);
