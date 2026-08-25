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
