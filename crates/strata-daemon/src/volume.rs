//! `daemon_status.volume` (status-bar design §3.2, §11.6): free and total
//! bytes of the volume holding the data root ([`strata_apfs::volume_space`];
//! docs/modules/apfs.md says what they mean on APFS). A thread of its own
//! samples once at start and then every [`EVERY`] into a cache, and
//! `daemon_status` reads only the cache: a wedged volume stalls the sampler,
//! never the reply, whose `as_of` then ages. A failed sample clears the
//! cache (`null`) until one succeeds.
//!
//! The data root holds the store, the session trees and the dependency
//! trees, and every configured checkout is on its volume (store open
//! refuses one that is not, `CrossVolume`), so this one volume is every
//! volume git-strata writes its bulk to: the sample names it (`path`,
//! `mount`, `name`; FEATURES `daemon_status.volume_name`) so a client can
//! say which disk is low instead of a generic "data volume".

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use strata_apfs::{VolumeLabel, VolumeSpace};

/// How often the sampler reads the volume.
pub const EVERY: Duration = Duration::from_secs(10);

/// How the sampler reads a volume ([`strata_apfs::volume_space`] outside
/// tests).
pub type SampleFn = dyn Fn(&Path) -> io::Result<VolumeSpace> + Send + Sync;

/// The cache the sampler fills.
pub struct Volume {
    path: PathBuf,
    last: Mutex<Option<Sample>>,
    stopped: Mutex<bool>,
    wake: Condvar,
}

/// One successful read. `label` is `None` when the volume's name could not
/// be read although its space could; the reply then leaves `mount` and
/// `name` out.
struct Sample {
    space: VolumeSpace,
    label: Option<VolumeLabel>,
    at: SystemTime,
}

/// Owns the sampler; stops it when dropped (the daemon going away). The
/// thread itself holds an `Arc` and is never joined: one stuck in `statfs`
/// ends when that call returns.
pub struct VolumeHandle(pub Arc<Volume>);

impl Drop for VolumeHandle {
    fn drop(&mut self) {
        *lock(&self.0.stopped) = true;
        self.0.wake.notify_all();
    }
}

impl std::ops::Deref for VolumeHandle {
    type Target = Volume;
    fn deref(&self) -> &Volume {
        &self.0
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Volume {
    /// Sample the volume holding `root` every [`EVERY`].
    pub fn start(root: &Path) -> VolumeHandle {
        Volume::start_guarded(root, Default::default())
    }

    pub(crate) fn start_guarded(root: &Path, drain: crate::drain::Drain) -> VolumeHandle {
        Volume::start_with_guard(
            root.to_path_buf(),
            EVERY,
            Arc::new(strata_apfs::volume_space),
            drain,
        )
    }

    /// [`Volume::start`] with the interval and the sampling function given
    /// (tests). A thread that cannot be spawned leaves the cache empty.
    pub fn start_with(path: PathBuf, every: Duration, sample: Arc<SampleFn>) -> VolumeHandle {
        Self::start_with_guard(path, every, sample, Default::default())
    }

    fn start_with_guard(
        path: PathBuf,
        every: Duration,
        sample: Arc<SampleFn>,
        drain: crate::drain::Drain,
    ) -> VolumeHandle {
        let v = Arc::new(Volume {
            path: path.clone(),
            last: Mutex::new(None),
            stopped: Mutex::new(false),
            wake: Condvar::new(),
        });
        let me = Arc::clone(&v);
        let spawned = std::thread::Builder::new()
            .name("strata-volume".into())
            .spawn(move || me.run(&path, every, &*sample, &drain));
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "the volume sampler thread could not be started");
        }
        VolumeHandle(v)
    }

    fn run(&self, path: &Path, every: Duration, sample: &SampleFn, drain: &crate::drain::Drain) {
        loop {
            if let Some(_pass) = drain.background() {
                let got = sample(path);
                if let Err(e) = &got {
                    tracing::debug!(path = %path.display(), error = %e, "volume sample failed");
                }
                let got = got.ok().map(|space| Sample {
                    space,
                    label: strata_apfs::volume_label(path)
                        .inspect_err(|e| {
                            tracing::debug!(path = %path.display(), error = %e, "volume name read failed")
                        })
                        .ok(),
                    at: SystemTime::now(),
                });
                *lock(&self.last) = got;
            }
            let end = Instant::now() + every;
            let mut stopped = lock(&self.stopped);
            loop {
                if *stopped {
                    return;
                }
                let now = Instant::now();
                if now >= end {
                    break;
                }
                stopped = self
                    .wake
                    .wait_timeout(stopped, end - now)
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
            }
        }
    }

