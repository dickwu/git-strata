//! The pinned schema (store.md §4.3), verbatim. `open` runs it in a
//! `:memory:` database and compares `sqlite_schema` row by row (§5.2 step 5).
//! Engine: Turso (see sql.rs); the gc gate table replaces the SQL function
//! `strata_gc_active()` the design names, and the four `(x IN (..)) = (y IS
//! NULL)` checks are spelled as AND/OR (Turso cannot type the `=` form in a
//! STRICT CHECK); the accepted rows are identical.

/// Every table, index and trigger, plus `application_id` and `user_version`.
pub(crate) const DDL: &str = r#"PRAGMA application_id = 1398035009;
PRAGMA user_version = 1;

CREATE TABLE deps_sets (
    repo        TEXT    NOT NULL,
    id          TEXT    NOT NULL CHECK (length(id) = 64 AND id NOT GLOB '*[^0-9a-f]*'),
    inputs_hash TEXT    NOT NULL CHECK (length(inputs_hash) = 64 AND inputs_hash NOT GLOB '*[^0-9a-f]*'),
    platform    TEXT    NOT NULL,
    toolchain   TEXT    NOT NULL,
    dirs        TEXT    NOT NULL CHECK (json_valid(dirs) AND json_type(dirs) = 'array'),
    origin      TEXT    NOT NULL CHECK (origin IN ('adopt', 'build', 'empty')),
    sealed_ns   INTEGER NOT NULL,
    PRIMARY KEY (repo, id),
    CHECK (origin <> 'empty' OR json_array_length(dirs) = 0)
) STRICT;

CREATE TABLE deps_attempts (
    id         INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id >= 1),
    repo       TEXT    NOT NULL,
    set_id     TEXT    NOT NULL CHECK (length(set_id) = 64 AND set_id NOT GLOB '*[^0-9a-f]*'),
    kind       TEXT    NOT NULL CHECK (kind IN ('adopt', 'build')),
    state      TEXT    NOT NULL CHECK (state IN ('running', 'sealed', 'failed', 'refused')),
    staging    TEXT    NOT NULL,
    pid        INTEGER CHECK (pid IS NULL OR pid > 0),
    started_ns INTEGER NOT NULL,
    ended_ns   INTEGER,
    error      TEXT    CHECK (error IS NULL OR json_valid(error)),
    CHECK ((state = 'running') = (ended_ns IS NULL)),
    CHECK ((state IN ('failed', 'refused') AND error IS NOT NULL) OR (state NOT IN ('failed', 'refused') AND error IS NULL))
) STRICT;
CREATE UNIQUE INDEX deps_attempts_one_running ON deps_attempts (repo, set_id) WHERE state = 'running';
CREATE INDEX deps_attempts_by_set ON deps_attempts (repo, set_id, id);

CREATE TABLE deps_proposals (
    id             INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id >= 1),
    repo           TEXT    NOT NULL,
    origin_session TEXT,
    from_set       TEXT    NOT NULL CHECK (length(from_set) = 64 AND from_set NOT GLOB '*[^0-9a-f]*'),
    from_inputs    TEXT    NOT NULL CHECK (length(from_inputs) = 64 AND from_inputs NOT GLOB '*[^0-9a-f]*'),
    to_set         TEXT    NOT NULL CHECK (length(to_set) = 64 AND to_set NOT GLOB '*[^0-9a-f]*'),
    to_inputs      TEXT    NOT NULL CHECK (length(to_inputs) = 64 AND to_inputs NOT GLOB '*[^0-9a-f]*'),
    tree           TEXT    CHECK (tree IS NULL OR (length(tree) = 40 AND tree NOT GLOB '*[^0-9a-f]*')),
    head           TEXT    CHECK (head IS NULL OR (length(head) = 40 AND head NOT GLOB '*[^0-9a-f]*')),
    state          TEXT    NOT NULL CHECK (state IN ('awaiting_publish', 'pending', 'applied', 'superseded')),
    superseded_by  INTEGER REFERENCES deps_proposals (id) DEFERRABLE INITIALLY DEFERRED,
    created_ns     INTEGER NOT NULL,
    updated_ns     INTEGER NOT NULL,
    CHECK (from_set <> to_set),
    CHECK ((tree IS NULL) = (head IS NULL)),
    CHECK (state <> 'awaiting_publish' OR tree IS NULL),
    CHECK (state NOT IN ('pending', 'applied') OR tree IS NOT NULL),
    CHECK ((state = 'superseded') = (superseded_by IS NOT NULL)),
    CHECK (superseded_by IS NULL OR superseded_by <> id),
    CHECK (origin_session IS NOT NULL OR tree IS NOT NULL)
) STRICT;
CREATE UNIQUE INDEX deps_proposals_one_pending ON deps_proposals (repo) WHERE state = 'pending';
CREATE UNIQUE INDEX deps_proposals_one_awaiting ON deps_proposals (repo, origin_session) WHERE state = 'awaiting_publish';
CREATE INDEX deps_proposals_by_superseded_by ON deps_proposals (superseded_by);
CREATE INDEX deps_proposals_by_origin ON deps_proposals (origin_session);

