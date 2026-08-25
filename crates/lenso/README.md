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
impl greeting::GreetingProvider for Greeting {
    // Generated Capability Provider methods remain the current behavior seam.
}
```

The generated Provider trait is still visible for now. Lowering ordinary
domain methods into request, stream, and event Provider implementations is a
separate authoring step because each interaction kind has different ownership
and failure semantics.
