# Instructions for AI coding agents

These instructions apply to the whole repository. Use them to choose which files to read, where to make a change, and how to verify it.

## Start a task

1. Read the user request and inspect `git status --short` and the existing diff. Preserve changes that are outside your task.
2. Read [README.md](README.md) for the public CLI and configuration workflow. Use the task map below to find the relevant implementation and tests.
3. Check the host platform and installed Rust toolchain before running Cargo or runtime tests. This project targets Linux x86-64 and pins `1.99.1`.
4. Make the smallest change that satisfies the request. Choose verification from the table below. If a required environment is unavailable, complete the work you can verify and report the missing checks.

## Project and execution model

toda is a Rust 2018 filesystem fault injector for Chaos Mesh. Its runtime targets Linux x86-64 and combines FUSE forwarding with `ptrace` replacement of existing process references. It builds a library and a binary from the same module tree.

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
| Process replacement | `src/replacer/` | File descriptor, working directory, and memory mapping replacement. |
| Traced execution | `src/ptrace/mod.rs` | Thread attachment, register and memory access, injected syscalls, and detach guards. |
| Shared helpers | `src/utils.rs`, `src/stop.rs` | Backing path construction and thread coordination. |
| Verification | `tests/jsonrpc_test.rs`, `tests/posix_test.rs` | RPC handler tests and FUSE filesystem integration tests. |
| Build and release | `Cargo.toml`, `Cargo.lock`, `rust-toolchain`, `.cargo/config.toml`, `Dockerfile`, `Makefile`, `.github/workflows/` | Dependencies, toolchain, linker flags, image builds, CI, and release packaging. |
| Manual examples | `config-examples/`, `example/` | RPC configuration examples and a Go filesystem workload. |

## Follow these editing rules

- Keep `1.99.1` and the committed `Cargo.lock` unless the task includes a toolchain or dependency change. The project uses stable Rust. Do not introduce nightly feature gates to work around a dependency mismatch.
- Preserve `-C relro-level=full` in `.cargo/config.toml` and the Docker build. It resolves shared-library symbols before mount changes.
- Follow the stable rustfmt settings in `rustfmt.toml`. Avoid unrelated formatting churn.
- `src/main.rs` declares its own modules rather than importing the library crate. When adding or removing a shared top-level module, update both crate roots as needed.
- Keep FUSE behavior in `hookfs`, rule behavior in `injector`, process-reference replacement in `replacer`, and low-level traced execution in `ptrace`.
- For a new injector, update `InjectorConfig`, `MultiInjector::build`, the relevant `Injector` hooks, and configuration examples together. Check the FUSE call sites that invoke those hooks.
- Preserve the public configuration shape unless the requested change includes a protocol change. Rules use `type` tags, camelCase fields, flattened filters, and a complete rule array as the single argument to `update`.
- Keep stdout available for JSON-RPC responses. Send diagnostics to stderr through the existing tracing setup.

## Check the contracts affected by your change

- For mount or process replacement changes, trace both `inject` and `resume` in `src/main.rs`. Recovery is explicit. Dropping `MountInjectionGuard` does not restore the mount. Check failure paths, pending delays, mount restoration, and traced-thread detachment.
- For path changes, preserve the distinction between the backing path and the visible path. `HookFs` translates backing paths to the original visible paths before filtering. Check mount management and `src/utils.rs` for the same backing path convention. Open-file attributes must come from the backing descriptor after unlink or rename-overwrite; keep the saved path for injection filters, not file identity.
- For injector changes, preserve configuration order and first-error behavior in fallible hooks unless the task changes those contracts. `update` builds a complete replacement list before taking the write lock. Inspect lock scope and the affected FUSE call sites.
- For configuration validation changes, account for current permissive behavior. Unknown method names are discarded, invalid glob patterns become unrestricted path filters, and `percent` is not range-checked. Changing these behaviors affects existing configurations.
- For RPC changes, check both `result` and JSON-RPC error behavior. `get_status` records initial setup rather than continuous mount health. Some failures are strings in `result`; malformed parameters produce JSON-RPC errors.
- For architecture changes, inspect the x86-64 registers and generated instructions in `ptrace` and `replacer`. `--mount-only` skips replacement at runtime but does not make the crate portable.

## Select and run verification

Run Cargo commands from the repository root on Linux x86-64 with the pinned toolchain and the dependencies listed in README.md. Select checks for the changed behavior rather than treating every row as a command sequence.

Before running POSIX tests, prepare a disposable environment. These tests recursively delete and recreate directories under `/tmp/test_mnt` and `/tmp/test_mnt_backend`. They mount FUSE with `allow_other`, require access to `/dev/fuse`, and need `user_allow_other` in `/etc/fuse.conf` for non-root users. Keep the returned FUSE session bound for the whole test so operations use the mounted filesystem. Run serially and avoid concurrent test processes because several tests reuse directory names. Keep the deleted-open-file and rename-overwrite cases enabled when changing inode paths, file handles, or attributes.

| Change or check | Command or action | Coverage and prerequisites |
| --- | --- | --- |
| Rust build | `cargo build --locked` | Compiles the library and binary. |
| Rust static analysis | `cargo clippy --locked --all-features --all-targets` | Matches the CI feature scope. Needs the Clippy component. |
| Rust formatting | `cargo fmt --all -- --check` | Checks `rustfmt.toml` rules. Needs the pinned toolchain's rustfmt component. |
| RPC handling | `cargo test --locked --test jsonrpc_test` | Tests handlers without mounting FUSE. Still compiles the Linux-specific library. |
| Filesystem behavior | `cargo test --locked --verbose -- --test-threads=1` | Runs the full suite, including real FUSE mounts. Requires the POSIX test setup above. |
| Injector behavior | Test the affected hook through a real configured injection. | The existing RPC and POSIX tests do not demonstrate that configured faults take effect. |
| CLI, mount, or process replacement | Run an injection and recovery cycle in the target PID and mount namespaces. | Check baseline I/O, the configured fault, rule updates or clearing, and restored I/O after SIGINT or SIGTERM. Exercise existing descriptors, working directories, or mappings affected by the change. |
| Documentation only | Check paths, commands, and JSON examples against the source. Run `git diff --check`. | Does not require a runtime build. Do not install a toolchain or change host mount settings just to check prose. |

Use `make example` only as a manual workload in an environment prepared for its mounts. It starts a privileged container and mounts host `/tmp`. It does not start toda or prove that injection works.

On macOS or another unsupported host, use a disposable Linux x86-64 environment for build and runtime checks. If that environment is unavailable, report those checks as not run. When using emulation, report which runtime paths it supports and which remain unverified. Do not report source inspection, compilation, or existing CI configuration as proof of a successful injection and recovery cycle.

## Keep documentation aligned

Keep README.md focused on building, using, and testing toda. Keep repository architecture and agent instructions here. Update README.md and `config-examples/` when CLI or configuration behavior changes.

Use the workflows as evidence of the configured checks, not proof that a local change passes them. The release workflow runs `make release` on `v*` tags and publishes a Linux amd64 archive. The Make targets explicitly select `linux/amd64`. The build image uses Debian 12 and the pinned Rust compiler.

## Report completion

State what changed, which behavior or documentation improved, and which checks you actually ran. Name any required checks that could not run and the missing prerequisite. Keep implementation, test results, and release status distinct.
