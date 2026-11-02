-- 1 while the entry waits for someone to let it through, 0 once it is shown.
-- Entries written before the waitlist existed were already public, so they
-- start out at 0.
ALTER TABLE entries ADD COLUMN waitlisted INTEGER NOT NULL DEFAULT 0;

-- The list only ever reads shown entries, so the index can skip the rest.
DROP INDEX entries_newest_idx;
CREATE INDEX entries_shown_newest_idx ON entries (created_at DESC, id DESC)
    WHERE waitlisted = 0;
