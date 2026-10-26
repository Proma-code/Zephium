-- V37 is immutable. Preserve every existing identity/revision/dispatch; only
-- a later native NeverStarted receipt may create the new Skipped state.
ALTER TABLE codex_conversations RENAME TO codex_conversations_v37;
CREATE TABLE codex_conversations (
    conversation TEXT PRIMARY KEY NOT NULL CHECK (
        length(CAST(conversation AS BLOB)) BETWEEN 1 AND 40
        AND conversation NOT GLOB '*[^A-Za-z0-9-]*'
    ),
    incarnation BLOB NOT NULL CHECK (length(incarnation) = 16),
    grant_epoch INTEGER NOT NULL CHECK (grant_epoch >= 1),
    thread_id TEXT UNIQUE CHECK (thread_id IS NULL OR (
        length(CAST(thread_id AS BLOB)) BETWEEN 1 AND 128
        AND thread_id NOT GLOB '*[^A-Za-z0-9_-]*'
    )),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    dispatch INTEGER NOT NULL CHECK (dispatch IN (0, 1, 2, 3)),
    reply INTEGER NOT NULL CHECK (reply >= 0),
    turn_id TEXT CHECK (turn_id IS NULL OR (
        length(CAST(turn_id AS BLOB)) BETWEEN 1 AND 128
        AND turn_id NOT GLOB '*[^A-Za-z0-9_-]*'
    )),
    outcome INTEGER CHECK (outcome IS NULL OR outcome IN (1, 2, 3)),
    CHECK (
        (dispatch = 0 AND thread_id IS NOT NULL AND reply = 0 AND turn_id IS NULL AND outcome IS NULL)
        OR (dispatch = 1 AND thread_id IS NOT NULL AND reply >= 1 AND outcome IS NULL)
        OR (dispatch = 2 AND thread_id IS NOT NULL AND reply >= 1 AND turn_id IS NOT NULL AND outcome IS NOT NULL)
        OR (dispatch = 3 AND reply >= 1 AND turn_id IS NULL AND outcome IS NULL)
    )
) STRICT;
INSERT INTO codex_conversations(conversation,incarnation,grant_epoch,thread_id,revision,dispatch,reply,turn_id,outcome)
SELECT conversation,incarnation,grant_epoch,thread_id,revision,dispatch,reply,turn_id,outcome
FROM codex_conversations_v37;
DROP TABLE codex_conversations_v37;
