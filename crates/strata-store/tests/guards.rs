//! store.md §7, Task 1: the schema's own guards, exercised with raw
//! connections (not through the store's write methods).

mod support;

use support::*;

const D: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const D2: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const I: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const T: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const H: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const B: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

struct Fx {
    _env: Env,
    c: Raw,
    gc: Gate,
}

fn fx() -> Fx {
    let env = env();
    env.open().close().unwrap();
    let (c, gc) = raw(&env.db(), false);
    c.execute_batch(&format!(
        "INSERT INTO deps_sets VALUES ('r', '{D}', '{I}', 'p', 't', '[]', 'empty', 0);
         INSERT INTO deps_sets VALUES ('r', '{D2}', '{I}', 'p', 't', '[]', 'empty', 0);
         INSERT INTO base_trees VALUES ('r', '{T}', 1, 0);
         INSERT INTO manifest_entries VALUES ('r', '{T}', 'a.txt', '{B}', 33188, 3);
         INSERT INTO base_gens VALUES (1, 'r', '{T}', '{D}', 0);
         INSERT INTO base_gens VALUES (2, 'r', '{T}', '{D2}', 0);
         INSERT INTO blobs VALUES ('{B}', 3, 0);
         INSERT INTO publishes VALUES (1, 'running', 0, NULL, NULL);
         INSERT INTO publish_items VALUES (1, 0, 0, 's-aaaaaaaa', 'r', 1, 'a.txt');
         INSERT INTO refs_audit VALUES (1, 0, 'r', 'push', 'refs/heads/main', NULL, '{H}', 'pushed', 1, NULL, NULL);
         INSERT INTO gc_runs VALUES (1, NULL, 0, 0, 'complete', '{{}}', NULL);
         INSERT INTO deps_main VALUES ('r', '{T}', '{H}', '{I}', '{D}', 0);
         INSERT INTO deps_attempts (repo, set_id, kind, state, staging, started_ns) VALUES ('r', '{D}', 'build', 'running', '/st/1', 0);"
    ))
    .unwrap();
    session(&c, "s-aaaaaaaa");
    c.execute_batch(&format!(
        "INSERT INTO seeds VALUES ('s-aaaaaaaa', 'r', 'x', '/src/x', NULL, 1, 0, 0);
         INSERT INTO checkpoints VALUES ('s-aaaaaaaa', 1, 'r', 1, 0, 'manual', 'm', 1);
         INSERT INTO overlay_entries VALUES ('s-aaaaaaaa', 1, 'a.txt', 'modified', '{B}', NULL, 33188);
         INSERT INTO move_intents VALUES ('s-aaaaaaaa', 'r', 2, '{T}', '{D2}', '{H}', '/stg', NULL, 'preparing', NULL, NULL, 0, NULL);
         INSERT INTO deps_proposals (repo, origin_session, from_set, from_inputs, to_set, to_inputs, state, created_ns, updated_ns)
           VALUES ('r', 's-aaaaaaaa', '{D}', '{I}', '{D2}', '{I}', 'awaiting_publish', 0, 0);"
    ))
    .unwrap();
    Fx { _env: env, c, gc }
}

/// A session (Creating) with one tracked workspace (generation 1, Ok).
fn session(c: &Connection, id: &str) {
    c.execute_batch(&format!(
        "INSERT INTO sessions VALUES ('{id}', NULL, '/w/{id}', 'creating', NULL, NULL, 0, NULL, NULL);
         INSERT INTO workspaces VALUES ('{id}', 'r', 1, '{H}', '/w/{id}/r', 1, 1, 'ok', NULL, '[]');"
    ))
    .unwrap();
}

fn exec(c: &Connection, sql: &str) -> Result<(), String> {
    c.execute_batch(sql).map_err(|e| e.to_string())
}

#[track_caller]
fn refused(c: &Connection, sql: &str, msg: &str) {
    match exec(c, sql) {
        Ok(()) => panic!("expected refusal ({msg}) for: {sql}"),
        Err(e) => assert!(e.contains(msg), "expected {msg:?}, got {e:?} for: {sql}"),
    }
}

