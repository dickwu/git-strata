//! `walk`, `walk_with`, `list_dir` (§5.6): a lazy, stack-based preorder walk
//! over `getattrlistbulk`, never following links or crossing mount points.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use strata_types::{Code, RepoPath, StrataError};
use unicode_normalization::UnicodeNormalization;

use crate::attrs::{Attrs, FileKind};
use crate::bulk;
use crate::error::{IoSide, classify_errno, errno_error};
use crate::hooks as seams;
use crate::rooted::{DIR_FLAGS, Fd, components, dir_open_error, open_root_fd, openat};
use crate::sys;
use crate::{DEFAULT_BULK_BUFFER_BYTES, MAX_BULK_BUFFER_BYTES, MAX_WALK_DEPTH, MIN_BULK_BUFFER_BYTES, Result};

thread_local! {
    static WALK_STOP: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>> =
        const { std::cell::RefCell::new(None) };
}

/// status-perf A1: run `f` with every [`Walk`] made on this thread given up
/// once `stop` is set: checked before each directory is read, the walk
/// then yields one `Busy` error (`reason: "stopped"`) and ends. Nests; the
/// previous flag comes back when `f` returns or unwinds.
pub fn with_walk_stop<R>(stop: std::sync::Arc<std::sync::atomic::AtomicBool>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<std::sync::Arc<std::sync::atomic::AtomicBool>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            WALK_STOP.with(|s| *s.borrow_mut() = prev);
        }
    }
    let _restore = Restore(WALK_STOP.with(|s| s.borrow_mut().replace(stop)));
    f()
}

fn walk_stopped() -> bool {
    WALK_STOP.with(|s| s.borrow().as_ref().is_some_and(|b| b.load(std::sync::atomic::Ordering::SeqCst)))
}

/// Options for [`walk_with`]. Default: the whole tree, depth limited only by
/// `MAX_WALK_DEPTH`, a `DEFAULT_BULK_BUFFER_BYTES` buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkOptions {
    /// Start at this directory under the root (resolved one level at a time
    /// with O_NOFOLLOW); `None` = the root. Yielded paths stay root-relative.
    pub start: Option<RepoPath>,
    /// `Some(n)`: only entries within `n` levels of `start`; directories at
    /// level `n` are yielded as `Dir` but not read. `Some(1)` = one level.
    pub max_depth: Option<u32>,
    /// `getattrlistbulk` buffer size, clamped to
    /// `[MIN_BULK_BUFFER_BYTES, MAX_BULK_BUFFER_BYTES]`.
    pub buffer_bytes: usize,
}

/// Hand-written: a derived default would give `buffer_bytes = 0` (clamped to
/// 1 KiB) instead of 64 KiB.
impl Default for WalkOptions {
    fn default() -> Self {
        WalkOptions { start: None, max_depth: None, buffer_bytes: DEFAULT_BULK_BUFFER_BYTES }
    }
}

type PathFilter<'f> = &'f (dyn Fn(&Path) -> bool + Send + Sync);
type AttrFilter<'f> = &'f (dyn Fn(&Path, &Attrs) -> bool + Send + Sync);

enum Filter<'f> {
    Path(PathFilter<'f>),
    Attrs(AttrFilter<'f>),
}

impl Filter<'_> {
    fn keep(&self, p: &Path, a: &Attrs) -> bool {
        match self {
            Filter::Path(f) => f(p),
            Filter::Attrs(f) => f(p, a),
        }
    }
}

struct Entry {
    rel: RepoPath,
    disk_name: String,
    attrs: Attrs,
    mount_point: bool,
}

struct Frame {
    fd: Fd,
    rel: Option<RepoPath>,
    depth: u32,
    entries: Option<VecDeque<Result<Entry>>>,
}

struct Descend {
    disk_name: String,
    rel: RepoPath,
    depth: u32,
}

