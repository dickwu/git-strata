//! status-perf: wall-clock timing of `status --all`,
//! `ls` and `status --brief` against the in-process daemon on a
//! throw-away store. Not a pass/fail test: `cargo test --test status_timing
//! -- --ignored --nocapture` prints the numbers.

mod support;

use std::fs;
use std::time::Instant;

use support::live::{Live, write};

fn time(lv: &Live, args: &[&str]) -> u128 {
    let mut best = u128::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        let r = lv.run(args);
        assert_eq!(r.code, 0, "{args:?}: {r:?}");
        best = best.min(t.elapsed().as_millis());
    }
    best
}

fn run(parallel: Option<u32>) {
    let repos = ["r1", "r2", "r3"];
    let lv = Live::new(&repos, &[("a.txt", b"alpha\n")]);
    if let Some(p) = parallel {
        let cfg = lv.home.config_dir().join("config.toml");
        let mut t = fs::read_to_string(&cfg).unwrap();
        t.push_str(&format!("\n[status]\nparallel = {p}\n"));
        fs::write(&cfg, t).unwrap();
    }
    lv.serve(|_| {
        for i in 0..8 {
            let s = lv.new_session(&format!("T-{i}"), "r1,r2,r3");
            write(&lv.ws(&s, "r1"), "b.txt", b"beta\n");
        }
        let all = time(&lv, &["status", "--all"]);
        let ls = time(&lv, &["ls"]);
        let brief = time(&lv, &["status", "--all", "--brief"]);
        eprintln!("parallel={parallel:?} (24 workspaces): status --all {all} ms; ls {ls} ms; status --all --brief {brief} ms");
    });
}

#[test]
#[ignore]
fn status_all_timing() {
    run(Some(1));
    run(None);
}
