//! store.md §7, Task 1: the L1 writer lock is not inherited by children.
//!
//! Kept in its own test binary on purpose. On macOS a spawned child holds a
//! reference to every descriptor of the parent, `O_CLOEXEC` ones included,
//! until its `exec` finishes image activation (under load that can take
//! 100 ms or more). A `flock` lives on the open file description, so while a
//! child is mid-spawn a sibling test that closes and reopens its own store
//! sees `Busy`. That is a property of `flock` + spawn, not of the engine; the
//! fix is to keep spawning tests out of binaries whose tests reopen stores.

mod support;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use support::*;

#[test]
fn lock_is_not_inherited_by_child_processes() {
    let env = env();
    let s = env.open();
    // The shell prints only after its own exec completed, so by the time we
    // read the line every O_CLOEXEC descriptor is gone from the child. If the
    // lock were inherited, the sleeper would keep it for 60 s.
    let mut child = Command::new("/bin/sh")
        .args(["-c", "echo ready; exec sleep 60"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "ready");
    s.close().unwrap();
    let s2 = env.try_open();
    let alive = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let _ = child.wait();
    assert!(alive, "the sleeper should still be running");
    s2.expect("open after the writer is closed, child still alive").close().unwrap();
}
