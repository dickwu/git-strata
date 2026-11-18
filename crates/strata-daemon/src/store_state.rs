//! The App's schema snapshot when the daemon is down.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::SystemTime;

use serde_json::{Value, json};
use strata_apfs::{IoSide, classify_io_error, full_fsync};
use strata_store::Store;
use strata_types::{Code, STORE_SCHEMA, StrataError, serde_rfc3339};

use crate::Result;

/// Called under the daemon's instance lock. Opening an existing store can
/// migrate it, so invalidate the last snapshot before handing off to the store.
pub(super) fn open(config_dir: &Path, root: &Path, checkouts: &[&Path]) -> Result<Store> {
    let before = migration_backups(root);
    write(
        config_dir,
        json!({ "root": root, "migrating_to": STORE_SCHEMA }),
    )?;
    let store = Store::open(root, checkouts)?;

    let migrated_from = before.and_then(|before| {
        let mut added = migration_backups(root)?
            .into_iter()
            .filter(|(name, _)| !before.contains_key(name));
        let (_, from) = added.next()?;
        added.next().is_none().then_some(from)
    });
    let at = serde_rfc3339::format(&SystemTime::now())
        .map_err(|e| StrataError::new(Code::Unknown, e))?;
    write(
        config_dir,
        json!({
            "root": root,
            "user_version": STORE_SCHEMA,
            "migrated_from": migrated_from,
            "at": at,
        }),
    )?;
    Ok(store)
}

/// The store writes a verified `store-v<from>-<ns>.db` backup before the
/// migration transaction. A new backup and a successful open give us the
/// source schema, including when the pre-migration version was in the WAL.
fn migration_backups(root: &Path) -> Option<BTreeMap<OsString, u32>> {
    let mut versions = BTreeMap::new();
    let entries = match fs::read_dir(root.join("store/backups")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Some(versions),
        Err(_) => return None,
    };
    for entry in entries {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let Some((version, nanos)) = name
            .to_str()
            .and_then(|s| s.strip_prefix("store-v"))
            .and_then(|s| s.strip_suffix(".db"))
            .and_then(|s| s.split_once('-'))
        else {
            continue;
        };
        if let Ok(version) = version.parse::<u32>()
            && (1..STORE_SCHEMA).contains(&version)
            && nanos.parse::<u128>().is_ok()
            && entry.file_type().ok()?.is_file()
        {
            versions.insert(name, version);
        }
    }
    Some(versions)
}

fn write(config_dir: &Path, state: Value) -> Result<()> {
    let path = config_dir.join("store_state.json");
    let mut random = [0u8; 16];
    // SAFETY: the buffer is writable for the length passed to arc4random_buf.
    unsafe { libc::arc4random_buf(random.as_mut_ptr().cast(), random.len()) };
    let tmp = config_dir.join(format!(
        ".store_state.json.{:032x}.tmp",
        u128::from_ne_bytes(random)
    ));
    let io = |e, path: &Path| classify_io_error(&e, IoSide::Write, path);

    let dir = File::open(config_dir).map_err(|e| io(e, config_dir))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&tmp)
        .map_err(|e| io(e, &tmp))?;
    let install = (|| {
        writeln!(file, "{state}").map_err(|e| io(e, &tmp))?;
        full_fsync(&file)?;
        fs::rename(&tmp, &path).map_err(|e| io(e, &path))
    })();
    if install.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    install?;
    full_fsync(&dir)
}
