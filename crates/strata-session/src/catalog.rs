//! Immutable configuration snapshots. The arena owns every published value
//! until shutdown; thread bindings contain indices, never borrowed pointers.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread::{Scope, ScopedJoinHandle};

use strata_gitio::Git;
use strata_store::DevPortsPolicy;
use strata_types::{Code, Config, RepoName, StrataError};

use crate::Result;

/// The lower layers borrow only these parts of a daemon catalog.
pub trait CatalogSource: Sync {
    fn config(&self) -> Result<&Config>;
    fn gits(&self) -> Result<&BTreeMap<RepoName, Git>>;
    fn dev_ports(&self) -> Result<Option<&DevPortsPolicy>>;
}

#[derive(Clone)]
struct Binding {
    cell: u64,
    index: usize,
    generation: u64,
}
thread_local! {
    static BINDINGS: RefCell<Vec<Binding>> = const { RefCell::new(Vec::new()) };
}
static NEXT_CELL: AtomicU64 = AtomicU64::new(1);

/// Missing propagation fails closed at a fence, never grants an exemption.
pub fn generation() -> u64 {
    BINDINGS.with(|b| b.borrow().last().map_or(0, |b| b.generation))
}

/// Generation of a specific daemon cell; nested operations on another daemon
/// cannot lend their admission generation to this one's lock manager.
pub fn generation_for(cell: u64) -> u64 {
    BINDINGS.with(|b| {
        b.borrow()
            .iter()
            .rev()
            .find(|b| b.cell == cell)
            .map_or(0, |b| b.generation)
    })
}

/// Stable-address, append-only arena. Publication is serialized by the owner.
pub struct CatalogCell<T: Send + Sync> {
    id: u64,
    // Boxes deliberately keep addresses stable when the Vec reallocates.
    #[allow(clippy::vec_box)]
    values: Mutex<Vec<Box<T>>>,
    current: AtomicUsize,
}

impl<T: Send + Sync> CatalogCell<T> {
    pub fn new(initial: T) -> Self {
        Self {
            id: NEXT_CELL.fetch_add(1, Ordering::Relaxed),
            values: Mutex::new(vec![Box::new(initial)]),
            current: AtomicUsize::new(0),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    fn at(&self, index: usize) -> &T {
        let values = self.values.lock().unwrap_or_else(|p| p.into_inner());
        let ptr: *const T = values[index].as_ref();
        drop(values);
        // SAFETY: entries are immutable boxed values, never removed/replaced.
        // The borrow of self prevents destruction of the arena while used.
        unsafe { &*ptr }
    }

    pub fn current(&self) -> &T {
        self.at(self.current.load(Ordering::Acquire))
    }

    pub fn publish(&self, next: T) {
        let mut values = self.values.lock().unwrap_or_else(|p| p.into_inner());
        let index = values.len();
        values.push(Box::new(next));
        self.current.store(index, Ordering::Release);
    }

    pub fn len(&self) -> usize {
        self.values.lock().unwrap_or_else(|p| p.into_inner()).len()
    }
    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn enter(&self, generation: impl FnOnce(&T) -> u64) -> Pin<'_> {
        let index = self.current.load(Ordering::Acquire);
        let binding = Binding {
            cell: self.id,
            index,
            generation: generation(self.at(index)),
        };
        let previous = BINDINGS.with(|b| {
            let old = b.borrow().clone();
            b.borrow_mut().push(binding);
            old
        });
        Pin {
            previous,
            _borrow: PhantomData,
            _thread: PhantomData,
        }
    }

    pub fn pinned(&self) -> Result<&T> {
        let index = BINDINGS.with(|b| {
            b.borrow()
                .iter()
                .rev()
                .find(|b| b.cell == self.id)
                .map(|b| b.index)
        });
        match index {
            Some(i) => Ok(self.at(i)),
            None => Err(
                StrataError::new(Code::Corrupt, "operation has no pinned catalog")
                    .with_detail(serde_json::json!({ "reason": "no_pinned_catalog" })),
            ),
        }
    }
}

/// Restores nested bindings on normal return and panic; cannot cross threads.
pub struct Pin<'a> {
    previous: Vec<Binding>,
    _borrow: PhantomData<&'a ()>,
    _thread: PhantomData<Rc<()>>,
}
impl Drop for Pin<'_> {
    fn drop(&mut self) {
        BINDINGS.with(|b| *b.borrow_mut() = std::mem::take(&mut self.previous));
    }
}

/// A scoped helper inherits the parent's exact catalog, even after a reload.
pub fn spawn_pinned<'s, 'e, F, T>(scope: &'s Scope<'s, 'e>, f: F) -> ScopedJoinHandle<'s, T>
where
    F: FnOnce() -> T + Send + 's,
    T: Send + 's,
{
    let bindings = BINDINGS.with(|b| b.borrow().clone());
    scope.spawn(move || {
        let previous = BINDINGS.with(|b| b.replace(bindings));
        let _pin = Pin {
            previous,
            _borrow: PhantomData,
            _thread: PhantomData,
        };
        f()
    })
}

/// Reuse the workspace's existing cryptographic content digest implementation.
pub fn content_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_borrows_and_helpers_keep_their_catalog() {
        let cell = CatalogCell::new((1, "old"));
        assert_eq!(cell.pinned().unwrap_err().code, Code::Corrupt);
        let _pin = cell.enter(|v| v.0);
        let old = cell.pinned().unwrap();
        for i in 2..100 {
            cell.publish((i, "new"));
        }
        assert_eq!(old, &(1, "old"));
        std::thread::scope(|s| {
            spawn_pinned(s, || {
                assert_eq!(cell.pinned().unwrap(), old);
                assert_eq!(generation(), 1);
            })
            .join()
            .unwrap();
        });
        assert_eq!(cell.current(), &(99, "new"));
    }
}