    /// `{free, size, as_of, path, mount?, name?}` of the last sample, or
    /// `null` before the first one succeeded or after one failed. `path` is
    /// the data root that was measured; `mount` and `name` say which volume
    /// holds it (left out when only the name read failed).
    pub fn json(&self) -> Value {
        match &*lock(&self.last) {
            Some(Sample { space, label, at }) => {
                let mut v = json!({
                    "free": space.free,
                    "size": space.size,
                    "as_of": strata_types::serde_rfc3339::format(at).ok(),
                    "path": self.path.to_string_lossy(),
                });
                if let Some(l) = label {
                    v["mount"] = json!(l.mount.to_string_lossy());
                    v["name"] = json!(l.name);
                }
                v
            }
            None => Value::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    fn wait_for(v: &Volume, want: impl Fn(&Value) -> bool) -> Value {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let got = v.json();
            if want(&got) || Instant::now() >= end {
                return got;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_data_root_volume_is_sampled_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let v = Volume::start(dir.path());
        let got = wait_for(&v, |j| !j.is_null());
        let (free, size) = (got["free"].as_u64().unwrap(), got["size"].as_u64().unwrap());
        assert!(size > 0 && free <= size, "{got}");
        assert!(
            strata_types::serde_rfc3339::parse(got["as_of"].as_str().unwrap()).is_ok(),
            "{got}"
        );
        let want = strata_apfs::volume_label(dir.path()).unwrap();
        assert_eq!(got["path"], json!(dir.path().to_string_lossy()), "{got}");
        assert_eq!(got["mount"], json!(want.mount.to_string_lossy()), "{got}");
        assert_eq!(got["name"], json!(want.name), "{got}");
    }

    #[test]
    fn a_failed_sample_is_null_until_one_succeeds() {
        // A path that does not exist: the real statfs fails.
        let dir = tempfile::tempdir().unwrap();
        let v = Volume::start_with(
            dir.path().join("missing"),
            Duration::from_millis(5),
            Arc::new(strata_apfs::volume_space),
        );
        std::thread::sleep(Duration::from_millis(50));
        assert!(v.json().is_null());
        // Success, then failure, then success again.
        let fail = Arc::new(AtomicBool::new(false));
        let f2 = fail.clone();
        let sample = move |_: &Path| {
            if f2.load(Ordering::SeqCst) {
                Err(io::Error::from_raw_os_error(libc::EIO))
            } else {
                Ok(VolumeSpace { free: 1, size: 2 })
            }
        };
        let v = Volume::start_with(
            dir.path().to_path_buf(),
            Duration::from_millis(5),
            Arc::new(sample),
        );
        assert_eq!(wait_for(&v, |j| !j.is_null())["size"], 2);
        fail.store(true, Ordering::SeqCst);
        assert!(wait_for(&v, Value::is_null).is_null());
        fail.store(false, Ordering::SeqCst);
        assert_eq!(wait_for(&v, |j| !j.is_null())["free"], 1);
    }

    #[test]
    fn a_failed_name_read_keeps_the_space_and_leaves_out_mount_and_name() {
        // The space reads (fake) but the path does not exist, so the real
        // name read fails.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let v = Volume::start_with(
            missing.clone(),
            Duration::from_millis(5),
            Arc::new(|_: &Path| Ok(VolumeSpace { free: 1, size: 2 })),
        );
        let got = wait_for(&v, |j| !j.is_null());
        assert_eq!(got["size"], 2, "{got}");
        assert_eq!(got["path"], json!(missing.to_string_lossy()), "{got}");
        assert!(
            got.get("mount").is_none() && got.get("name").is_none(),
            "{got}"
        );
    }

    #[test]
    fn dropping_the_handle_stops_the_sampler() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let c2 = calls.clone();
        let sample = move |p: &Path| {
            c2.fetch_add(1, Ordering::SeqCst);
            strata_apfs::volume_space(p)
        };
        let v = Volume::start_with(
            dir.path().to_path_buf(),
            Duration::from_millis(5),
            Arc::new(sample),
        );
        wait_for(&v, |j| !j.is_null());
        let cache = Arc::clone(&v.0);
        drop(v);
        std::thread::sleep(Duration::from_millis(50));
        let n = calls.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(calls.load(Ordering::SeqCst), n, "no sample after the stop");
        assert!(!cache.json().is_null(), "the last figures stay readable");
    }
}
