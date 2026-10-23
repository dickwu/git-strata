//! `resolve`, `select` and `list` (§5.4).

use std::ffi::OsStr;
use std::path::Component;

use serde_json::json;
use strata_store::{Lifecycle, SessionRecord};
use strata_types::{Code, RepoName, SessionId, StrataError};

use crate::sys::real_dir_io;
use crate::{Result, Sessions, ticket};

impl Sessions<'_> {
    /// The session workspace `cwd` is in (or under), by real path (D16).
    /// Store errors pass through; everything else that does not lead to a
    /// live session's workspace is `None`.
    pub fn resolve(&self, cwd: &std::path::Path) -> Result<Option<(SessionId, RepoName)>> {
        // 1. The volume's spelling, symlinks resolved.
        let Ok(real) = real_dir_io(cwd) else { return Ok(None) };
        // 2. Under the real path of <root>/sessions/.
        let Ok(sessions) = real_dir_io(&self.config.paths.root.join("sessions")) else { return Ok(None) };
        let Ok(rest) = real.strip_prefix(&sessions) else { return Ok(None) };
        let mut parts = rest.components().map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        });
        // 3. A session id segment (`.repos` fails here).
        let Some(Some(sid)) = parts.next() else { return Ok(None) };
        let Some(Ok(id)) = sid.to_str().map(|s| SessionId::try_from(s.to_string())) else { return Ok(None) };
        // 4. A repo segment.
        let Some(Some(seg)) = parts.next() else { return Ok(None) };
        // 5. The store: live, and the manifest has this repo, byte for byte.
        let Some(rec) = self.store.session(&id)? else { return Ok(None) };
        if rec.lifecycle == Lifecycle::Removed {
            return Ok(None);
        }
        Ok(repo_named(&rec, seg).map(|r| (id, r)))
    }

    /// A session by id, or by ticket among live sessions (§5.4).
    pub fn select(&self, selector: &str) -> Result<SessionId> {
        let none = || {
            StrataError::new(Code::NoSuchSession, format!("no session matches {selector:?}"))
                .with_detail(json!({ "selector": selector }))
        };
        if let Ok(id) = SessionId::try_from(selector.to_string()) {
            return match self.store.session(&id)? {
                Some(r) if r.lifecycle != Lifecycle::Removed => Ok(id),
                _ => Err(none()),
            };
        }
        if let Err(mut e) = ticket::validate(selector) {
            // Neither an id nor a ticket: always InvalidInput (§5.4), even
            // for the reserved id-like form `new` calls ReservedName.
            e.code = Code::InvalidInput;
            return Err(e);
        }
        let hits: Vec<SessionRecord> = self
            .live_sorted()?
            .into_iter()
            .filter(|r| r.manifest.ticket.as_deref().map(str::as_bytes) == Some(selector.as_bytes()))
            .collect();
        match hits.len() {
            0 => Err(none()),
            1 => Ok(hits[0].manifest.id.clone()),
            _ => {
                let candidates: Vec<&str> = hits.iter().map(|r| r.manifest.id.as_str()).collect();
                Err(StrataError::new(
                    Code::AmbiguousMatch,
                    format!("ticket {selector:?} names {} sessions", hits.len()),
                )
                .with_detail(json!({ "ticket": selector, "candidates": candidates })))
            }
        }
    }

    /// Sessions that are not Removed, Creating and Unknown included, by
    /// creation time (D17).
    pub fn list(&self) -> Result<Vec<SessionId>> {
        Ok(self.live_sorted()?.into_iter().map(|r| r.manifest.id).collect())
    }

    fn live_sorted(&self) -> Result<Vec<SessionRecord>> {
        let mut v: Vec<SessionRecord> =
            self.store.sessions()?.into_iter().filter(|r| r.lifecycle != Lifecycle::Removed).collect();
        v.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.manifest.id.as_str().cmp(b.manifest.id.as_str())));
        Ok(v)
    }
}

fn repo_named(rec: &SessionRecord, seg: &OsStr) -> Option<RepoName> {
    use std::os::unix::ffi::OsStrExt;
    rec.manifest.repos.keys().find(|r| r.as_str().as_bytes() == seg.as_bytes()).cloned()
}
