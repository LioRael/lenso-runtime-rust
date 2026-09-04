# `lenso`

The stable Rust authoring facade for Lenso vNext Plugins.

Plugin packages depend on `lenso` plus the generated Capability crates they
provide or require. The facade intentionally keeps Native Adapter factories,
Kernel lifecycle implementations, endpoint construction, inventory
registration, and generated-code dependencies out of the ordinary authoring
Interface.

```rust,ignore
use lenso::prelude::*;

#[derive(Clone, Debug, serde::Deserialize, PluginConfig)]
struct GreetingConfig {
    #[lenso(default = "Hello")]
    prefix: String,
}

#[plugin]
#[derive(Clone, Debug)]
struct Greeting {
    #[config]
    config: GreetingConfig,
    profile: Port<profile::ProfileClient>,
}

#[provides(greeting::Greeting)]
impl Greeting {
    async fn greet(
        &self,
        _ctx: Ctx,
        request: greeting::GreetRequest,
    ) -> Result<greeting::GreetResponse, greeting::GreetError> {
        let _ = (&self.profile, request);
        todo!("ordinary async domain behavior")
    }
}
```

`PluginConfig` embeds `#[lenso(default = <JSON literal>)]` field values as
locked `configuration_defaults` in the generated Plugin Descriptor. App
Definitions may omit those values; Plan resolution still materializes and
validates one complete configuration before boot. A Plugin using an explicit
complex Schema can instead select a package-local defaults object with

```rust,ignore
#[plugin(
    configuration_schema = "config.schema.json",
    configuration_defaults = "config.defaults.json"
)]
```

Stateless Plugins omit configuration entirely; the facade derives a closed
empty-object Schema and rejects non-empty configuration before readiness:

```rust,ignore
#[plugin]
#[derive(Clone, Debug, Default)]
struct Health {}

#[provides(health::Health)]
impl Health {
    async fn check(
        &self,
        _ctx: Ctx,
        request: health::CheckRequest,
    ) -> Result<health::CheckResponse, health::CheckError> {
        todo!()
    }
}
```

A cohesive Plugin may provide several Capabilities from the same state and
lifecycle. List them once and keep their generated domain methods in one
inherent implementation:

```rust,ignore
#[provides(agent::Model, agent::ModelMetadata, health::Health)]
impl OpenAiModel {
    async fn complete(
        &self,
        ctx: Ctx,
        request: agent::CompleteRequest,
    ) -> Result<agent::CompleteResponse, agent::CompleteError> {
        todo!()
    }

    async fn describe(
        &self,
        ctx: Ctx,
        request: agent::DescribeRequest,
    ) -> Result<agent::DescribeResponse, agent::DescribeError> {
        todo!()
    }

    async fn check(
        &self,
        ctx: Ctx,
        request: health::CheckRequest,
    ) -> Result<health::CheckResponse, health::CheckError> {
        todo!()
    }
}
```

The annotation order is preserved in the generated Descriptor. All Request,
Stream, and Event endpoints are aggregated into one factory and one Plugin
lifecycle. Repeating a Capability is rejected. Explicit generated Provider
trait implementations remain a single-Capability compatibility escape hatch;
multi-Capability Plugins use the inherent implementation above.

Generated lowering owns the Provider trait implementation, future boxing,
endpoint construction, and native registration. A method that only has
Capability-defined rejection returns an ordinary domain `Result`. A method
that must deliberately preserve infrastructure failure returns
`PluginResult<T, DomainError>` and constructs `PluginError::runtime(error)`.

Stream providers return `ProviderStream<C>`. `ProviderStream::channel(&ctx,
capacity)` also returns a `ProviderStreamChannel<C>` for a generation-managed
task. That task sends typed `C::Message` values, receives typed `StreamInput<C>`
values, half-closes either direction, and emits exactly one success, Domain
Error, or Runtime Failure terminal outcome. Bounded backpressure, cancellation,
and native type erasure stay inside the facade.

Event subscribers use the same inherent async method shape without exposing an
endpoint or boxed future:

```rust,ignore
#[provides(notifications::Notifications)]
impl Notifications {
    async fn notify(
        &self,
        ctx: Ctx,
        event: notifications::NotifyRequest,
    ) -> PluginEventResult {
        self.handle(ctx, event).await
    }
}
```

Publishing remains volatile fan-out: every explicit subscriber binding has an
independent bounded admission result. The handler runs after native admission,
so it has no publisher-visible Domain result. Returning `PluginEventResult`
preserves a handler Runtime Failure for diagnostics and Plugin supervision;
a simple infallible async handler may return `()`.

Plugins with resources or managed work opt into convention-based lifecycle
methods and override only the phases they own:

```rust,ignore
#[plugin(lifecycle)]
#[derive(Clone, Debug)]
struct Worker {
    #[config]
    config: WorkerConfig,
}

impl Lifecycle for Worker {
    async fn prepare(&self, context: PrepareContext) -> Result<(), RuntimeFailure> {
        todo!("reserve reversible resources")
    }

    async fn activate(&self, context: ActivateContext) -> Result<(), RuntimeFailure> {
        todo!("start generation-owned work")
    }

    async fn deactivate(&self, context: DeactivateContext) -> Result<(), RuntimeFailure> {
        todo!("release owned resources")
    }
}
```

The existing function-path lifecycle attributes remain available for
compatibility.

Plugins that only need generation-owned background work can declare a managed task field without
storing an optional Kernel scope or writing an activation hook:

```rust,ignore
#[plugin]
#[derive(Clone, Debug)]
struct Worker {
    #[tasks]
    tasks: ManagedTasks,
}

impl Worker {
    fn start(&self) -> Result<(), ManagedTasksError> {
        self.tasks.spawn_local(async move {
            // Work is cancelled and joined with this Plugin generation.
        })?;
        Ok(())
    }
}
```

The field becomes active immediately before an optional `Lifecycle::activate` hook runs. Spawning
before activation or during deactivation fails explicitly with `ManagedTasksError::Inactive`.
Long-running work can obtain `tasks.cancellation()` and stop cooperatively with its generation.

## Restartable Host stop

With the `host` feature enabled, an application can stop accepting new routes,
wait for outstanding route leases, and clean up its Generations while preserving
durable restart intent:

```rust,ignore
let suspended = host.drain_and_suspend(Duration::from_secs(10)).await?;
assert!(suspended.host_suspended);
```

The first request establishes one monotonic deadline for waiting and cleanup.
Calls through cloned Controller clients join the same operation and receive the
same result; dropping a waiter does not cancel stopping. The stop signal has its
own bounded channel, ahead of queued inspection and mutation commands. An operation
already executing in the Controller must return before that signal is handled;
its elapsed time still consumes the original deadline.

Every product operation must retain its route lease until its work completes.
Suspension preserves the exact durable active Generation for `HostBuilder::recover`;
`shutdown()` remains permanent retirement, and `suspend()` remains the immediate
operation for a Host already known to have no outstanding leases.

Cleanup failure, deadline expiry, or a failed durable write does not report clean
suspension. The Controller exits instead of resuming a partially stopped graph.
Deadline expiry is not process-termination proof: an external native execution
owner must settle runtime and child processes before recovery. This API does not
provide that OS owner or a TypeScript launcher. Blocking runtime code or synchronous
Store I/O cannot be preempted by Tokio timers; the outer process owner must enforce
the physical termination budget.
