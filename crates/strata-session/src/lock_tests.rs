use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// A lock adapter that only answers `is_shut_down`.
#[derive(Default)]
struct Lock {
    closed: AtomicBool,
}
impl RepoWriteLock for Lock {
    fn with_write(
        &self,
        _repo: &RepoName,
        f: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Result<()>> {
        Ok(f())
    }
    fn is_shut_down(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}
struct Before;
impl BeforeCheckpoint for Before {
    fn checkpoint(
        &self,
        _: &WorkspaceRef<'_>,
        _: &Base,
        _: CheckpointReason,
    ) -> Result<Checkpointed> {
        unreachable!("lock-only test")
    }
}

fn git_failure(stderr: &str) -> StrataError {
    StrataError::new(Code::Unknown, "git update-ref -d failed")
        .with_detail(serde_json::json!({ "exit": 1, "stderr": stderr }))
}

/// new-lock §2.7 (`ref_lock_retry_only_on_lockfile_contention`): only git's
/// lock-file form is contention; the generic `cannot lock ref` of a
/// compare-and-swap mismatch and a moved pin are not.
#[test]
fn ref_lock_retry_only_on_lockfile_contention() {
    let loose = "fatal: cannot lock ref 'refs/strata/pins/s-aaaaaaaa': Unable to create \
                 '/r/.git/refs/strata/pins/s-aaaaaaaa.lock': File exists.\n\nAnother git process seems to be running";
    let packed = "error: could not delete references: cannot lock ref 'refs/strata/pins/s-aaaaaaaa': \
                  Unable to create '/r/.git/packed-refs.lock': File exists.";
    assert!(ref_lock_busy(&git_failure(loose)));
    assert!(ref_lock_busy(&git_failure(packed)));
    let mismatch =
        "fatal: cannot lock ref 'refs/strata/pins/s-aaaaaaaa': is at 1111 but expected 2222";
    assert!(!ref_lock_busy(&git_failure(mismatch)));
    assert!(!ref_lock_busy(&git_failure(
        "fatal: cannot lock ref 'refs/strata/pins/s-aaaaaaaa'"
    )));
    let moved = StrataError::new(Code::HeadMoved, "moved")
        .with_detail(serde_json::json!({ "stderr": loose }));
    assert!(
        !ref_lock_busy(&moved),
        "only an Unknown git failure is contention"
    );
}

/// new-lock §4.1 (ii): every failed repo waited for its lock → certified;
/// any other failure wins and is returned uncertified.
#[test]
fn head_refusal_is_certified_only_when_every_failure_is_lock_wait() {
    let busy = |repo: &str| {
        StrataError::new(Code::Busy, "waited")
            .with_detail(serde_json::json!({ "reason": "lock_wait", "lock": { "repo": repo } }))
    };
    let fenced = StrataError::new(Code::Busy, "fence")
        .with_detail(serde_json::json!({ "reason": "pins_tightened" }));
    let one = crate::new::certify_head(vec![busy("api"), busy("tool")]);
    assert_eq!(one.detail.as_ref().unwrap()["retry_safe"], true);
    assert_eq!(one.detail.as_ref().unwrap()["lock"]["repo"], "api");
    let mixed = crate::new::certify_head(vec![busy("api"), fenced.clone()]);
    assert_eq!(mixed, fenced, "pins_tightened takes precedence");
    let no_such = StrataError::new(Code::NoSuchBranch, "no tip");
    assert_eq!(
        crate::new::certify_head(vec![no_such.clone(), busy("tool")]),
        no_such
    );
}

/// new-lock §2.4a: a removed take still refuses at shutdown, as the closed
/// lock did.
#[test]
fn removed_takes_refuse_at_shutdown_like_a_closed_lock() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    // This unit exercises only the lock adapter, without a Git fixture.
    let config = Config {
        paths: strata_types::Paths { root: root.clone() },
        repos: BTreeMap::new(),
        status: Default::default(),
        dev_ports: Default::default(),
        publish: Default::default(),
    };
    let store = Store::open(&root, &[]).unwrap();
    let gits = BTreeMap::new();
    let lock = Lock::default();
    let env = InstallEnv {
        path: String::new(),
        timeout: Duration::ZERO,
    };
    let sessions = Sessions::open(&config, &gits, &store, &lock, &env, &Before).unwrap();
    assert!(sessions.refuse_if_shut_down().is_ok());
    lock.closed.store(true, Ordering::SeqCst);
    let e = sessions.refuse_if_shut_down().unwrap_err();
    assert_eq!(e.code, Code::ReadOnly);
    assert_eq!(e.detail.unwrap()["reason"], "shutting_down");
    let id = SessionId::try_from("s-aaaaaaaa".to_string()).unwrap();
    let e = sessions
        .unpin(&RepoName::try_from("tool".to_string()).unwrap(), &id)
        .unwrap_err();
    assert_eq!(
        e.code,
        Code::ReadOnly,
        "the unpin refuses before it reads the pin"
    );
}
