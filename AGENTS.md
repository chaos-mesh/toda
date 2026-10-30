# Instructions for AI coding agents

These instructions apply to the whole repository. Use them to choose which files to read, where to make a change, and how to verify it.

## Start a task

1. Read the user request and inspect `git status --short` and the existing diff. Preserve changes that are outside your task.
2. Read [README.md](README.md) for the public CLI and configuration workflow. Use the task map below to find the relevant implementation and tests.
3. Check the host platform and installed Rust toolchain before running Cargo or runtime tests. This project targets Linux x86-64 and AArch64 and pins `1.99.1`.
4. Make the smallest change that satisfies the request. Choose verification from the table below. If a required environment is unavailable, complete the work you can verify and report the missing checks.

## Project and execution model

toda is a Rust 2018 filesystem fault injector for Chaos Mesh. Its runtime targets Linux x86-64 and AArch64 and combines FUSE forwarding with `ptrace` replacement of existing process references. Target processes must use the same native 64-bit architecture as toda. The binary imports the shared implementation from the library crate.

The CLI starts with an empty `Vec<InjectorConfig>`. It moves the target mount to a sibling `__chaosfs__<name>__` path, mounts `HookFs` at the original path, redirects existing references, and enables injection. A separate thread serves JSON-RPC over standard input and standard output. SIGINT and SIGTERM start explicit recovery. `--mount-only` skips process replacement in both directions.

## Find the files for a task

Start with the row that matches the requested change. Read callers and related tests before editing:

| Task | Files to read | Responsibility |
| --- | --- | --- |
| CLI and lifecycle | `src/main.rs` | Arguments, logging, injection order, signal handling, and recovery. |
| Library exports | `src/lib.rs` | Public modules used by integration tests and library callers. |
| Control protocol | `src/jsonrpc.rs` | `get_status`, complete rule-set updates, and RPC responses. |
| Rule model and dispatch | `src/injector/injector_config.rs`, `src/injector/multi_injector.rs` | Serialized configuration and construction of the ordered injector list. |
| Injection behavior | `src/injector/` | Filters and latency, fault, attribute override, and data corruption hooks. |
| Filesystem operations | `src/hookfs/mod.rs` | Backing filesystem operations, inode paths, file handles, directory handles, and injection calls. |
| FUSE adapter | `src/hookfs/async_fs.rs`, `src/hookfs/reply.rs`, `src/hookfs/errors.rs`, `src/hookfs/runtime.rs` | Callback scheduling, kernel replies, error conversion, and the shared Tokio runtime. |
| Mount management | `src/mount.rs`, `src/mount_injector.rs`, `src/fuse_device.rs` | Mount discovery, mount moves, FUSE device setup, and explicit mount recovery. |
| Process replacement | `src/replacer/` | File descriptor, working directory, and memory mapping replacement transactions and rollback. |
| Traced execution | `src/ptrace/mod.rs`, `src/ptrace/arch.rs` | Thread ownership and stop verification, architecture-specific registers and syscalls, memory access, and explicit release. |
| Shared helpers | `src/utils.rs`, `src/stop.rs` | Backing path construction and thread coordination. |
| Verification | `tests/jsonrpc_test.rs`, `tests/posix_test.rs`, `tests/mount_injector_test.rs` | RPC handlers, FUSE filesystem behavior, and mount lifecycle integration tests. |
| Build and release | `Cargo.toml`, `Cargo.lock`, `rust-toolchain`, `.cargo/config.toml`, `Dockerfile`, `Makefile`, `.github/workflows/` | Dependencies, toolchain, linker flags, image builds, CI, and release packaging. |
| Manual examples | `config-examples/`, `example/` | RPC configuration examples and a Go filesystem workload. |

## Follow these editing rules

- Keep `1.99.1` and the committed `Cargo.lock` unless the task includes a toolchain or dependency change. The project uses stable Rust. Do not introduce nightly feature gates to work around a dependency mismatch.
- Preserve `-C relro-level=full` in `.cargo/config.toml` and the Docker build. It resolves shared-library symbols before mount changes.
- Follow the stable rustfmt settings in `rustfmt.toml`. Avoid unrelated formatting churn.
- Declare shared top-level modules in `src/lib.rs`. Import them from `toda` in `src/main.rs` as needed; do not compile a second private module tree in the binary.
- Keep FUSE behavior in `hookfs`, rule behavior in `injector`, process-reference replacement in `replacer`, and low-level traced execution in `ptrace`.
- For a new injector, update `InjectorConfig`, `MultiInjector::build`, the relevant `Injector` hooks, and configuration examples together. Check the FUSE call sites that invoke those hooks.
- Preserve the public configuration shape unless the requested change includes a protocol change. Rules use `type` tags, camelCase fields, flattened filters, and a complete rule array as the single argument to `update`.
- Keep stdout available for JSON-RPC responses. Send diagnostics to stderr through the existing tracing setup.

## Check the contracts affected by your change