CREATE TABLE deps_main (
    repo        TEXT    NOT NULL PRIMARY KEY,
    tree        TEXT    NOT NULL CHECK (length(tree) = 40 AND tree NOT GLOB '*[^0-9a-f]*'),
    head        TEXT    NOT NULL CHECK (length(head) = 40 AND head NOT GLOB '*[^0-9a-f]*'),
    inputs_hash TEXT    NOT NULL CHECK (length(inputs_hash) = 64 AND inputs_hash NOT GLOB '*[^0-9a-f]*'),
    set_id      TEXT    NOT NULL CHECK (length(set_id) = 64 AND set_id NOT GLOB '*[^0-9a-f]*'),
    observed_ns INTEGER NOT NULL
) STRICT;

CREATE TABLE base_trees (
    repo       TEXT    NOT NULL,
    tree       TEXT    NOT NULL CHECK (length(tree) = 40 AND tree NOT GLOB '*[^0-9a-f]*'),
    files      INTEGER NOT NULL CHECK (files >= 0),
    created_ns INTEGER NOT NULL,
    PRIMARY KEY (repo, tree)
) STRICT;

CREATE TABLE manifest_entries (
    repo TEXT    NOT NULL,
    tree TEXT    NOT NULL,
    path TEXT    NOT NULL,
    blob TEXT    NOT NULL CHECK (length(blob) = 64 AND blob NOT GLOB '*[^0-9a-f]*'),
    mode INTEGER NOT NULL CHECK (mode IN (33188, 33261, 40960)),
    size INTEGER NOT NULL CHECK (size >= 0),
    PRIMARY KEY (repo, tree, path),
    FOREIGN KEY (repo, tree) REFERENCES base_trees (repo, tree)
) STRICT;

CREATE TABLE base_gens (
    id         INTEGER PRIMARY KEY CHECK (id >= 1),
    repo       TEXT    NOT NULL,
    tree       TEXT    NOT NULL,
    deps       TEXT    NOT NULL,
    created_ns INTEGER NOT NULL,
    UNIQUE (repo, tree, deps),
    UNIQUE (repo, id),
    FOREIGN KEY (repo, tree) REFERENCES base_trees (repo, tree),
    FOREIGN KEY (repo, deps) REFERENCES deps_sets (repo, id)
) STRICT;
CREATE INDEX base_gens_by_deps ON base_gens (repo, deps);
CREATE INDEX base_gens_by_age ON base_gens (repo, created_ns);

CREATE TABLE sessions (
    id         TEXT    NOT NULL PRIMARY KEY CHECK (id GLOB 's-[a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7]'),
    ticket     TEXT,
    root       TEXT    NOT NULL UNIQUE,
    lifecycle  TEXT    NOT NULL CHECK (lifecycle IN ('creating', 'active', 'removing', 'removed', 'unknown')),
    phase      TEXT,
    scene      TEXT    CHECK (scene IS NULL OR json_valid(scene)),
    created_ns INTEGER NOT NULL,
    removed_ns INTEGER,
    CHECK ((lifecycle = 'removed') = (removed_ns IS NOT NULL)),
    CHECK (lifecycle <> 'unknown' OR scene IS NOT NULL)
) STRICT;