/// The lazy walk iterator. The root is opened on the first `next()`.
pub struct Walk<'f> {
    root: PathBuf,
    opts: WalkOptions,
    filter: Filter<'f>,
    started: bool,
    done: bool,
    root_id: (libc::dev_t, libc::ino_t),
    stack: Vec<Frame>,
    descend: Option<Descend>,
    out: VecDeque<Result<(RepoPath, Attrs)>>,
}

/// Preorder walk of everything under `root` (not `root` itself); never follows
/// symlinks, never enters mount points. Siblings are sorted by `RepoPath`
/// bytes (NFC) within each directory; the whole output is NOT full-path byte
/// order. `filter` returning `false` drops the entry and its subtree.
pub fn walk<'f>(root: &Path, filter: &'f (dyn Fn(&Path) -> bool + Send + Sync)) -> Walk<'f> {
    Walk::new(root, &WalkOptions::default(), Filter::Path(filter))
}

/// [`walk`] with options; the filter also sees the attributes.
pub fn walk_with<'f>(root: &Path, opts: &WalkOptions, filter: &'f (dyn Fn(&Path, &Attrs) -> bool + Send + Sync)) -> Walk<'f> {
    Walk::new(root, opts, Filter::Attrs(filter))
}

fn allow_all(_: &Path, _: &Attrs) -> bool {
    true
}

/// The direct children of `dir` under `root` (`None` = the root): `walk_with`
/// with `start = dir`, `max_depth = Some(1)`, everything allowed.
pub fn list_dir(root: &Path, dir: Option<&RepoPath>) -> Walk<'static> {
    let opts = WalkOptions { start: dir.cloned(), max_depth: Some(1), ..Default::default() };
    walk_with(root, &opts, &allow_all)
}

fn unreadable(errno: i32, shown: &Path) -> StrataError {
    errno_error(Code::Unknown, errno, shown, Some("unreadable_entry"))
}

