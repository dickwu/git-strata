//! The App's schema snapshot when the daemon is down.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::SystemTime;

use serde_json::{Value, json};
use strata_apfs::{IoSide, classify_io_error, full_fsync};
use strata_store::Store;
use strata_types::{Code, STORE_SCHEMA, StrataError, serde_rfc3339};

use crate::Result;

/// Replace the last snapshot only when the store is about to change schema
/// or has finished opening. A rejected open can leave valid evidence intact.
pub(super) fn open(config_dir: &Path, root: &Path, checkouts: &[&Path]) -> Result<Store> {
    let mut migrated_from = None;
    let store = Store::open_with_schema_hook(root, checkouts, |from| {
        migrated_from = from;
        write(
            config_dir,
            json!({ "root": root, "migrating_to": STORE_SCHEMA }),
        )
    })?;

    let at = serde_rfc3339::format(&SystemTime::now())
        .map_err(|e| StrataError::new(Code::Unknown, e))?;
    write(
        config_dir,
        json!({
            "root": root,
            "user_version": STORE_SCHEMA,
            "daemon_version": env!("CARGO_PKG_VERSION"),
            "migrated_from": migrated_from,
            "at": at,
        }),
    )?;
    Ok(store)
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
