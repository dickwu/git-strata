# Worker handover parking

During H2 the worker parks writes and four read-class RPCs. A
`publish_plan` can refresh tracking refs, a cross-base `diff` can store
blobs, and cold `grep` and `files` requests can publish an index. Each takes
one admission at dispatch and holds it until its handler returns. Other reads
continue.

An ordinary abort replays parked requests in order on their original streams.
A terminal stop replies to untouched parked requests and complete buffered
frames by ID with retry-safe `Busy upgrading`. Their handlers never start.
A connection that already answers `generation_changed` refusals first answers
the complete frames queued on it; the stop never waits for a client that keeps
sending. Once admitted work and these refusals finish, an H2 stop allows two
seconds for final replies and then closes writes to a client that stopped reading.

`git-strata daemon status` shows the target PID, parked count, and outstanding
work on workers with `daemon.handover`. The queue holds 64 connections or
32 MiB, whichever fills first. Overflow returns a retry-safe `Busy upgrading`.
A shipped 0.3.x hook can miss a checkpoint on overflow. A control client that
receives `generation_changed: true` must reconnect and check the worker identity.