impl<'f> Walk<'f> {
    fn new(root: &Path, opts: &WalkOptions, filter: Filter<'f>) -> Walk<'f> {
        Walk {
            root: root.to_path_buf(),
            opts: opts.clone(),
            filter,
            started: false,
            done: false,
            root_id: (0, 0),
            stack: Vec::new(),
            descend: None,
            out: VecDeque::new(),
        }
    }

    fn shown(&self, rel: Option<&RepoPath>) -> PathBuf {
        match rel {
            Some(r) => self.root.join(r.as_str()),
            None => self.root.clone(),
        }
    }

    /// §5.6 steps 1–3.
    fn start(&mut self) -> Result<()> {
        let fd = open_root_fd(&self.root, IoSide::Read)?;
        let st = sys::fstat(fd.0).map_err(|e| classify_errno(e, IoSide::Read, &self.root))?;
        self.root_id = (st.st_dev, st.st_ino);
        let mut dir = fd;
        if let Some(s) = self.opts.start.clone() {
            let mut walked = self.root.clone();
            for (step, comp) in components(&s).into_iter().enumerate() {
                seams::before_open(step, comp);
                walked.push(comp);
                match openat(dir.0, comp, DIR_FLAGS, 0) {
                    Ok(next) => dir = next,
                    Err(e) => return Err(dir_open_error(e, Some(dir.0), comp, IoSide::Read, &walked)),
                }
            }
        }
        self.stack.push(Frame { fd: dir, rel: self.opts.start.clone(), depth: 0, entries: None });
        Ok(())
    }

    /// §5.6 step 5.4: open a child directory and push it.
    fn push_child(&mut self, d: Descend) {
        let shown = self.shown(Some(&d.rel));
        let parent = self.stack.last().expect("a parent frame").fd.0;
        let name = OsStr::new(&d.disk_name);
        seams::before_open(d.depth as usize, name);
        match openat(parent, name, DIR_FLAGS, 0) {
            Ok(fd) => self.stack.push(Frame { fd, rel: Some(d.rel), depth: d.depth, entries: None }),
            // Gone, or no longer a directory: as if it were never there.
            Err(libc::ENOENT) | Err(libc::ENOTDIR) | Err(libc::ELOOP) => {}
            Err(libc::EACCES) => self.out.push_back(Err(unreadable(libc::EACCES, &shown))),
            Err(e @ (libc::EMFILE | libc::ENFILE)) => self.out.push_back(Err(errno_error(Code::Unknown, e, &shown, None))),
            Err(e) => self.out.push_back(Err(classify_errno(e, IoSide::Read, &shown))),
        }
    }

    /// §5.6 step 7.
    fn finish(&mut self) {
        self.done = true;
        let same = matches!(sys::lstat(&self.root), Ok(st) if (st.st_dev, st.st_ino) == self.root_id);
        if !same {
            self.out.push_back(Err(errno_error(Code::Busy, libc::EBUSY, &self.root, Some("root_changed"))));
        }
    }
}

/// `read_dir` step 3: NFC the on-disk name and join it to the parent.
fn entry_path(rel: Option<&RepoPath>, name: &str) -> Result<RepoPath> {
    let nfc: String = name.nfc().collect();
    let joined = match rel {
        Some(r) => format!("{}/{}", r.as_str(), nfc),
        None => nfc,
    };
    RepoPath::try_from(joined).map_err(|mut e| {
        e.code = Code::InvalidInput;
        e
    })
}

/// `read_dir` (§5.6): every entry of the directory open at `fd`, sorted by
/// `RepoPath` bytes.
fn read_dir(fd: &Fd, rel: Option<&RepoPath>, shown: &Path, buffer_bytes: usize) -> Result<VecDeque<Result<Entry>>> {
    let parent_dev = sys::fstat(fd.0).map_err(|e| classify_errno(e, IoSide::Read, shown))?.st_dev;
    let mut size = buffer_bytes.clamp(MIN_BULK_BUFFER_BYTES, MAX_BULK_BUFFER_BYTES);
    let mut buf = vec![0u8; size];
    let mut al = bulk::attrlist(true);
    let mut ok: Vec<Entry> = Vec::new();
    let mut errs: Vec<StrataError> = Vec::new();
    loop {
        let n = unsafe {
            libc::getattrlistbulk(
                fd.0,
                (&mut al as *mut libc::attrlist).cast(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                libc::FSOPT_ATTR_CMN_EXTENDED as u64,
            )
        };
        if n < 0 {
            match sys::errno() {
                libc::EINTR => continue,
                libc::ERANGE if size < MAX_BULK_BUFFER_BYTES => {
                    size = (size * 2).min(MAX_BULK_BUFFER_BYTES);
                    buf.resize(size, 0);
                    continue;
                }
                libc::ERANGE => return Err(errno_error(Code::Unknown, libc::ERANGE, shown, Some("record_too_large"))),
                libc::ENOENT => break,
                e => return Err(unreadable(e, shown)),
            }
        }
        if n == 0 {
            break;
        }
        // Parse the window, fire the seam, then finish the entries (fallback
        // stat, readlinkat): entries that vanish in between are skipped.
        let mut raws = Vec::new();
        for rec in bulk::parse_records(&buf, n as usize) {
            match rec {
                Ok(r) => raws.push(r),
                Err(mut e) => {
                    crate::error::set_detail(&mut e, "path", shown.to_string_lossy().into());
                    errs.push(e);
                }
            }
        }
        seams::after_bulk_window(n as usize);
        for raw in raws {
            let Some(fin) = bulk::finish_entry(fd.0, parent_dev, &raw) else { continue };
            let disk = raw.name.as_deref().unwrap_or_default();
            let shown_child = shown.join(disk);
            let fin = match fin {
                Ok(f) => f,
                Err(e) => {
                    errs.push(classify_errno(e, IoSide::Read, &shown_child));
                    continue;
                }
            };
            match entry_path(rel, &fin.name) {
                Ok(p) => ok.push(Entry { rel: p, disk_name: fin.name, attrs: fin.attrs, mount_point: fin.mount_point }),
                Err(mut e) => {
                    crate::error::set_detail(&mut e, "path", shown_child.to_string_lossy().into());
                    errs.push(e);
                }
            }
        }
    }
    ok.sort_by(|a, b| a.rel.cmp(&b.rel));
    let mut q: VecDeque<Result<Entry>> = ok.into_iter().map(Ok).collect();
    q.extend(errs.into_iter().map(Err));
    Ok(q)
}

impl Iterator for Walk<'_> {
    type Item = Result<(RepoPath, Attrs)>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.out.pop_front() {
                return Some(item);
            }
            if self.done {
                return None;
            }
            if !self.started {
                self.started = true;
                if let Err(e) = self.start() {
                    self.done = true;
                    self.stack.clear();
                    return Some(Err(e));
                }
                continue;
            }
            if let Some(d) = self.descend.take() {
                self.push_child(d);
                continue;
            }
            let buffer_bytes = self.opts.buffer_bytes;
            let Some(top) = self.stack.last_mut() else {
                self.finish();
                continue;
            };
            if top.entries.is_none() {
                if walk_stopped() {
                    self.done = true;
                    self.stack.clear();
                    let e = StrataError::new(Code::Busy, "walk stopped")
                        .with_detail(serde_json::json!({ "reason": "stopped" }));
                    return Some(Err(e));
                }
                let shown = match &top.rel {
                    Some(r) => self.root.join(r.as_str()),
                    None => self.root.clone(),
                };
                match read_dir(&top.fd, top.rel.as_ref(), &shown, buffer_bytes) {
                    // A directory at MAX_WALK_DEPTH is read but its entries
                    // (one level deeper) are never yielded (addendum 8).
                    Ok(q) if top.depth >= MAX_WALK_DEPTH && !q.is_empty() => {
                        top.entries = Some(VecDeque::new());
                        let mut e = errno_error(Code::TooLarge, libc::ELOOP, &shown, Some("too_deep"));
                        crate::error::set_detail(&mut e, "max_depth", MAX_WALK_DEPTH.into());
                        return Some(Err(e));
                    }
                    Ok(q) => top.entries = Some(q),
                    Err(e) => {
                        top.entries = Some(VecDeque::new());
                        return Some(Err(e));
                    }
                }
                continue;
            }
            let depth = top.depth + 1;
            match top.entries.as_mut().unwrap().pop_front() {
                None => {
                    self.stack.pop();
                }
                Some(Err(e)) => return Some(Err(e)),
                Some(Ok(e)) => {
                    if !self.filter.keep(Path::new(e.rel.as_str()), &e.attrs) {
                        continue;
                    }
                    let within = self.opts.max_depth.is_none_or(|n| depth < n);
                    if e.attrs.kind == FileKind::Dir && !e.mount_point && within {
                        self.descend = Some(Descend { disk_name: e.disk_name, rel: e.rel.clone(), depth });
                    }
                    return Some(Ok((e.rel, e.attrs)));
                }
            }
        }
    }
}

const _: () = {
    const fn send<T: Send>() {}
    send::<Walk<'static>>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bulk::tests::{Rec, build};

    #[test]
    fn walk_parser_yields_nfc_names() {
        let nfd = "cafe\u{301}";
        let rec = Rec { name: Some(nfd.as_bytes().to_vec()), objtype: Some(1), mtime: Some((1, 0)), ctime: Some((1, 0)), mode: Some(0o644), fileid: Some(9), len: Some(0), ..Default::default() };
        let raw = bulk::parse_records(&build(&rec), 1).pop().unwrap().unwrap();
        assert_eq!(raw.name.as_deref(), Some(nfd), "the parser keeps the disk bytes");
        let parent = RepoPath::try_from("d".to_string()).unwrap();
        assert_eq!(entry_path(Some(&parent), raw.name.as_deref().unwrap()).unwrap().as_str(), "d/caf\u{e9}");
        assert_eq!(entry_path(None, nfd).unwrap().as_str(), "caf\u{e9}");
    }
}