CREATE TABLE workspaces (
    session    TEXT    NOT NULL REFERENCES sessions (id),
    repo       TEXT    NOT NULL,
    base_gen   INTEGER NOT NULL,
    head       TEXT    NOT NULL CHECK (length(head) = 40 AND head NOT GLOB '*[^0-9a-f]*'),
    path       TEXT    NOT NULL UNIQUE,
    tracked    INTEGER NOT NULL CHECK (tracked IN (0, 1)),
    generation INTEGER NOT NULL CHECK (generation >= 1),
    health     TEXT    NOT NULL CHECK (health IN ('ok', 'unknown')),
    scene      TEXT    CHECK (scene IS NULL OR json_valid(scene)),
    conflicts  TEXT    NOT NULL CHECK (json_valid(conflicts) AND json_type(conflicts) = 'array'),
    PRIMARY KEY (session, repo),
    FOREIGN KEY (repo, base_gen) REFERENCES base_gens (repo, id),
    CHECK (health <> 'unknown' OR scene IS NOT NULL)
) STRICT;
CREATE INDEX workspaces_by_base_gen ON workspaces (repo, base_gen);

CREATE TABLE seeds (
    session         TEXT    NOT NULL,
    repo            TEXT    NOT NULL,
    path            TEXT    NOT NULL,
    source          TEXT    NOT NULL,
    link            TEXT,
    size            INTEGER NOT NULL CHECK (size >= 0),
    source_mtime_ns INTEGER NOT NULL,
    copied_ns       INTEGER NOT NULL,
    PRIMARY KEY (session, repo, path),
    FOREIGN KEY (session, repo) REFERENCES workspaces (session, repo)
) STRICT;

CREATE TABLE move_intents (
    session    TEXT    NOT NULL,
    repo       TEXT    NOT NULL,
    n          INTEGER NOT NULL CHECK (n >= 2),
    to_tree    TEXT    NOT NULL CHECK (length(to_tree) = 40 AND to_tree NOT GLOB '*[^0-9a-f]*'),
    to_deps    TEXT    NOT NULL CHECK (length(to_deps) = 64 AND to_deps NOT GLOB '*[^0-9a-f]*'),
    head       TEXT    NOT NULL CHECK (length(head) = 40 AND head NOT GLOB '*[^0-9a-f]*'),
    staging    TEXT    NOT NULL,
    phase      TEXT,
    state      TEXT    NOT NULL CHECK (state IN ('preparing', 'swapping', 'done', 'refused', 'abandoned', 'unknown')),
    conflicts  TEXT    CHECK (conflicts IS NULL OR (json_valid(conflicts) AND json_type(conflicts) = 'array')),
    old_tree   TEXT,
    started_ns INTEGER NOT NULL,
    ended_ns   INTEGER,
    PRIMARY KEY (session, repo, n),
    FOREIGN KEY (session, repo) REFERENCES workspaces (session, repo),
    CHECK ((state IN ('preparing', 'swapping') AND ended_ns IS NULL) OR (state NOT IN ('preparing', 'swapping') AND ended_ns IS NOT NULL)),
    CHECK (old_tree IS NULL OR state = 'done'),
    CHECK (state = 'abandoned' OR (state IN ('preparing', 'refused') AND conflicts IS NULL) OR (state NOT IN ('preparing', 'refused') AND conflicts IS NOT NULL))
) STRICT;
CREATE UNIQUE INDEX move_intents_one_open ON move_intents (session, repo) WHERE state IN ('preparing', 'swapping');

CREATE TABLE checkpoints (
    session     TEXT    NOT NULL,
    id          INTEGER NOT NULL CHECK (id >= 1),
    repo        TEXT    NOT NULL,
    base_gen    INTEGER NOT NULL,
    at_ns       INTEGER NOT NULL,
    reason      TEXT    NOT NULL CHECK (reason IN ('tool_call', 'manual', 'before_publish', 'before_restore', 'before_rollout', 'before_remove')),
    reason_text TEXT,
    entries     INTEGER NOT NULL CHECK (entries >= 0),
    PRIMARY KEY (session, id),
    FOREIGN KEY (session, repo) REFERENCES workspaces (session, repo),
    FOREIGN KEY (repo, base_gen) REFERENCES base_gens (repo, id),
    CHECK ((reason IN ('tool_call', 'manual') AND reason_text IS NOT NULL) OR (reason NOT IN ('tool_call', 'manual') AND reason_text IS NULL))
) STRICT;
CREATE UNIQUE INDEX checkpoints_by_tool_call ON checkpoints (session, repo, reason_text) WHERE reason = 'tool_call';
CREATE INDEX checkpoints_by_repo ON checkpoints (session, repo, id);
CREATE INDEX checkpoints_by_base_gen ON checkpoints (repo, base_gen);

