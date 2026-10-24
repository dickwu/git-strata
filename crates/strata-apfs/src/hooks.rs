//! Thread-local test hooks (§3 `seams`). Compiled always so the call sites
#![allow(dead_code)]
//! need no `cfg`; the setters are public only with `feature = "test-seams"`,
//! and without it the hooks can never be set, so the calls are no-ops.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::rc::Rc;

/// The kind of change `make_removable` / `set_file_meta` is about to make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetaChange {
    Flags,
    Mode,
    Mtime,
}

type OpenHook = Rc<dyn Fn(usize, &OsStr)>;
type BoxedOpenHook = Box<dyn Fn(usize, &OsStr)>;
type MetaHook = Rc<dyn Fn(usize, MetaChange)>;
type BulkHook = Rc<dyn Fn(usize)>;

thread_local! {
    static BEFORE_OPEN: RefCell<Option<OpenHook>> = const { RefCell::new(None) };
    static BEFORE_META: RefCell<Option<MetaHook>> = const { RefCell::new(None) };
    static AFTER_BULK: RefCell<Option<BulkHook>> = const { RefCell::new(None) };
}

// The hook is cloned out of the cell before it runs, so a hook may call back
// into this crate (which fires hooks again) without a RefCell double borrow.

pub(crate) fn before_open(step: usize, name: &OsStr) {
    if let Some(h) = BEFORE_OPEN.with(|c| c.borrow().clone()) {
        h(step, name);
    }
}

pub(crate) fn before_meta(step: usize, change: MetaChange) {
    if let Some(h) = BEFORE_META.with(|c| c.borrow().clone()) {
        h(step, change);
    }
}

pub(crate) fn after_bulk_window(n: usize) {
    if let Some(h) = AFTER_BULK.with(|c| c.borrow().clone()) {
        h(n);
    }
}

#[cfg_attr(not(feature = "test-seams"), allow(dead_code))]
pub fn set_before_open(hook: Option<BoxedOpenHook>) {
    BEFORE_OPEN.with(|c| *c.borrow_mut() = hook.map(Rc::from));
}

#[cfg_attr(not(feature = "test-seams"), allow(dead_code))]
pub fn set_before_meta(hook: Option<Box<dyn Fn(usize, MetaChange)>>) {
    BEFORE_META.with(|c| *c.borrow_mut() = hook.map(Rc::from));
}

#[cfg_attr(not(feature = "test-seams"), allow(dead_code))]
pub fn set_after_bulk_window(hook: Option<Box<dyn Fn(usize)>>) {
    AFTER_BULK.with(|c| *c.borrow_mut() = hook.map(Rc::from));
}
