# toda

[![FOSSA Status](https://app.fossa.com/api/projects/git%2Bgithub.com%2Fchaos-mesh%2Ftoda.svg?type=shield)](https://app.fossa.com/projects/git%2Bgithub.com%2Fchaos-mesh%2Ftoda?ref=badge_shield)

toda injects filesystem faults through FUSE for Chaos Mesh. It can delay filesystem operations, return errors, override file attributes, and corrupt read or write data.

toda moves the target mount to a sibling path and mounts a FUSE filesystem at the original path. By default, it also uses `ptrace` to redirect existing file descriptors, working directories, and memory mappings through the new mount. Rules can change at runtime through JSON-RPC over standard input and standard output.

## Requirements

- Linux on x86-64 or AArch64. Process replacement requires native 64-bit target processes of the same architecture as toda.
- FUSE support, access to `/dev/fuse`, and FUSE mount tools such as `fusermount3`.
- Permission to move and mount filesystems and, unless `--mount-only` is set, trace the target processes.
- The target processes' PID and mount namespaces. Enter those namespaces before starting toda. toda does not enter namespaces itself.

Use an existing mount point as the target. An ordinary directory that is not a mount point cannot be moved with `MS_MOVE`. The target mount's propagation settings must permit the move.

Replacing existing memory mappings requires kernel support for `mremap` with `MREMAP_DONTUNMAP` on regular file mappings. toda probes this capability at runtime and refuses unsupported mappings before replacing them. `--mount-only` skips process replacement and does not require this capability.

Process replacement reopens regular-file and directory descriptors, including `O_PATH` descriptors. Other descriptor types, such as open FIFOs and devices, are rejected unless opened with `O_PATH`. Deleted file paths cannot be reopened. File mappings must be readable, page-aligned regular-file mappings with ordinary protection and sharing flags. Special, locked, huge-page, userfault, and other unrepresented mapping attributes are rejected. Private page contents, current protections, and shared mappings' writable capability are preserved for supported mappings. Reopened descriptors retain their offset, status flags, and `FD_CLOEXEC`; shared open-file-description relationships are not preserved.

## Build

The repository pins stable Rust to `1.99.1` in [rust-toolchain](rust-toolchain). Keep this toolchain when building the current source.

On a Linux x86-64 or AArch64 system with Rust installed through rustup, install the native dependencies and build:

```sh
sudo apt-get update
sudo apt-get install -y build-essential fuse3
cargo build --locked --release
```

The binary is `target/release/toda`. [.cargo/config.toml](.cargo/config.toml) sets `-C relro-level=full` so shared-library symbols are resolved at startup, before toda changes mounts.

To build with Docker and copy the binary to `./toda`, run:

```sh
make release
```

This target builds the `chaos-mesh/toda` image using the [Dockerfile](Dockerfile), then copies `/toda` out of the image. `make image` builds the image without copying the binary. Both targets default to `ARCH=amd64`. To build an AArch64 binary, run:

```sh
make release ARCH=arm64
```

`ARCH` accepts `amd64` or `arm64` and also selects the platform for `make example`. Docker must support running Linux containers for the selected architecture. The image uses Debian 12 and Rust 1.99.1. BuildKit shares Cargo dependency caches and keeps a separate build cache for each architecture.

The release workflow publishes `toda-linux-amd64.tar.gz` and `toda-linux-arm64.tar.gz`. Each archive contains a binary named `toda`.

The Docker-built binary requires glibc 2.34 or newer. To run on a system with an older glibc, build natively on that system with the pinned toolchain.

## Run and configure injection

In the target PID and mount namespaces, start toda against the target mount. This example uses `/var/run/test`, the mount path in the bundled example:

```sh
sudo ./target/release/toda --path /var/run/test
```

If you built with `make release`, use `./toda` instead of `./target/release/toda`.

toda starts with no injection rules. It reads one JSON-RPC request per line from standard input, writes responses to standard output, and writes logs to standard error. Keep standard input open to send further requests. Closing standard input does not trigger mount recovery.

Enter this request on one line to check the initial mount result:

```json
{"jsonrpc":"2.0","method":"get_status","params":[""],"id":1}
```

The request requires one string parameter, even though the handler ignores its value. After a successful mount, the response is:

```json
{"jsonrpc":"2.0","result":"ok","id":1}
```

To add a 100 ms delay to reads under the target mount, send:

```json
{"jsonrpc":"2.0","method":"update","params":[[{"type":"latency","path":"/var/run/test/**/*","methods":["READ"],"percent":100,"latency":"100ms"}]],"id":2}
```

`update` takes one positional argument, an array of rules. Each successful update replaces the entire rule set. A successful update returns `"ok"` in `result`. Injector construction failures can also be returned as strings in `result`, so check the value as well as the JSON-RPC `error` field.

To clear all rules while keeping FUSE mounted, send:

```json
{"jsonrpc":"2.0","method":"update","params":[[]],"id":3}
```

### Rule fields

Rules use a `type` field and camelCase field names. `percent` is the chance of injection for a matching operation. Use values from 0 to 100. Path filters match paths at the original mount location, not the internal `__chaosfs__<name>__` path.

| Type | Effect | Additional fields |
| --- | --- | --- |
| `latency` | Delays matching operations. | `latency`, a duration such as `100ms` or `10s`. |
| `fault` | Returns a filesystem error. | `faults`, an array of objects with numeric `errno` and relative `weight` values. |
| `attrOverride` | Changes returned file attributes. | `path` and the attributes to override, such as `size`, `perm`, `uid`, or `gid`. |
| `mistake` | Changes read or write data. | `mistake`, an object with `filling` set to `zero` or `random`, `maxLength`, and `maxOccurrences`. |

`latency`, `fault`, and `mistake` accept optional `path` glob and `methods` filters. Omitting `path` matches all paths. Omitting `methods`, or supplying an empty array, selects all methods. Method names are case-insensitive. Use names such as `READ`, `WRITE`, and `OPEN`. The complete set is defined by `Method` in [src/injector/filter.rs](src/injector/filter.rs).

`attrOverride` requires `path` and does not have a `methods` filter. See [src/injector/injector_config.rs](src/injector/injector_config.rs) for the complete configuration types.

The [latency example](config-examples/latency-example.json) and [data corruption example](config-examples/mistake-example.json) contain complete `update` requests. Change their paths to match your target and send each request as one line. [no-inject.json](config-examples/no-inject.json) contains only the empty rule array, `[]`. Wrap that array in an `update` request as shown above.

### Options and recovery

| Option | Behavior |
| --- | --- |
| `--path <path>` | Required target mount path. Use an absolute path. |
| `--mount-only` | Skips replacement of existing file descriptors, working directories, and memory mappings. Existing references can continue to bypass FUSE. |
| `-v <filter>`, `--verbose <filter>` | Sets the tracing filter. The default is `trace`. A valid `RUST_LOG` value takes precedence. |

Send `SIGINT` with Ctrl+C, or send `SIGTERM`, to start recovery. toda interrupts pending delays and attempts to restore process references, unmount FUSE, and move the backing mount to its original location. `--mount-only` also skips process replacement during recovery. `SIGKILL` bypasses this recovery path.

If setup or recovery fails, toda retains the mount and stopped-process state needed to finish safely. Send `SIGINT` or `SIGTERM` to roll back failed setup or retry incomplete recovery. Resolve the reported underlying error before retrying. Targets are released only after their references and the mount are ready. This does not provide recovery after toda itself is forcibly terminated.

After successful recovery, toda attempts to remove the empty backing directory. A directory that is already absent needs no cleanup. Other cleanup failures produce a warning without failing mount recovery or deleting remaining contents.

If FUSE mounting fails after toda moves the target mount, toda attempts to move it back before returning the setup error. If restoration also fails, the error reports both failures and the mount paths.

## Develop and test

On Linux with the build dependencies installed, run the JSON-RPC handler tests without mounting FUSE:

```sh
cargo test --locked --test jsonrpc_test
```

The POSIX tests mount FUSE with `allow_other` and delete and recreate directories under `/tmp/test_mnt` and `/tmp/test_mnt_backend`. Run them in a disposable Linux environment. For a non-root test user, enable `user_allow_other` in `/etc/fuse.conf` and ensure that the user can access `/dev/fuse`.

Mount the FUSE control filesystem if it is not already mounted:

```sh
mountpoint -q /sys/fs/fuse/connections || sudo mount -t fusectl fusectl /sys/fs/fuse/connections
```

Tests use its [pending-request counter](https://www.kernel.org/doc/html/latest/filesystems/fuse/fuse.html#control-filesystem) to finish replies before unmounting.

Run the full suite serially, as CI does on native x86-64 and AArch64 runners:

```sh
cargo test --locked --verbose -- --test-threads=1
```

The POSIX tests cover filesystem operations, including access through open files after deletion or replacement by rename. The suite does not cover the full CLI injection and recovery flow.

The mount lifecycle tests are ignored by default. Run them in a disposable Linux environment with `CAP_SYS_ADMIN`, `/dev/fuse`, and the `unshare` command:

```sh
cargo test --locked --test mount_injector_test -- --ignored --test-threads=1
```

The tests use private mount namespaces to check FUSE setup rollback and recovery with empty or nonempty backing directories. They verify that the original mount and file reads and writes recover. CI runs these tests in a separate privileged step.

CI also builds and runs Clippy on both architectures. These checks do not prove process replacement works. To verify replacement, run an injection and recovery cycle in the target PID and mount namespaces on each architecture. Check existing descriptors, working directories, and mappings before injection, with an active rule, and after recovery. User-mode CPU emulation does not provide native `ptrace` behavior for this check.

`make example` builds and starts a privileged Docker container that repeatedly reads and writes files through a volume at `/var/run/test`. It is a workload for manual testing. It does not start toda or configure injection automatically.

See [AGENTS.md](AGENTS.md) for the module map, change guidelines, and verification requirements.

## Limitations

- Kernel caching affects injection. A successful read can remain cached, so subsequent reads might not reach FUSE. Attribute caching can preserve a result from an earlier lookup. Setting `percent` to 100 avoids probabilistic selection differences when an operation reaches FUSE, but does not force cache misses.
- An optional mode to bypass the content cache is deferred. See [TODO.md](TODO.md) for the proposal and its limits.
- Paths near 4096 bytes are not supported.

## License

[Apache License 2.0](LICENSE).