CREATE TABLE blobs (
    id         TEXT    NOT NULL PRIMARY KEY CHECK (length(id) = 64 AND id NOT GLOB '*[^0-9a-f]*'),
    size       INTEGER NOT NULL CHECK (size >= 0),
    created_ns INTEGER NOT NULL
) STRICT;

CREATE TABLE overlay_entries (
    session    TEXT    NOT NULL,
    checkpoint INTEGER NOT NULL,
    path       TEXT    NOT NULL,
    kind       TEXT    NOT NULL CHECK (kind IN ('added', 'modified', 'deleted', 'renamed', 'untracked')),
    blob       TEXT    REFERENCES blobs (id),
    from_path  TEXT,
    mode       INTEGER NOT NULL,
    PRIMARY KEY (session, checkpoint, path),
    FOREIGN KEY (session, checkpoint) REFERENCES checkpoints (session, id),
    CHECK ((kind = 'deleted') = (blob IS NULL)),
    CHECK ((kind = 'renamed') = (from_path IS NOT NULL)),
    CHECK ((kind = 'deleted' AND mode = 0) OR (kind <> 'deleted' AND mode IN (33188, 33261, 40960)))
) STRICT;
CREATE UNIQUE INDEX overlay_entries_by_from ON overlay_entries (session, checkpoint, from_path) WHERE from_path IS NOT NULL;
CREATE INDEX overlay_entries_by_blob ON overlay_entries (blob);

CREATE TABLE publishes (
    id         INTEGER PRIMARY KEY CHECK (id >= 1),
    phase      TEXT    NOT NULL CHECK (phase IN ('running', 'done', 'unknown')),
    started_ns INTEGER NOT NULL,
    ended_ns   INTEGER,
    outcome    TEXT    CHECK (outcome IS NULL OR json_valid(outcome)),
    CHECK ((phase = 'running') = (ended_ns IS NULL))
) STRICT;

CREATE TABLE publish_items (
    publish    INTEGER NOT NULL REFERENCES publishes (id),
    batch      INTEGER NOT NULL CHECK (batch >= 0),
    item       INTEGER NOT NULL CHECK (item >= 0),
    session    TEXT    NOT NULL,
    repo       TEXT    NOT NULL,
    checkpoint INTEGER NOT NULL CHECK (checkpoint >= 1),
    path       TEXT    NOT NULL,
    PRIMARY KEY (publish, batch, item)
) STRICT;
CREATE INDEX publish_items_by_checkpoint ON publish_items (session, checkpoint);

CREATE TABLE refs_audit (
    id        INTEGER PRIMARY KEY CHECK (id >= 1),
    at_ns     INTEGER NOT NULL,
    repo      TEXT    NOT NULL,
    op        TEXT    NOT NULL CHECK (op IN ('update_ref', 'push')),
    ref_name  TEXT    NOT NULL,
    old_oid   TEXT    CHECK (old_oid IS NULL OR (length(old_oid) = 40 AND old_oid NOT GLOB '*[^0-9a-f]*')),
    new_oid   TEXT    NOT NULL CHECK (length(new_oid) = 40 AND new_oid NOT GLOB '*[^0-9a-f]*'),
    outcome   TEXT    NOT NULL CHECK (outcome IN ('applied', 'head_moved', 'pushed', 'refused', 'unknown')),
    publish   INTEGER REFERENCES publishes (id),
    confirmed TEXT    CHECK (confirmed IS NULL OR json_valid(confirmed)),
    detail    TEXT,
    CHECK (op <> 'update_ref' OR (ref_name GLOB 'refs/strata/*' AND outcome IN ('applied', 'head_moved'))),
    CHECK (op <> 'push' OR outcome IN ('pushed', 'refused', 'unknown'))
) STRICT;
CREATE INDEX refs_audit_by_publish ON refs_audit (publish);

