//! Mutates the test process environment, so it is its own test binary.
mod common;

use common::*;

#[test]
fn command_strips_inherited_git_env() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let other = sb.init_repo("other");
    let expected = std::fs::canonicalize(repo.join(".git")).unwrap();
    // SAFETY: this binary has exactly one test; nothing else reads the env concurrently.
    unsafe {
        std::env::set_var("GIT_DIR", other.join(".git"));
        std::env::set_var("GIT_WORK_TREE", &other);
        std::env::set_var("GIT_INDEX_FILE", other.join(".git/index"));
        std::env::set_var("GIT_OBJECT_DIRECTORY", other.join(".git/objects"));
        std::env::set_var("GIT_CONFIG_PARAMETERS", "'core.bare'='true'");
        std::env::set_var("STRATA_PUBLISH", "1");
    }
    let git = sb.open(&repo);
    assert_eq!(git.common_dir(), expected);
    assert_eq!(git.repo_id(), strata_types::RepoId::new(expected));
    let calls = git.take_git_calls();
    assert!(!calls.is_empty());
    for c in &calls {
        for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_CONFIG_PARAMETERS", "STRATA_PUBLISH"] {
            assert!(env_of(c, k).is_none(), "{k} leaked into {:?}", args_of(c));
        }
    }
}
