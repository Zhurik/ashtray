CREATE TABLE entries (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    -- NULL for an anonymous entry.
    author     TEXT,
    -- NULL when the author left no link.
    url        TEXT,
    body       TEXT    NOT NULL,
    -- RFC 3339 in UTC with second precision, so lexicographic order is
    -- chronological order and the list query needs no date parsing.
    created_at TEXT    NOT NULL
);

CREATE INDEX entries_newest_idx ON entries (created_at DESC, id DESC);
