use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

#[derive(Default)]
struct Lock {
    attempts: AtomicUsize,
    waits: Mutex<Vec<SystemTime>>,
}
impl RepoWriteLock for Lock {
    fn with_write(
        &self,
        _repo: &RepoName,
        f: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Result<()>> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) < 2 {
            return Err(StrataError::new(Code::Busy, "attempt expired")
                .with_detail(serde_json::json!({"reason":"lock_wait"})));
        }
        Ok(f())
    }
    fn record_long_wait(&self, _repo: &RepoName, attempt_since: SystemTime) {
        self.waits.lock().unwrap().push(attempt_since);
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

#[test]
fn unbounded_lock_records_each_expired_attempt_and_preserves_inner_errors() {
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
    let repo = RepoName::try_from("tool".to_string()).unwrap();
    let start = SystemTime::now();
    let mut calls = 0;
    assert_eq!(
        sessions
            .with_lock_unbounded(&repo, || {
                calls += 1;
                Ok(7)
            })
            .unwrap(),
        7
    );
    assert_eq!(calls, 1);
    let waits = lock.waits.lock().unwrap().clone();
    assert_eq!(waits.len(), 2);
    assert!(waits[0] >= start && waits[1] > waits[0]);
    let error = StrataError::new(Code::Busy, "closure refusal");
    let result: Result<()> = sessions.with_lock_unbounded(&repo, || Err(error.clone()));
    assert_eq!(result.unwrap_err(), error);
    assert_eq!(lock.attempts.load(Ordering::SeqCst), 4);
    assert_eq!(lock.waits.lock().unwrap().len(), 2);
}
