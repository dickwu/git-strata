//! Per-repo dispatch (§5.1 phase B and phase D): up to 4 workers, repos
//! claimed in `RepoName` order; once one fails no further repo is
//! dispatched, and the first failure in `RepoName` order is the result.

use std::sync::Mutex;

use strata_types::RepoName;

use crate::Result;

pub(crate) const WORKERS: usize = 4;

/// Run `f` for each repo; `seam` is checked as each repo is claimed (in
/// order), so a seam without `@<repo>` fires on the first one dispatched.
pub(crate) fn each_repo<T: Send>(
    repos: &[RepoName],
    seam: &(dyn Fn(&RepoName) -> Result<()> + Sync),
    f: &(dyn Fn(&RepoName) -> Result<T> + Sync),
) -> Result<Vec<(RepoName, T)>> {
    struct Shared<T> {
        next: usize,
        failed: bool,
        results: Vec<Option<Result<T>>>,
    }
    let shared = Mutex::new(Shared { next: 0, failed: false, results: (0..repos.len()).map(|_| None).collect() });
    let workers = WORKERS.min(repos.len()).max(1);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = {
                        let mut sh = shared.lock().unwrap_or_else(|p| p.into_inner());
                        if sh.failed || sh.next >= repos.len() {
                            return;
                        }
                        let i = sh.next;
                        sh.next += 1;
                        if let Err(e) = seam(&repos[i]) {
                            sh.failed = true;
                            sh.results[i] = Some(Err(e));
                            return;
                        }
                        i
                    };
                    let r = f(&repos[i]);
                    let mut sh = shared.lock().unwrap_or_else(|p| p.into_inner());
                    if r.is_err() {
                        sh.failed = true;
                    }
                    sh.results[i] = Some(r);
                }
            });
        }
    });
    let sh = shared.into_inner().unwrap_or_else(|p| p.into_inner());
    let mut out = Vec::new();
    for (i, r) in sh.results.into_iter().enumerate() {
        match r {
            Some(Ok(v)) => out.push((repos[i].clone(), v)),
            Some(Err(e)) => return Err(e),
            None => {}
        }
    }
    Ok(out)
}