CREATE TABLE gc_runs (
    id          INTEGER PRIMARY KEY CHECK (id >= 1),
    scope       TEXT,
    started_ns  INTEGER NOT NULL,
    ended_ns    INTEGER NOT NULL,
    outcome     TEXT    NOT NULL CHECK (outcome IN ('complete', 'partial')),
    report      TEXT    NOT NULL CHECK (json_valid(report)),
    scrub_after TEXT    CHECK (scrub_after IS NULL OR (length(scrub_after) = 64 AND scrub_after NOT GLOB '*[^0-9a-f]*')),
    CHECK (scope IS NULL OR scrub_after IS NULL)
) STRICT;
CREATE INDEX gc_runs_by_scope ON gc_runs (scope, id);

-- The gc delete gate (engine: Turso has no host-registered SQL functions,
-- so the `*_delete_gc_only` triggers read this one-row table). gc sets
-- active = 1 as the first statement of its write transaction and back to 0
-- as the last one, so no committed state ever has active = 1.
CREATE TABLE strata_gc_gate (
    id     INTEGER PRIMARY KEY CHECK (id = 1),
    active INTEGER NOT NULL CHECK (active IN (0, 1))
) STRICT;
INSERT INTO strata_gc_gate (id, active) VALUES (1, 0);
CREATE TRIGGER strata_gc_gate_no_insert BEFORE INSERT ON strata_gc_gate BEGIN SELECT RAISE(ABORT, 'illegal insert: strata_gc_gate'); END;
CREATE TRIGGER strata_gc_gate_no_delete BEFORE DELETE ON strata_gc_gate BEGIN SELECT RAISE(ABORT, 'append-only: strata_gc_gate'); END;

CREATE TRIGGER deps_sets_no_update        BEFORE UPDATE ON deps_sets        BEGIN SELECT RAISE(ABORT, 'immutable: deps_sets'); END;
CREATE TRIGGER base_trees_no_update       BEFORE UPDATE ON base_trees       BEGIN SELECT RAISE(ABORT, 'immutable: base_trees'); END;
CREATE TRIGGER manifest_entries_no_update BEFORE UPDATE ON manifest_entries BEGIN SELECT RAISE(ABORT, 'immutable: manifest_entries'); END;
CREATE TRIGGER base_gens_no_update        BEFORE UPDATE ON base_gens        BEGIN SELECT RAISE(ABORT, 'immutable: base_gens'); END;
CREATE TRIGGER seeds_no_update            BEFORE UPDATE ON seeds            BEGIN SELECT RAISE(ABORT, 'immutable: seeds'); END;
CREATE TRIGGER checkpoints_no_update      BEFORE UPDATE ON checkpoints      BEGIN SELECT RAISE(ABORT, 'immutable: checkpoints'); END;
CREATE TRIGGER blobs_no_update            BEFORE UPDATE ON blobs            BEGIN SELECT RAISE(ABORT, 'immutable: blobs'); END;
CREATE TRIGGER overlay_entries_no_update  BEFORE UPDATE ON overlay_entries  BEGIN SELECT RAISE(ABORT, 'immutable: overlay_entries'); END;
CREATE TRIGGER publish_items_no_update    BEFORE UPDATE ON publish_items    BEGIN SELECT RAISE(ABORT, 'immutable: publish_items'); END;
CREATE TRIGGER refs_audit_no_update       BEFORE UPDATE ON refs_audit       BEGIN SELECT RAISE(ABORT, 'immutable: refs_audit'); END;
CREATE TRIGGER gc_runs_no_update          BEFORE UPDATE ON gc_runs          BEGIN SELECT RAISE(ABORT, 'immutable: gc_runs'); END;

CREATE TRIGGER sessions_insert_guard BEFORE INSERT ON sessions
WHEN NEW.lifecycle <> 'creating' OR NEW.scene IS NOT NULL OR NEW.removed_ns IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'illegal insert: sessions'); END;
CREATE TRIGGER sessions_update_guard BEFORE UPDATE ON sessions
WHEN NEW.id IS NOT OLD.id OR NEW.ticket IS NOT OLD.ticket OR NEW.root IS NOT OLD.root OR NEW.created_ns IS NOT OLD.created_ns
  OR (NEW.phase IS NOT OLD.phase AND NEW.lifecycle NOT IN ('creating', 'removing'))
  OR (NEW.scene IS NOT OLD.scene AND NOT (NEW.lifecycle = 'unknown' AND OLD.lifecycle <> 'unknown'))
  OR NOT ((NEW.lifecycle = OLD.lifecycle AND OLD.lifecycle IN ('creating', 'removing'))
       OR (OLD.lifecycle = 'creating' AND NEW.lifecycle IN ('active', 'unknown'))
       OR (OLD.lifecycle = 'active'   AND NEW.lifecycle = 'removing')
       OR (OLD.lifecycle = 'removing' AND NEW.lifecycle IN ('removed', 'unknown'))
       OR (OLD.lifecycle = 'unknown'  AND NEW.lifecycle = 'removing'))
