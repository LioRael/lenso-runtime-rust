# Private native process ownership

The optional `process-owner` feature builds `lenso-process-owner`, an outer
execution owner for the future TypeScript Host distribution. It is not a new
public application command, a Kernel, or an application-ready runtime.

The helper starts exactly one supplied executable in a separate process group.
An independent native loop watches both launcher pipe loss and child exit, so a
dead JS launcher or dead Rust runtime cannot leave ordinary managed descendants
running indefinitely. macOS uses child observation without reaping; Linux also
enables subreaping for orphaned descendants. `waitid(NOWAIT)` retains the direct
child identity until final group signalling. No signal is sent after that child
is reaped, avoiding a signal to a reused process-group ID.

## Ownership and failure

The distribution must supply one shared, stable, absolute ownership registry
outside all replaceable application roots. The helper locks both the canonical
root path and its filesystem identity. Different Host identities, symlink aliases,
renamed roots, and replacement directories cannot bypass an outstanding owner.
Registry records are never unlinked as routine cleanup.

Before spawn, both records durably say `unconfirmed`. After the direct child is
reaped and the process group is confirmed absent, they are marked `settled`.
Timeout, observation failure, or a killed ownership helper leaves uncertainty
that rejects a later start even after the OS releases its advisory locks.
There is deliberately no automatic reset based on a PID, elapsed time, or missing
pipe. Operator recovery from an uncertain helper death is not implemented here.

The executable inherits no launcher control pipe or ownership file descriptors.
Without application control its standard streams are null. With application
control enabled, stdin/stdout carry only nested bounded control frames; the owner
relays at most eight queued requests and sixteen queued results. A stop is kept
separately and takes priority over inspection. Processes that detach from the
managed group are outside this trusted-process profile.
This is not filesystem/network confinement or supervision of hostile code.

## Private transport

The helper takes the expected distribution identity as its sole argument and
accepts a bounded start object on stdin. Frames are a four-byte unsigned big-endian
length followed by UTF-8 JSON; the payload limit is 256 KiB. Start contains protocol
version 1, the matching distribution identity, request ID 1, absolute executable,
root and registry paths, argument strings, and finite stop/confirmation budgets.
Identity equality is a protocol check; immutable runtime artifact verification
still belongs to distribution assembly.

After start, only `stop` requests are accepted, with strictly increasing request
IDs. Unknown operations, incompatible versions, duplicate fields, oversized or
truncated frames start cleanup. Repeated stops do not extend the first deadline.
Pipe loss starts the same cleanup. Stop first signals the group with TERM; bounded
outer cleanup escalates to KILL and checks group absence. The final confirmation
budget is independent of the cooperative stop budget.

The two output messages are `owned` and `terminal`. `owned` confirms OS process
ownership, never Plugin readiness. `terminal` distinguishes confirmed/unconfirmed
termination and includes its cause. This enforcement primitive always reports
`forced: true`; callers must not reinterpret it as a clean business suspension.
A blocked output writer cannot block cleanup. Missing terminal output is not
termination proof. No Plugin configuration, business calls, logs, or arbitrary
inspection payloads cross this ownership channel.

The companion private TypeScript transport lives in `lenso-cli/src/host-owner.ts`.
It uses the same frame bounds, validates version/identity/envelopes, joins repeated
stops, and resolves a terminal ownership outcome. It never kills the native owner
when its own waiting budget expires. It is not exported as the public Host SDK.

The framework Host now supplies that private application server. It invokes exact
runtime assembly only after the start handshake, verifies the expected active
Generation before Ready, returns revision-bound pages of Instance/package/
implementation/Artifact identities, and calls durable drain-and-suspend for stop.
The TS side joins repeated stop calls and combines the suspension receipt with
the native termination receipt. Configuration and business payloads remain absent.

The CLI prepares a digest-locked directory and generated `host.js` around this
owner. `lenso-host-distribution` now assembles verified Plan and Artifact
authority, while the product `lenso-host-runtime` in the Bun distribution
repository installs its Adapter catalog and recovery path. Released runtime
assets are still required before the source implementation is distributable.

## Verification

```sh
cargo test --locked -p lenso-runner --features process-owner --test process_owner
cargo clippy --locked --workspace --all-targets --features lenso/host,lenso-runner/process-owner -- -D warnings
cargo test --locked --workspace --features lenso/host,lenso-runner/process-owner
```

Real process cases cover stop, control EOF, launcher SIGKILL, runtime SIGKILL,
ignored TERM, descendants, malformed frames, duplicate ownership, symlink/rename/
replacement aliases, helper death uncertainty, and restart after confirmed cleanup.
CI enables this feature on Linux and adds a macOS process-owner job. Local evidence
is macOS; adding a Linux job does not count as having run Linux verification.
