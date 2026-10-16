# Deferred work

## Optional bypass of the FUSE content cache

Status: deferred by the user on 2026-10-16. Not implemented. Keep the current behavior until this work is explicitly requested.

- Goal: make ordinary reads and writes that use the FUSE mount reach toda even when file contents were previously cached by the kernel.
- Candidate approach: add an opt-in mode that returns `FOPEN_DIRECT_IO` for file open and create operations. Keep the current mode as the default.
- Tradeoffs: bypassing the cache can reduce performance and affect `mmap` compatibility. Validate mapped-file behavior before choosing the implementation.
- Boundary: this cannot disable application-level caches or guarantee that every mapped-memory access becomes a FUSE request.
- Verification when resumed: test repeated reads after warming the cache, rule changes on an already-open file, newly created files, and mapped-file compatibility.