BEGIN SELECT RAISE(ABORT, 'illegal update: sessions'); END;

CREATE TRIGGER workspaces_insert_guard BEFORE INSERT ON workspaces
WHEN NEW.generation <> 1 OR NEW.health <> 'ok' OR NEW.scene IS NOT NULL OR json_array_length(NEW.conflicts) <> 0
BEGIN SELECT RAISE(ABORT, 'illegal insert: workspaces'); END;
CREATE TRIGGER workspaces_update_guard BEFORE UPDATE ON workspaces
WHEN NEW.session IS NOT OLD.session OR NEW.repo IS NOT OLD.repo OR NEW.path IS NOT OLD.path OR NEW.tracked IS NOT OLD.tracked
  OR ((NEW.base_gen IS NOT OLD.base_gen OR NEW.head IS NOT OLD.head OR NEW.generation IS NOT OLD.generation OR NEW.conflicts IS NOT OLD.conflicts)
      AND NOT (NEW.generation > OLD.generation AND OLD.tracked = 1 AND NEW.health = 'ok'
               AND NEW.generation IS (SELECT MAX(n) FROM move_intents WHERE session = NEW.session AND repo = NEW.repo)))
  OR (NEW.health IS NOT OLD.health
      AND NOT (OLD.health = 'ok' AND NEW.health = 'unknown')
      AND NOT (OLD.health = 'unknown' AND NEW.health = 'ok'
               AND (SELECT lifecycle FROM sessions WHERE id = NEW.session) IS 'active'
               AND (SELECT state FROM move_intents WHERE session = NEW.session AND repo = NEW.repo ORDER BY n DESC LIMIT 1) IS 'unknown'))
  OR (NEW.scene IS NOT OLD.scene AND NOT (OLD.health = 'ok' AND NEW.health = 'unknown'))
BEGIN SELECT RAISE(ABORT, 'illegal update: workspaces'); END;

CREATE TRIGGER move_intents_insert_guard BEFORE INSERT ON move_intents
WHEN NEW.state <> 'preparing' OR NEW.old_tree IS NOT NULL OR NEW.ended_ns IS NOT NULL
  OR NEW.n <= (SELECT generation FROM workspaces WHERE session = NEW.session AND repo = NEW.repo)
  OR NEW.n <= (SELECT COALESCE(MAX(n), 0) FROM move_intents WHERE session = NEW.session AND repo = NEW.repo)
BEGIN SELECT RAISE(ABORT, 'illegal insert: move_intents'); END;
CREATE TRIGGER move_intents_update_guard BEFORE UPDATE ON move_intents
WHEN NEW.session IS NOT OLD.session OR NEW.repo IS NOT OLD.repo OR NEW.n IS NOT OLD.n OR NEW.to_tree IS NOT OLD.to_tree
  OR NEW.to_deps IS NOT OLD.to_deps OR NEW.head IS NOT OLD.head OR NEW.staging IS NOT OLD.staging OR NEW.started_ns IS NOT OLD.started_ns
  OR (NEW.phase IS NOT OLD.phase AND OLD.state NOT IN ('preparing', 'swapping'))
  OR (NEW.conflicts IS NOT OLD.conflicts AND NOT (OLD.state = 'preparing' AND NEW.state = 'swapping'))
  OR (NEW.old_tree IS NOT OLD.old_tree AND NOT (OLD.old_tree IS NULL AND OLD.state IN ('done', 'unknown') AND NEW.state = 'done'))
  OR NOT ((NEW.state = OLD.state AND OLD.state IN ('preparing', 'swapping'))
       OR (NEW.state = OLD.state AND OLD.state = 'done' AND NEW.ended_ns IS OLD.ended_ns)
       OR (OLD.state = 'preparing' AND NEW.state IN ('swapping', 'refused', 'abandoned'))
       OR (OLD.state = 'swapping'  AND NEW.state IN ('done', 'unknown'))
       OR (OLD.state = 'unknown'   AND NEW.state IN ('done', 'abandoned')
           AND NEW.n IS (SELECT MAX(n) FROM move_intents WHERE session = NEW.session AND repo = NEW.repo)
           AND (SELECT lifecycle FROM sessions WHERE id = NEW.session) IS 'active'
           AND EXISTS (SELECT 1 FROM workspaces
                       WHERE session = NEW.session AND repo = NEW.repo AND health = 'ok'
                         AND (generation = NEW.n) = (NEW.state = 'done'))))
