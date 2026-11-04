//! Best-effort pin history, deliberately separate from the strict pins.json format.
use crate::PinnedFlags;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::SystemTime;
use strata_types::RepoName;

pub const ROTATE_BYTES: u64 = 1024 * 1024;
#[derive(Clone, Debug, Default, Serialize)]
pub struct PinMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relaxed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct Entry {
    pub at: String,
    pub event: String,
    pub repo: RepoName,
    pub from: Option<PinnedFlags>,
    pub to: PinnedFlags,
    /// LocalAuthentication does not disclose Touch ID versus password.
    pub auth: String,
    pub pid: u32,
    pub version: String,
}
impl Entry {
    pub fn new(event: &str, repo: RepoName, from: Option<PinnedFlags>, to: PinnedFlags) -> Self {
        Self {
            at: strata_types::serde_rfc3339::format(&SystemTime::now()).unwrap_or_default(),
            event: event.into(),
            repo,
            from,
            to,
            auth: if event == "config_relax" {
                "local_authentication"
            } else {
                "startup"
            }
            .into(),
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}
pub fn apply(meta: &mut BTreeMap<RepoName, PinMeta>, e: &Entry) {
    if strata_types::serde_rfc3339::parse(&e.at).is_err() {
        return;
    }
    let m = meta.entry(e.repo.clone()).or_default();
    let slot = match e.event.as_str() {
        "config_relax" => &mut m.relaxed_at,
        "tighten" | "pin_new" => &mut m.pinned_at,
        _ => return,
    };
    if slot.as_ref().is_none_or(|at| {
        strata_types::serde_rfc3339::parse(at).ok()
            <= strata_types::serde_rfc3339::parse(&e.at).ok()
    }) {
        *slot = Some(e.at.clone());
    }
}
pub fn load(pins: &Path) -> BTreeMap<RepoName, PinMeta> {
    let mut meta = BTreeMap::new();
    for name in ["audit.jsonl.1", "audit.jsonl"] {
        let p = pins.with_file_name(name);
        let Ok(f) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&p)
        else {
            continue;
        };
        let Ok(metadata) = f.metadata() else { continue };
        if !metadata.is_file() {
            continue;
        }
        if metadata.len() > ROTATE_BYTES + 65536 {
            tracing::warn!(path = %p.display(), bytes = metadata.len(), "oversized pin audit sidecar ignored; timestamps from this file are unavailable");
            continue;
        }
        for line in BufReader::new(f).split(b'\n').map_while(Result::ok) {
            if let Ok(e) = serde_json::from_slice::<Entry>(&line) {
                apply(&mut meta, &e);
            }
        }
    }
    meta
}
pub fn append(pins: &Path, entry: &Entry) -> io::Result<()> {
    let path = pins.with_file_name("audit.jsonl");
    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');
    if let Ok(m) = fs::symlink_metadata(&path) {
        if !m.is_file() {
            return Err(io::Error::other("audit sidecar is not a regular file"));
        }
        if m.len() + line.len() as u64 > ROTATE_BYTES {
            fs::rename(&path, pins.with_file_name("audit.jsonl.1"))?;
        }
    }
    let mut f = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)?;
    f.set_permissions(fs::Permissions::from_mode(0o600))?;
    // A crash may leave a partial final JSON record. Preserve it for inspection
    // but delimit it so the next successful record is independently loadable.
    if f.metadata()?.len() > 0 {
        f.seek(SeekFrom::End(-1))?;
        let mut last = [0];
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            f.write_all(b"\n")?;
        }
    }
    f.write_all(&line)?;
    f.sync_all()?;
    File::open(path.parent().unwrap_or(Path::new(".")))?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotates_reads_both_files_and_ignores_corrupt_lines() {
        let d = tempfile::tempdir().unwrap();
        let pins = d.path().join("pins.json");
        let flags: PinnedFlags = serde_json::from_value(
            serde_json::json!({"confirm":true,"push":false,"deploy_on_push":false}),
        )
        .unwrap();
        let repo = RepoName::try_from("app".to_string()).unwrap();
        append(
            &pins,
            &Entry::new("pin_new", repo.clone(), None, flags.clone()),
        )
        .unwrap();
        let path = pins.with_file_name("audit.jsonl");
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&vec![b' '; ROTATE_BYTES as usize]).unwrap();
        drop(f);
        append(
            &pins,
            &Entry::new("config_relax", repo.clone(), Some(flags.clone()), flags),
        )
        .unwrap();
        assert!(pins.with_file_name("audit.jsonl.1").exists());
        // Rotation can include the padding line; both valid history lines remain usable.
        let meta = load(&pins);
        assert!(meta[&repo].relaxed_at.is_some());
        assert!(meta[&repo].pinned_at.is_some());
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
