//! §5.6: a base's deps set by reuse, adoption or cold install.

use std::path::Path;

use strata_types::{Code, DepsSet};

use crate::build::{build, find_set, inputs_tracked, key_of};
use crate::layout::reason;
use crate::{Ctx, Result, adopt, seal};

/// Adoptions in a row that may see `changed_during_clone` before the cold
/// install takes over (§5.6 step 4).
const ADOPT_TRIES: usize = 3;

/// §5.6: sealed → adopt → cold install. Empty `deps_dirs` seals the empty set.
pub fn ensure(ctx: &Ctx, tree: &Path) -> Result<DepsSet> {
    // 1.
    if ctx.cfg.deps_dirs.is_empty() {
        return seal::seal_empty(ctx);
    }
    // 2. Preflight; nothing written.
    inputs_tracked(ctx, tree)?;
    let (_, id) = key_of(ctx, tree)?;
    // 3. Hot path: no lock.
    if let Some(set) = find_set(ctx.store, ctx.repo, &id)? {
        return Ok(set);
    }
    // 4.
    for _ in 0..ADOPT_TRIES {
        match adopt(ctx, tree) {
            Ok(set) => return Ok(set),
            Err(e) if e.code == Code::Drift && reason(&e) == Some("changed_during_clone") => {
                continue;
            }
            Err(e) if matches!(e.code, Code::Drift | Code::NoSuchPath | Code::NotDirectory) => {
                break;
            }
            Err(e) => return Err(e),
        }
    }
    // 5.
    build(ctx, tree, None)
}
