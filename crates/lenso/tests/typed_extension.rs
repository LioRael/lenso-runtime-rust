use lenso::{CtxExt, TypedExtension};
use lenso_kernel::{CancellationToken, InvocationContext};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RunScope {
    root: String,
}

impl TypedExtension for RunScope {
    const KEY: &'static str = "example.run-scope@1";
}

#[test]
fn typed_extensions_round_trip_without_moving_types_into_kernel() {
    let context = InvocationContext::new(1, None, CancellationToken::new())
        .with_typed_extension(&RunScope {
            root: "/workspace".to_owned(),
        })
        .expect("typed extension should encode and attach");

    assert_eq!(
        context
            .typed_extension::<RunScope>()
            .expect("typed extension should decode"),
        Some(RunScope {
            root: "/workspace".to_owned(),
        })
    );
}