#[track_caller]
fn ok(c: &Connection, sql: &str) {
    if let Err(e) = exec(c, sql) {
        panic!("expected success, got {e} for: {sql}");
    }
}

fn one(c: &Connection, sql: &str) -> String {
    c.query_row(sql, [], |r| {
        let v: Value = r.get(0)?;
        Ok(format!("{v:?}"))
    })
    .unwrap()
}

fn dump(c: &Connection, table: &str) -> Vec<String> {
    let mut st = c.prepare(&format!("SELECT * FROM {table}")).unwrap();
    let n = st.column_count().unwrap();
    st.query_map([], |r| {
        let mut s = String::new();
        for i in 0..n {
            let v: Value = r.get(i)?;
            s.push_str(&format!("{v:?}|"));
        }
        Ok(s)
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

const IMMUTABLE: [(&str, &str); 10] = [
    ("deps_sets", "platform = 'q'"),
    ("base_trees", "files = 2"),
    ("manifest_entries", "size = 4"),
    ("base_gens", "created_ns = 1"),
    ("seeds", "size = 2"),
    ("checkpoints", "entries = 2"),
    ("blobs", "size = 4"),
    ("overlay_entries", "mode = 33261"),
    ("publish_items", "path = 'b'"),
    ("refs_audit", "detail = 'x'"),
];

#[test]
fn committed_rows_are_immutable() {
    let f = fx();
    let c = &f.c;
    let mut tables: Vec<(&str, &str)> = IMMUTABLE.to_vec();
    tables.push(("gc_runs", "outcome = 'partial'"));
    for (t, set) in tables {
        let before = dump(c, t);
        refused(c, &format!("UPDATE {t} SET {set}"), &format!("immutable: {t}"));
        assert_eq!(dump(c, t), before, "{t} changed");
    }
    // the immutable key columns of the mutable tables
    refused(c, "UPDATE sessions SET root = '/elsewhere'", "illegal update: sessions");
    refused(c, "UPDATE workspaces SET path = '/elsewhere'", "illegal update: workspaces");
    refused(c, "UPDATE deps_attempts SET staging = '/elsewhere'", "illegal update: deps_attempts");
    refused(c, "UPDATE deps_main SET repo = 'q'", "illegal update: deps_main");
    refused(c, "UPDATE publishes SET started_ns = 5", "illegal update: publishes");
    // REPLACE's implicit delete should hit the delete guard. ENGINE GAP
    // (Turso 0.8.3 and 0.9.0-pre.1, docs/impl-progress/store.md "Engine: Turso"): there is no
    // recursive_triggers, so REPLACE deletes the old row without firing the
    // DELETE trigger. The store itself never issues REPLACE (only ON
    // CONFLICT DO NOTHING / DO UPDATE on mutable rows); this pins what a raw
    // client can do so a Turso fix shows up here.
    let before = dump(c, "blobs");
    let replace = [
        ("blobs", format!("INSERT OR REPLACE INTO blobs VALUES ('{B}', 9, 9)")),
        ("deps_sets", format!("INSERT OR REPLACE INTO deps_sets VALUES ('r', '{D}', '{I}', 'z', 'z', '[]', 'empty', 9)")),
    ];
    let mut gap = false;
    for (t, q) in &replace {
        match exec(c, q) {
            Err(m) => assert!(m.contains(&format!("delete outside gc: {t}")) || m.contains("FOREIGN KEY"), "{t}: {m}"),
            Ok(()) => {
                eprintln!("ENGINE GAP: INSERT OR REPLACE into {t} bypassed the delete guard");
                gap = true;
            }
        }
    }
    if !gap {
        assert_eq!(dump(c, "blobs"), before);
    }
    // a batch with one conflicting row leaves no half batch
    let n0 = one(c, "SELECT count(*) FROM blobs");
    let e = exec(
        c,
        &format!(
            "INSERT INTO blobs VALUES ('{a}', 1, 0), ('{b}', 1, 0), ('{B}', 1, 0)",
            a = "0".repeat(64),
            b = "1".repeat(64)
        ),
    )
    .unwrap_err();
    assert!(e.contains("UNIQUE") || e.contains("constraint"), "{e}");
    assert_eq!(one(c, "SELECT count(*) FROM blobs"), n0);
    // base and deps rows are as first written (unless REPLACE got through)
    if !gap {
        assert_eq!(dump(c, "deps_sets")[0], format!("Text(\"r\")|Text(\"{D}\")|Text(\"{I}\")|Text(\"p\")|Text(\"t\")|Text(\"[]\")|Text(\"empty\")|Integer(0)|"));
    }
    #[cfg(any())]
    assert_eq!(dump(c, "deps_sets")[0], format!("Text(\"r\")|Text(\"{D}\")|Text(\"{I}\")|Text(\"p\")|Text(\"t\")|Text(\"[]\")|Text(\"empty\")|Integer(0)|"));
    assert_eq!(one(c, "SELECT files FROM base_trees"), "Integer(1)");
}

const GC_TABLES: [&str; 15] = [
    "overlay_entries",
    "checkpoints",
    "seeds",
    "move_intents",
    "workspaces",
    "sessions",
    "manifest_entries",
    "base_gens",
    "base_trees",
    "deps_attempts",
    "deps_proposals",
    "deps_main",
    "deps_sets",
    "blobs",
    "gc_runs",
];

#[test]
fn delete_outside_gc_is_refused() {
    let f = fx();
    let c = &f.c;
    for t in GC_TABLES {
        let before = dump(c, t);
        assert!(!before.is_empty(), "fixture leaves {t} empty");
        refused(c, &format!("DELETE FROM {t}"), &format!("delete outside gc: {t}"));
        assert_eq!(dump(c, t), before);
    }
    // a connection without the function (like the sqlite3 CLI) cannot delete either
    let b = bare(&f._env.db());
    for t in GC_TABLES {
        let n = one(&b, &format!("SELECT count(*) FROM {t}"));
        assert!(exec(&b, &format!("DELETE FROM {t}")).is_err(), "{t}: bare delete succeeded");
        assert_eq!(one(&b, &format!("SELECT count(*) FROM {t}")), n);
    }
    drop(b);
    // gc mode: parents with children are held by the foreign keys
    f.gc.store(true, std::sync::atomic::Ordering::SeqCst);
    refused(c, "DELETE FROM sessions", "FOREIGN KEY");
    refused(c, "DELETE FROM base_trees", "FOREIGN KEY");
    refused(c, "DELETE FROM blobs", "FOREIGN KEY");
    assert_eq!(one(c, "SELECT count(*) FROM sessions"), "Integer(1)");
    // children first, then parents
    for t in GC_TABLES {
        ok(c, &format!("DELETE FROM {t}"));
        assert_eq!(one(c, &format!("SELECT count(*) FROM {t}")), "Integer(0)", "{t}");
    }
    for t in ["publish_items", "refs_audit", "publishes"] {
        let before = dump(c, t);
        refused(c, &format!("DELETE FROM {t}"), &format!("append-only: {t}"));
        assert_eq!(dump(c, t), before);
    }
}

// ---------------------------------------------------------------- graphs

fn lifecycle_set(to: &str, from: &str) -> String {
    let removed = if to == "removed" { "1" } else { "NULL" };
    let scene = if to == "unknown" && from != "unknown" { "'{\"new\":1}'" } else { "scene" };
    format!("lifecycle = '{to}', removed_ns = {removed}, scene = {scene}")
}

/// Drive a fresh session to `state` along allowed edges.
fn session_in(c: &Connection, id: &str, state: &str) {
    session(c, id);
    let path: &[&str] = match state {
        "creating" => &[],
        "active" => &["active"],
        "removing" => &["active", "removing"],
        "removed" => &["active", "removing", "removed"],
        "unknown" => &["unknown"],
        _ => unreachable!(),
    };
    let mut from = "creating";
    for to in path {
        ok(c, &format!("UPDATE sessions SET {} WHERE id = '{id}'", lifecycle_set(to, from)));
        from = to;
    }
}

const LIFECYCLES: [&str; 5] = ["creating", "active", "removing", "removed", "unknown"];

#[test]
fn state_graphs_are_enforced_by_triggers() {
    let f = fx();
    let c = &f.c;

    // ---- session lifecycle: every pair
    let allowed = [
        ("creating", "creating"),
        ("removing", "removing"),
        ("creating", "active"),
        ("creating", "unknown"),
        ("active", "removing"),
        ("removing", "removed"),
        ("removing", "unknown"),
        ("unknown", "removing"),
    ];
    let mut k = 0;
    for from in LIFECYCLES {
        for to in LIFECYCLES {
            k += 1;
            let id = format!("s-{}", sid(k));
            session_in(c, &id, from);
            let before = dump_where(c, "sessions", &id);
            let sql = format!("UPDATE sessions SET {} WHERE id = '{id}'", lifecycle_set(to, from));
            if allowed.contains(&(from, to)) {
                ok(c, &sql);
            } else {
                refused(c, &sql, "illegal update: sessions");
                assert_eq!(dump_where(c, "sessions", &id), before);
            }
        }
    }
    // phase only in creating / removing; scene only on entering unknown
    session_in(c, "s-phaseaaa", "creating");
    ok(c, "UPDATE sessions SET phase = 'p1' WHERE id = 's-phaseaaa'");
    ok(c, "UPDATE sessions SET lifecycle = 'active' WHERE id = 's-phaseaaa'");
    refused(c, "UPDATE sessions SET phase = 'p2' WHERE id = 's-phaseaaa'", "illegal update: sessions");
    refused(c, "UPDATE sessions SET scene = '{}' WHERE id = 's-phaseaaa'", "illegal update: sessions");
    refused(c, "INSERT INTO sessions VALUES ('s-bbbbbbbb', NULL, '/q', 'active', NULL, NULL, 0, NULL, NULL)", "illegal insert: sessions");

    // ---- intents: insert guard
    session_in(c, "s-intentaa", "active");
    let w = "session = 's-intentaa' AND repo = 'r'";
    let ins = |n: i64| {
        format!("INSERT INTO move_intents VALUES ('s-intentaa', 'r', {n}, '{T}', '{D2}', '{H}', '/stg', NULL, 'preparing', NULL, NULL, 0, NULL)")
    };
    ok(c, &ins(2));
    refused(c, &ins(3), "UNIQUE"); // one open intent per workspace
    ok(c, &format!("UPDATE move_intents SET state = 'refused', ended_ns = 1 WHERE {w} AND n = 2"));
    refused(c, &ins(2), "illegal insert: move_intents");
    ok(c, &ins(3));
    ok(c, &format!("UPDATE move_intents SET state = 'abandoned', ended_ns = 1 WHERE {w} AND n = 3"));
    refused(
        c,
        &format!("INSERT INTO move_intents VALUES ('s-intentaa', 'r', 9, '{T}', '{D2}', '{H}', '/stg', NULL, 'swapping', '[]', NULL, 0, NULL)"),
        "illegal insert: move_intents",
    );
    // n must exceed the workspace generation too
    ok(c, &ins(4));
    ok(c, &format!("UPDATE move_intents SET state = 'swapping', conflicts = '[]' WHERE {w} AND n = 4"));
    ok(c, &format!("UPDATE workspaces SET base_gen = 2, generation = 4, conflicts = '[]' WHERE {w}"));
    ok(c, &format!("UPDATE move_intents SET state = 'done', ended_ns = 1 WHERE {w} AND n = 4"));
    assert_eq!(one(c, &format!("SELECT generation FROM workspaces WHERE {w}")), "Integer(4)");

    // ---- intents: every pair of states (unknown → done | abandoned is conditional, tested below)
    let states = ["preparing", "swapping", "done", "refused", "abandoned", "unknown"];
    let allowed = [
        ("preparing", "preparing"),
        ("swapping", "swapping"),
        ("done", "done"),
        ("preparing", "swapping"),
        ("preparing", "refused"),
        ("preparing", "abandoned"),
        ("swapping", "done"),
        ("swapping", "unknown"),
    ];
    for from in states {
        for to in states {
            k += 1;
            let id = format!("s-{}", sid(k));
            session_in(c, &id, "creating");
            intent_in(c, &id, from);
            let w = format!("session = '{id}' AND repo = 'r' AND n = 2");
            let ended = if matches!(to, "preparing" | "swapping") { "NULL".to_string() } else if from == to { "ended_ns".into() } else { "7".into() };
            let conflicts = if from == "preparing" && to == "swapping" { "'[]'" } else { "conflicts" };
            let sql = format!("UPDATE move_intents SET state = '{to}', ended_ns = {ended}, conflicts = {conflicts} WHERE {w}");
            let before = dump_where(c, "move_intents", &id);
            if allowed.contains(&(from, to)) {
                ok(c, &sql);
            } else {
                refused(c, &sql, "illegal update: move_intents");
                assert_eq!(dump_where(c, "move_intents", &id), before, "{from} -> {to}");
            }
        }
    }
    // conflicts only on preparing → swapping, and swapping needs them (CHECK)
    session_in(c, "s-conflict", "active");
    intent_in(c, "s-conflict", "preparing");
    let w = "session = 's-conflict' AND repo = 'r' AND n = 2";
    refused(c, &format!("UPDATE move_intents SET state = 'swapping' WHERE {w}"), "CHECK constraint failed");
    refused(c, &format!("UPDATE move_intents SET conflicts = '[]' WHERE {w}"), "illegal update: move_intents");
    ok(c, &format!("UPDATE move_intents SET state = 'swapping', conflicts = '[\"a\"]' WHERE {w}"));
    refused(c, &format!("UPDATE move_intents SET conflicts = '[]' WHERE {w}"), "illegal update: move_intents");
    ok(c, &format!("UPDATE move_intents SET phase = 'p' WHERE {w}"));
    ok(c, &format!("UPDATE move_intents SET state = 'unknown', ended_ns = 1 WHERE {w}"));
    refused(c, &format!("UPDATE move_intents SET phase = 'q' WHERE {w}"), "illegal update: move_intents");

    // ---- workspaces: base/head/generation only to the intent with the largest n
    let ws = "session = 's-conflict' AND repo = 'r'";
    refused(c, &format!("UPDATE workspaces SET head = '{T}' WHERE {ws}"), "illegal update: workspaces");
    refused(c, &format!("UPDATE workspaces SET base_gen = 2, generation = 3 WHERE {ws}"), "illegal update: workspaces");
    // health ok → unknown writes the scene; scene never changes otherwise
    refused(c, &format!("UPDATE workspaces SET scene = '{{}}' WHERE {ws}"), "illegal update: workspaces");
    ok(c, &format!("UPDATE workspaces SET health = 'unknown', scene = '{{\"s\":1}}' WHERE {ws}"));
    refused(c, &format!("UPDATE workspaces SET scene = '{{\"s\":2}}' WHERE {ws}"), "illegal update: workspaces");
    // unknown intent → done needs the workspace back to Ok first
    refused(c, &format!("UPDATE move_intents SET state = 'done' WHERE {w}"), "illegal update: move_intents");
    // unknown → ok only while the session is active and the latest intent is unknown
    ok(c, "UPDATE sessions SET lifecycle = 'removing' WHERE id = 's-conflict'");
    refused(c, &format!("UPDATE workspaces SET health = 'ok' WHERE {ws}"), "illegal update: workspaces");
    // (a fresh session for the positive path)
    session_in(c, "s-resolvea", "active");
    intent_in(c, "s-resolvea", "unknown");
    let ws = "session = 's-resolvea' AND repo = 'r'";
    let w = "session = 's-resolvea' AND repo = 'r' AND n = 2";
    // accept: workspace to Ok at generation n, then the intent to done
    refused(c, &format!("UPDATE workspaces SET base_gen = 2, generation = 3, health = 'ok' WHERE {ws}"), "illegal update: workspaces");
    ok(c, &format!("UPDATE workspaces SET base_gen = 2, generation = 2, health = 'ok' WHERE {ws}"));
    let scene_kept = one(c, &format!("SELECT scene FROM workspaces WHERE {ws}"));
    assert!(scene_kept.contains("Text"), "scene stays as history");
    refused(c, &format!("UPDATE move_intents SET state = 'abandoned' WHERE {w}"), "illegal update: move_intents");
    ok(c, &format!("UPDATE move_intents SET state = 'done', old_tree = '/old' WHERE {w}"));
    refused(c, &format!("UPDATE move_intents SET old_tree = '/other' WHERE {w}"), "illegal update: move_intents");
    // reject: workspace to Ok without moving, then the intent to abandoned
    session_in(c, "s-rejectaa", "active");
    intent_in(c, "s-rejectaa", "unknown");
    let ws = "session = 's-rejectaa' AND repo = 'r'";
    let w = "session = 's-rejectaa' AND repo = 'r' AND n = 2";
    ok(c, &format!("UPDATE workspaces SET health = 'ok' WHERE {ws}"));
    refused(c, &format!("UPDATE move_intents SET state = 'done' WHERE {w}"), "illegal update: move_intents");
    ok(c, &format!("UPDATE move_intents SET state = 'abandoned' WHERE {w}"));
    // untracked workspaces never move
    c.execute_batch(&format!(
        "INSERT INTO sessions VALUES ('s-untracka', NULL, '/w/u', 'creating', NULL, NULL, 0, NULL, NULL);
         INSERT INTO workspaces VALUES ('s-untracka', 'r', 1, '{H}', '/w/u/r', 0, 1, 'ok', NULL, '[]');"
    ))
    .unwrap();
    intent_raw(c, "s-untracka");
    refused(c, "UPDATE workspaces SET base_gen = 2, generation = 2 WHERE session = 's-untracka'", "illegal update: workspaces");
    refused(
        c,
        &format!("INSERT INTO workspaces VALUES ('s-untracka', 'q', 1, '{H}', '/w/u/q', 1, 2, 'ok', NULL, '[]')"),
        "illegal insert: workspaces",
    );

    // ---- attempts
    let att = |c: &Connection| -> i64 {
        c.execute_batch(&format!(
            "INSERT INTO deps_attempts (repo, set_id, kind, state, staging, started_ns) VALUES ('q', '{D}', 'adopt', 'running', '/st', 0)"
        ))
        .unwrap();
        c.last_insert_rowid()
    };
    let a = att(c);
    ok(c, &format!("UPDATE deps_attempts SET pid = 42 WHERE id = {a}"));
    refused(c, &format!("UPDATE deps_attempts SET pid = 43 WHERE id = {a}"), "illegal update: deps_attempts");
    ok(c, &format!("UPDATE deps_attempts SET state = 'sealed', ended_ns = 1 WHERE id = {a}"));
    for to in ["running", "failed", "refused", "sealed"] {
        refused(c, &format!("UPDATE deps_attempts SET state = '{to}', ended_ns = 2, error = '{{}}' WHERE id = {a}"), "illegal update: deps_attempts");
    }
    for to in ["failed", "refused"] {
        let a = att(c);
        refused(c, &format!("UPDATE deps_attempts SET state = '{to}', ended_ns = 1 WHERE id = {a}"), "CHECK constraint failed");
        ok(c, &format!("UPDATE deps_attempts SET state = '{to}', ended_ns = 1, error = '{{\"code\":\"x\"}}' WHERE id = {a}"));
        refused(c, &format!("UPDATE deps_attempts SET state = 'sealed', error = NULL WHERE id = {a}"), "illegal update: deps_attempts");
    }
    refused(
        c,
        &format!("INSERT INTO deps_attempts (repo, set_id, kind, state, staging, started_ns, ended_ns) VALUES ('q', '{D2}', 'adopt', 'sealed', '/st', 0, 1)"),
        "illegal insert: deps_attempts",
    );

    // ---- proposals
    let prop = |c: &Connection, origin: &str| -> i64 {
        c.execute_batch(&format!(
            "INSERT INTO deps_proposals (repo, origin_session, from_set, from_inputs, to_set, to_inputs, state, created_ns, updated_ns)
             VALUES ('r', '{origin}', '{D}', '{I}', '{D2}', '{I}', 'awaiting_publish', 0, 0)"
        ))
        .unwrap();
        c.last_insert_rowid()
    };
    let p1 = prop(c, "s-p1aaaaaa");
    let p2 = prop(c, "s-p2aaaaaa");
    refused(c, &format!("UPDATE deps_proposals SET state = 'applied', tree = '{T}', head = '{H}' WHERE id = {p1}"), "illegal update: deps_proposals");
    ok(c, &format!("UPDATE deps_proposals SET state = 'pending', tree = '{T}', head = '{H}' WHERE id = {p1}"));
    refused(c, &format!("UPDATE deps_proposals SET tree = '{H}', head = '{T}' WHERE id = {p1}"), "illegal update: deps_proposals");
    refused(c, &format!("UPDATE deps_proposals SET state = 'awaiting_publish', tree = NULL, head = NULL WHERE id = {p1}"), "illegal update: deps_proposals");
    ok(c, &format!("UPDATE deps_proposals SET state = 'applied' WHERE id = {p1}"));
    for to in ["pending", "superseded", "awaiting_publish", "applied"] {
        refused(c, &format!("UPDATE deps_proposals SET state = '{to}' WHERE id = {p1}"), "illegal update: deps_proposals");
    }
    ok(c, &format!("UPDATE deps_proposals SET state = 'superseded', superseded_by = {p1} WHERE id = {p2}"));
    for to in ["pending", "applied", "awaiting_publish"] {
        refused(c, &format!("UPDATE deps_proposals SET state = '{to}', superseded_by = NULL WHERE id = {p2}"), "illegal update: deps_proposals");
    }
    refused(
        c,
        &format!(
            "INSERT INTO deps_proposals (repo, origin_session, from_set, from_inputs, to_set, to_inputs, tree, head, state, created_ns, updated_ns)
             VALUES ('r', NULL, '{D}', '{I}', '{D2}', '{I}', '{T}', '{H}', 'applied', 0, 0)"
        ),
        "illegal insert: deps_proposals",
    );

    // ---- publishes
    let pubrow = |c: &Connection, id: i64| ok(c, &format!("INSERT INTO publishes VALUES ({id}, 'running', 0, NULL, NULL)"));
    pubrow(c, 10);
    refused(c, "UPDATE publishes SET outcome = '{}' WHERE id = 10", "illegal update: publishes");
    ok(c, "UPDATE publishes SET phase = 'unknown', ended_ns = 1 WHERE id = 10");
    refused(c, "UPDATE publishes SET phase = 'running', ended_ns = NULL WHERE id = 10", "illegal update: publishes");
    ok(c, "UPDATE publishes SET phase = 'done', ended_ns = 2 WHERE id = 10");
    for to in ["running", "unknown", "done"] {
        refused(c, &format!("UPDATE publishes SET phase = '{to}' WHERE id = 10"), "illegal update: publishes");
    }
    // publish.md §5.7: `assign` rewrites the outcome of a closed row, nothing else
    ok(c, "UPDATE publishes SET outcome = '{\"assigned\":[]}' WHERE id = 10");
    refused(c, "UPDATE publishes SET outcome = '{}', ended_ns = 3 WHERE id = 10", "illegal update: publishes");
    pubrow(c, 11);
    ok(c, "UPDATE publishes SET phase = 'done', ended_ns = 1, outcome = '{}' WHERE id = 11");
    refused(c, "INSERT INTO publishes VALUES (12, 'done', 0, 1, NULL)", "illegal insert: publishes");
}

fn sid(k: usize) -> String {
    // eight base32 letters from a counter
    let alpha = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut n = k;
    let mut s = Vec::new();
    for _ in 0..8 {
        s.push(alpha[n % 32]);
        n /= 32;
    }
    String::from_utf8(s).unwrap()
}

fn dump_where(c: &Connection, table: &str, id: &str) -> Vec<String> {
    dump(c, table).into_iter().filter(|r| r.contains(id)).collect()
}

fn intent_raw(c: &Connection, id: &str) {
    ok(
        c,
        &format!("INSERT INTO move_intents VALUES ('{id}', 'r', 2, '{T}', '{D2}', '{H}', '/stg', NULL, 'preparing', NULL, NULL, 0, NULL)"),
    );
}

/// Intent n = 2 driven to `state` along allowed edges (workspace untouched
/// except for `unknown`, which also marks the workspace Unknown).
fn intent_in(c: &Connection, id: &str, state: &str) {
    intent_raw(c, id);
    let w = format!("session = '{id}' AND repo = 'r' AND n = 2");
    let up = |set: &str| ok(c, &format!("UPDATE move_intents SET {set} WHERE {w}"));
    match state {
        "preparing" => {}
        "swapping" => up("state = 'swapping', conflicts = '[]'"),
        "done" => {
            up("state = 'swapping', conflicts = '[]'");
            up("state = 'done', ended_ns = 1");
        }
        "refused" => up("state = 'refused', ended_ns = 1"),
        "abandoned" => up("state = 'abandoned', ended_ns = 1"),
        "unknown" => {
            up("state = 'swapping', conflicts = '[]'");
            up("state = 'unknown', ended_ns = 1");
            ok(c, &format!("UPDATE workspaces SET health = 'unknown', scene = '{{\"m\":1}}' WHERE session = '{id}' AND repo = 'r'"));
        }
        _ => unreachable!(),
    }
}

/// v2: `sessions.agent` accepts exactly what `AgentName` accepts, and
/// never changes once written.
#[test]
fn agent_column_matches_agent_name_and_is_immutable() {
    let f = fx();
    let c = &f.c;
    let long = "a".repeat(64);
    let longer = "a".repeat(65);
    let cases: Vec<&str> = vec![
        "claude-code", "codex", "cursor-agent", "A", "9", "a.b_c-D", &long, &longer, "", "-x", ".x", "_x", "a b", "a/b",
        "a\u{0}b", "\u{0}", "é", "aé", "a\n", "a*", "a[b]", "a?", "ａ",
    ];
    for (k, name) in cases.iter().enumerate() {
        let id = format!("s-{}", sid(1000 + k));
        let rust = strata_types::AgentName::try_from(name.to_string()).is_ok();
        let sql = c
            .execute(
                "INSERT INTO sessions (id, ticket, root, lifecycle, created_ns, agent) VALUES (?1, NULL, ?2, 'creating', 0, ?3)",
                params![id, format!("/w/{id}"), name.to_string()],
            )
            .is_ok();
        assert_eq!(sql, rust, "AgentName and the CHECK disagree on {name:?}");
    }
    let (named, none) = (format!("s-{}", sid(1000)), "s-zzzzzzzz");
    ok(c, &format!("INSERT INTO sessions (id, ticket, root, lifecycle, created_ns, agent) VALUES ('{none}', NULL, '/w/n', 'creating', 0, NULL)"));
    refused(c, &format!("UPDATE sessions SET agent = 'codex' WHERE id = '{named}'"), "immutable: sessions.agent");
    refused(c, &format!("UPDATE sessions SET agent = NULL WHERE id = '{named}'"), "immutable: sessions.agent");
    refused(c, &format!("UPDATE sessions SET agent = 'codex' WHERE id = '{none}'"), "immutable: sessions.agent");
    ok(c, &format!("UPDATE sessions SET agent = 'claude-code' WHERE id = '{named}'"));
    ok(c, &format!("UPDATE sessions SET lifecycle = 'active' WHERE id = '{named}'"));
    assert_eq!(one(c, &format!("SELECT agent FROM sessions WHERE id = '{named}'")), r#"Text("claude-code")"#);
}
