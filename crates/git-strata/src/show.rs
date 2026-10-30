//! What a human sees on stderr before a confirmation (cli.md §5): the
//! commits a push publishes, the proposal an approval moves sessions to.

use serde_json::Value;
use strata_deps::{Proposal, publish_chain};
use strata_gitio::Git;
use strata_types::{CommitOid, RepoConfig, RepoName};

/// How many commits are listed before "… and more".
pub const SHOW_COMMITS: usize = 500;

/// The commits `origin..top` of `repo`'s publish chain, one per line.
/// Returns the chain top shown (`None`: no local chain, or unreadable);
/// the caller sends it so the daemon binds the confirmation to exactly
/// this plan (review H2).
pub fn push_plan(repo: &RepoName, rc: &RepoConfig, why: &str) -> Option<CommitOid> {
    let branch = &rc.publish.branch;
    eprintln!("{why}: push {repo} to origin/{branch}");
    let git = match Git::open(&rc.path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("  (cannot open {}: {})", rc.path.display(), e.message);
            return None;
        }
    };
    // Review P1-15: the URL the approval binds (redacted; the prompt shows it too).
    match git.push_url("origin") {
        Ok(u) => eprintln!("  to {}", strata_types::display_url(&u)),
        Err(e) => eprintln!("  (cannot read origin's push URL: {})", e.message),
    }
    match publish_chain(&git, branch) {
        Ok(Some(ch)) => match &ch.top {
            Some(top) => {
                eprintln!("  origin {} .. chain top {} ({:?})", ch.origin, top, ch.state);
                match git.commits_between(&ch.origin, top, SHOW_COMMITS) {
                    Ok((v, more)) => {
                        if v.is_empty() {
                            eprintln!("  no commits beyond origin");
                        }
                        for (c, s) in v {
                            eprintln!("  {c} {s}");
                        }
                        if more {
                            eprintln!("  … and more (first {SHOW_COMMITS} shown)");
                        }
                    }
                    Err(e) => eprintln!("  (cannot list the commits: {})", e.message),
                }
                Some(top.clone())
            }
            None => {
                eprintln!("  no local publish chain yet (origin {})", ch.origin);
                None
            }
        },
        Ok(None) => {
            eprintln!("  no origin/{branch} tracking ref");
            None
        }
        Err(e) => {
            eprintln!("  (cannot read the publish chain: {})", e.message);
            None
        }
    }
}

/// A proposal's fields (§5 step 1).
pub fn proposal(label: &str, p: &Proposal) {
    let state = serde_json::to_string(&p.state).unwrap_or_default();
    eprintln!("{label} {} for {}: state {state}", p.id.get(), p.repo);
    eprintln!("  from {} ({})", p.from, p.from_inputs);
    eprintln!("  to   {} ({})", p.to, p.to_inputs);
    if let Some(t) = &p.tree {
        eprintln!("  tree {t}");
    }
    if let Some(h) = &p.head {
        eprintln!("  head {h}");
    }
}

/// A proposal given as JSON (from an error detail).
pub fn proposal_value(label: &str, v: &Value) {
    match serde_json::from_value::<Proposal>(v.clone()) {
        Ok(p) => proposal(label, &p),
        Err(_) => eprintln!("{label} {v}"),
    }
}