BEGIN SELECT RAISE(ABORT, 'illegal update: move_intents'); END;

CREATE TRIGGER deps_attempts_insert_guard BEFORE INSERT ON deps_attempts
WHEN NEW.state <> 'running' OR NEW.pid IS NOT NULL OR NEW.error IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'illegal insert: deps_attempts'); END;
CREATE TRIGGER deps_attempts_update_guard BEFORE UPDATE ON deps_attempts
WHEN NEW.id IS NOT OLD.id OR NEW.repo IS NOT OLD.repo OR NEW.set_id IS NOT OLD.set_id OR NEW.kind IS NOT OLD.kind
  OR NEW.staging IS NOT OLD.staging OR NEW.started_ns IS NOT OLD.started_ns
  OR (NEW.pid IS NOT OLD.pid AND NOT (OLD.pid IS NULL AND NEW.state = 'running'))
  OR OLD.state <> 'running'
BEGIN SELECT RAISE(ABORT, 'illegal update: deps_attempts'); END;

CREATE TRIGGER deps_proposals_insert_guard BEFORE INSERT ON deps_proposals
WHEN NEW.state NOT IN ('awaiting_publish', 'pending') OR NEW.superseded_by IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'illegal insert: deps_proposals'); END;
CREATE TRIGGER deps_proposals_update_guard BEFORE UPDATE ON deps_proposals
WHEN NEW.id IS NOT OLD.id OR NEW.repo IS NOT OLD.repo OR NEW.origin_session IS NOT OLD.origin_session
  OR NEW.from_set IS NOT OLD.from_set OR NEW.from_inputs IS NOT OLD.from_inputs OR NEW.to_set IS NOT OLD.to_set
  OR NEW.to_inputs IS NOT OLD.to_inputs OR NEW.created_ns IS NOT OLD.created_ns
  OR ((NEW.tree IS NOT OLD.tree OR NEW.head IS NOT OLD.head) AND NOT (OLD.state = 'awaiting_publish' AND NEW.state = 'pending'))
  OR NOT ((OLD.state = 'awaiting_publish' AND NEW.state IN ('pending', 'superseded'))
       OR (OLD.state = 'pending' AND NEW.state IN ('applied', 'superseded')))
BEGIN SELECT RAISE(ABORT, 'illegal update: deps_proposals'); END;

CREATE TRIGGER deps_main_update_guard BEFORE UPDATE ON deps_main
WHEN NEW.repo IS NOT OLD.repo
BEGIN SELECT RAISE(ABORT, 'illegal update: deps_main'); END;

CREATE TRIGGER publishes_insert_guard BEFORE INSERT ON publishes
WHEN NEW.phase <> 'running'
BEGIN SELECT RAISE(ABORT, 'illegal insert: publishes'); END;
CREATE TRIGGER publishes_update_guard BEFORE UPDATE ON publishes
WHEN NEW.id IS NOT OLD.id OR NEW.started_ns IS NOT OLD.started_ns
  OR NOT ((OLD.phase = 'running' AND NEW.phase IN ('done', 'unknown')) OR (OLD.phase = 'unknown' AND NEW.phase = 'done'))
BEGIN SELECT RAISE(ABORT, 'illegal update: publishes'); END;

