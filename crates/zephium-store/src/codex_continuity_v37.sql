CREATE TABLE codex_owner (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL CHECK (schema_version = 1),
    incarnation BLOB NOT NULL CHECK (length(incarnation) = 16),
    grant_epoch INTEGER NOT NULL CHECK (grant_epoch >= 1)
) STRICT;
CREATE TABLE codex_conversations (
    conversation TEXT PRIMARY KEY NOT NULL CHECK (
        length(CAST(conversation AS BLOB)) BETWEEN 1 AND 40
        AND conversation NOT GLOB '*[^A-Za-z0-9-]*'
    ),
    incarnation BLOB NOT NULL CHECK (length(incarnation) = 16),
    grant_epoch INTEGER NOT NULL CHECK (grant_epoch >= 1),
    thread_id TEXT NOT NULL UNIQUE CHECK (
        length(CAST(thread_id AS BLOB)) BETWEEN 1 AND 128
        AND thread_id NOT GLOB '*[^A-Za-z0-9_-]*'
    ),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    dispatch INTEGER NOT NULL CHECK (dispatch IN (0, 1, 2)),
    reply INTEGER NOT NULL CHECK (reply >= 0),
    turn_id TEXT CHECK (turn_id IS NULL OR (
        length(CAST(turn_id AS BLOB)) BETWEEN 1 AND 128
        AND turn_id NOT GLOB '*[^A-Za-z0-9_-]*'
    )),
    outcome INTEGER CHECK (outcome IS NULL OR outcome IN (1, 2, 3)),
    CHECK (
        (dispatch = 0 AND reply = 0 AND turn_id IS NULL AND outcome IS NULL)
        OR (dispatch = 1 AND reply >= 1 AND outcome IS NULL)
        OR (dispatch = 2 AND reply >= 1 AND turn_id IS NOT NULL AND outcome IS NOT NULL)
    )
) STRICT;
