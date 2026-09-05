# Lenso portable Plugin Runtime SDK

This crate is the domain-neutral lowering layer used by product SDKs. It turns
one generated JSON Capability dispatcher into a Wasm Component or a trusted
Process implementation. It intentionally contains no Agent Tool, Ingress,
authentication, or other product semantics.

Plugin projects normally depend on it through the `lenso` facade name:

```toml
[dependencies]
lenso = { package = "lenso-plugin-sdk", version = "0.2" }
```

## Migrating from 0.1

Version 0.1 incorrectly exposed Agent-specific `AgentTool` and
`export_agent_tool!` interfaces from the Runtime SDK. Those interfaces were
removed rather than retained as compatibility aliases. Agent Tool authors use
the product SDK and the same annotations as a linked native Plugin:

```rust,ignore
#[lenso::plugin]
#[derive(Clone, Copy, Debug, Default)]
struct TextTools {}

#[lenso_agent_tool_sdk::tool_provider]
impl TextTools {
    #[tool(
        name = "uppercase",
        description = "Convert text to uppercase.",
        execution = "parallel_safe"
    )]
    fn uppercase(arguments: Arguments) -> Result<ExecuteResponse, ExecuteError> {
        // Business implementation.
    }
}
```

Cargo metadata selects `wasm`, `process`, or both outputs. Plugin business code
does not implement transport framing or a target-specific Agent Tool trait.

Native Process output uses the complete-object `lenso.process-stdio@2` profile.
Generated glue validates Host initialization, constructs one Plugin object,
reports invocation settlement separately from its result, and attempts stop once
after the Host drains admitted work. `--lenso-describe` prints the generated
descriptor for build tooling without starting the runtime session.
