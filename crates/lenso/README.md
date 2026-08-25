# `lenso`

The stable Rust authoring facade for Lenso vNext Modules.

Module packages depend on `lenso` plus the generated Capability crates they
provide or require. The facade intentionally keeps Native Adapter factories,
Kernel lifecycle implementations, endpoint construction, inventory
registration, and generated-code dependencies out of the ordinary authoring
Interface.

```rust,ignore
use lenso::prelude::*;

#[derive(Clone, Debug, serde::Deserialize, ModuleConfig)]
struct GreetingConfig {
    prefix: String,
}

#[module]
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

Stateless Modules omit configuration entirely; the facade derives a closed
empty-object Schema and rejects non-empty configuration before readiness:

```rust,ignore
#[module]
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

A cohesive Module may provide several Capabilities from the same state and
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
Stream, and Event endpoints are aggregated into one factory and one Module
lifecycle. Repeating a Capability is rejected. Explicit generated Provider
trait implementations remain a single-Capability compatibility escape hatch;
multi-Capability Modules use the inherent implementation above.

Generated lowering owns the Provider trait implementation, future boxing,
endpoint construction, and native registration. A method that only has
Capability-defined rejection returns an ordinary domain `Result`. A method
that must deliberately preserve infrastructure failure returns
`ModuleResult<T, DomainError>` and constructs `ModuleError::runtime(error)`.

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
    ) -> ModuleEventResult {
        self.handle(ctx, event).await
    }
}
```

Publishing remains volatile fan-out: every explicit subscriber binding has an
independent bounded admission result. The handler runs after native admission,
so it has no publisher-visible Domain result. Returning `ModuleEventResult`
preserves a handler Runtime Failure for diagnostics and Module supervision;
a simple infallible async handler may return `()`.

Modules with resources or managed work opt into convention-based lifecycle
methods and override only the phases they own:

```rust,ignore
#[module(lifecycle)]
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