CREATE TRIGGER deps_sets_delete_gc_only        BEFORE DELETE ON deps_sets        WHEN NOT (SELECT active FROM strata_gc_gate) BEGIN SELECT RAISE(ABORT, 'delete outside gc: deps_sets'); END;
CREATE TRIGGER deps_attempts_delete_gc_only    BEFORE DELETE ON deps_attempts    WHEN NOT (SELECT active FROM strata_gc_gate) BEGIN SELECT RAISE(ABORT, 'delete outside gc: deps_attempts'); END;
CREATE TRIGGER deps_proposals_delete_gc_only   BEFORE DELETE ON deps_proposals   WHEN NOT (SELECT active FROM strata_gc_gate) BEGIN SELECT RAISE(ABORT, 'delete outside gc: deps_proposals'); END;
CREATE TRIGGER deps_main_delete_gc_only        BEFORE DELETE ON deps_main        WHEN NOT (SELECT active FROM strata_gc_gate) BEGIN SELECT RAISE(ABORT, 'delete outside gc: deps_main'); END;
CREATE TRIGGER base_trees_delete_gc_only       BEFORE DELETE ON base_trees       WHEN NOT (SELECT active FROM strata_gc_gate) BEGIN SELECT RAISE(ABORT, 'delete outside gc: base_trees'); END;
CREATE TRIGGER manifest_entries_delete_gc_only BEFORE DELETE ON manifest_entries WHEN NOT (SELECT active FROM strata_gc_gate) BEGIN SELECT RAISE(ABORT, 'delete outside gc: manifest_entries'); END;
CREATE TRIGGER base_gens_delete_gc_only        BEFORE DELETE ON base_gens        WHEN NOT (SELECT active FROM strata_gc_gate) BEGIN SELECT RAISE(ABORT, 'delete outside gc: base_gens'); END;
CREATE TRIGGER sessions_delete_gc_only         BEFORE DELETE ON sessions         WHEN NOT (SELECT active FROM strata_gc_gate) BEGIN SELECT RAISE(ABORT, 'delete outside gc: sessions'); END;
CREATE TRIGGER workspaces_delete_gc_only       BEFORE DELETE ON workspaces       WHEN NOT (SELECT active FROM strata_gc_gate) BEGIN SELECT RAISE(ABORT, 'delete outside gc: workspaces'); END;
CREATE TRIGGER seeds_delete_gc_only            BEFORE DELETE ON seeds            WHEN NOT (SELECT active FROM strata_gc_gate) BEGIN SELECT RAISE(ABORT, 'delete outside gc: seeds'); END;
CREATE TRIGGER move_intents_delete_gc_only     BEFORE DELETE ON move_intents     WHEN NOT (SELECT active FROM strata_gc_gate) BEGIN SELECT RAISE(ABORT, 'delete outside gc: move_intents'); END;
CREATE TRIGGER checkpoints_delete_gc_only      BEFORE DELETE ON checkpoints      WHEN NOT (SELECT active FROM strata_gc_gate) BEGIN SELECT RAISE(ABORT, 'delete outside gc: checkpoints'); END;
CREATE TRIGGER blobs_delete_gc_only            BEFORE DELETE ON blobs            WHEN NOT (SELECT active FROM strata_gc_gate) BEGIN SELECT RAISE(ABORT, 'delete outside gc: blobs'); END;
CREATE TRIGGER overlay_entries_delete_gc_only  BEFORE DELETE ON overlay_entries  WHEN NOT (SELECT active FROM strata_gc_gate) BEGIN SELECT RAISE(ABORT, 'delete outside gc: overlay_entries'); END;
CREATE TRIGGER gc_runs_delete_gc_only          BEFORE DELETE ON gc_runs          WHEN NOT (SELECT active FROM strata_gc_gate) BEGIN SELECT RAISE(ABORT, 'delete outside gc: gc_runs'); END;
CREATE TRIGGER publishes_no_delete     BEFORE DELETE ON publishes     BEGIN SELECT RAISE(ABORT, 'append-only: publishes'); END;
CREATE TRIGGER publish_items_no_delete BEFORE DELETE ON publish_items BEGIN SELECT RAISE(ABORT, 'append-only: publish_items'); END;
CREATE TRIGGER refs_audit_no_delete    BEFORE DELETE ON refs_audit    BEGIN SELECT RAISE(ABORT, 'append-only: refs_audit'); END;
"#;

pub(crate) const APPLICATION_ID: i64 = 1398035009;
pub(crate) const USER_VERSION: i64 = 1;
