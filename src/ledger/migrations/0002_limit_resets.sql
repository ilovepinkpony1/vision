-- Migration 2: the reset time of each limit reading (section 5 of docs/design.md).
-- The limit gate ignores a reading after its reset time, so that an old reading never blocks
-- new turns. Each value is RFC 3339 in UTC.

ALTER TABLE turns ADD COLUMN five_hour_resets TEXT;
ALTER TABLE turns ADD COLUMN seven_day_resets TEXT;