- For mount or process replacement changes, trace both `Injection::start` and `Injection::recover` in `src/main.rs`. Recovery is explicit. Dropping `MountInjectionGuard` does not restore the mount. Keep unfinished replacement and mount state owned on errors. Once reverse replacement succeeds, clean up retained FUSE references while targets stay stopped, restore the mount, then release targets. Check failure paths, pending delays, mount restoration, and traced-thread detachment. After successful mount restoration, directory cleanup is best effort: an absent directory is already clean; other removal failures are warnings and must preserve remaining contents.
- For path changes, preserve the distinction between the backing path and the visible path. `HookFs` translates backing paths to the original visible paths before filtering. Check mount management and `src/utils.rs` for the same backing path convention. Open-file attributes must come from the backing descriptor after unlink or rename-overwrite. For GETATTR filters, prefer the newest tracked path whose backing device and inode still match that descriptor; fall back to the saved opening path when no candidate can be verified. Release the file and inode table locks before filesystem calls or injected delays.
- For injector changes, preserve configuration order and first-error behavior in fallible hooks unless the task changes those contracts. `update` builds a complete replacement list before taking the write lock. Inspect lock scope and the affected FUSE call sites.
- For configuration validation changes, account for current permissive behavior. Unknown method names are discarded, invalid glob patterns become unrestricted path filters, and `percent` is not range-checked. Changing these behaviors affects existing configurations.
- For RPC changes, check both `result` and JSON-RPC error behavior. `get_status` records initial setup rather than continuous mount health. Some failures are strings in `result`; malformed parameters produce JSON-RPC errors.
- For architecture changes, inspect both register and syscall backends in `ptrace` and the shared transactions in `replacer`. Check syscall restart, instruction alignment, page size, signal state, rollback, and thread release on both architectures. `--mount-only` skips replacement at runtime but does not make the crate portable to other operating systems or architectures.
- Mapping replacement requires runtime-probed support for `MREMAP_DONTUNMAP` on regular file mappings. Preserve the original mappings until the replacement batch can commit. Reject unsupported mapping types before changing originals. Do not infer kernel or page-size coverage from a successful build.

## Select and run verification

Run Cargo commands from the repository root on Linux x86-64 or AArch64 with the pinned toolchain and the dependencies listed in README.md. Verify architecture changes on both platforms. Select checks for the changed behavior rather than treating every row as a command sequence.

Before running POSIX tests, prepare a disposable environment. These tests recursively delete and recreate directories under `/tmp/test_mnt` and `/tmp/test_mnt_backend`. They mount FUSE with `allow_other`, require access to `/dev/fuse`, and need `user_allow_other` in `/etc/fuse.conf` for non-root users. Mount `fusectl` at `/sys/fs/fuse/connections` as shown in README.md; the session guard waits for its pending-request count to reach zero before unmounting and joining the dispatcher. Keep the returned session guard bound for the whole test so operations use the mounted filesystem. Run serially and avoid concurrent test processes because several tests reuse directory names. Keep the deleted-open-file and rename-overwrite cases enabled when changing inode paths, file handles, or attributes.

| Change or check | Command or action | Coverage and prerequisites |
| --- | --- | --- |
| Rust build | `cargo build --locked` | Compiles the library and binary. |
| Docker build and packaging | `make release` and `make release ARCH=arm64` | Builds Linux amd64 and arm64 binaries respectively. Each command writes `./toda`, so run them separately and retain each output if comparing them. Requires Docker execution support for the selected architecture. |
| Rust static analysis | `cargo clippy --locked --all-features --all-targets -- -D warnings` | Matches CI and rejects warnings. Needs the Clippy component. |
| Rust formatting | `cargo fmt --all -- --check` | Checks `rustfmt.toml` rules. Needs the pinned toolchain's rustfmt component. |
| RPC handling | `cargo test --locked --test jsonrpc_test` | Tests handlers without mounting FUSE. Still compiles the Linux-specific library. |
| Filesystem behavior | `cargo test --locked --verbose -- --test-threads=1` | Runs the full suite, including real FUSE mounts. Requires the POSIX test setup above. |
| Mount recovery | `cargo test --locked --test mount_injector_test -- --ignored --test-threads=1` | Tests mount rollback and empty or nonempty backing-directory cleanup in private mount namespaces. Requires Linux, `CAP_SYS_ADMIN`, `/dev/fuse`, `unshare`, and the FUSE setup above. |
| Injector behavior | Test the affected hook through a real configured injection. | The existing RPC and POSIX tests do not demonstrate that configured faults take effect. |
| CLI, mount, or process replacement | Run an injection and recovery cycle in the target PID and mount namespaces. | Check baseline I/O, the configured fault, rule updates or clearing, and restored I/O after SIGINT or SIGTERM. Exercise existing descriptors, working directories, or mappings affected by the change. |
| Documentation only | Check paths, commands, and JSON examples against the source. Run `git diff --check`. | Does not require a runtime build. Do not install a toolchain or change host mount settings just to check prose. |

Use `make example` only as a manual workload in an environment prepared for its mounts. It starts a privileged container and mounts host `/tmp`. It does not start toda or prove that injection works.

On macOS or another unsupported host, use a disposable Linux environment of the target architecture for build and runtime checks. If that environment is unavailable, report those checks as not run. User-mode CPU emulation does not validate native `ptrace` behavior. Use native execution or full-system emulation for injection and recovery checks, and report which runtime paths remain unverified. Do not report source inspection, compilation, or existing CI configuration as proof of a successful injection and recovery cycle.

## Keep documentation aligned

Keep README.md focused on building, using, and testing toda. Keep repository architecture and agent instructions here. Update README.md and `config-examples/` when CLI or configuration behavior changes.

Use the workflows as evidence of the configured checks, not proof that a local change passes them. CI builds, tests, and runs Clippy on native x86-64 and AArch64 runners. The release workflow builds each architecture on `v*` tags, then publishes both archives in one job. Make defaults to `ARCH=amd64` and accepts `ARCH=arm64`. The build image uses Debian 12 and the pinned Rust compiler. BuildKit shares Cargo registry and Git caches and separates target caches by compiler, base image, and architecture.

## Report completion

State what changed, which behavior or documentation improved, and which checks you actually ran. Name any required checks that could not run and the missing prerequisite. Keep implementation, test results, and release status distinct.
