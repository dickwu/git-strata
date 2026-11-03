mod support;

use strata_publish::{RebaseReport, rebase_session};
use strata_types::{BaseGen, CommitOid, SyncDeps};
use support::*;

#[test]
fn proposal_from_head_recorded() {
    let mut f = Fx::with(&["tool"]);
    f.cfg_mut("tool").sync.follow_main = Some(false);
    f.cfg_mut("tool").sync.deps = SyncDeps::Confirm;
    let s = f.new_session(&["tool"]);
    let repo = f.rn("tool");
    let ws = f.store().session(&s).unwrap().unwrap().manifest.repos[&repo].clone();
    let head =
        CommitOid::try_from(f.sb.commit(&f.main("tool"), &[("bun.lock", b"lock v2\n")], "deps"))
            .unwrap();
    let target = BaseGen {
        tree: f.git_of("tool").tree_of(&head).unwrap(),
        ..ws.base.clone()
    };
    let deps = f.deps_ctx("tool");
    let sessions = f.sessions();
    // No observe_main/publish has populated the proposal slot first.
    assert!(
        strata_deps::open_proposals(f.store(), Some(&repo))
            .unwrap()
            .is_empty()
    );
    let out = rebase_session(&f.ctx(&deps, &sessions), s, &repo, target, head.clone()).unwrap();
    let RebaseReport::Waiting { proposal } = out else {
        panic!("{out:?}")
    };
    assert_eq!(proposal.from_head.as_ref(), Some(&ws.head));
    assert_eq!(proposal.head.as_ref(), Some(&head));
    let stored = strata_deps::get_proposal(f.store(), proposal.id).unwrap();
    assert_eq!(stored.from_head, Some(ws.head));
}
