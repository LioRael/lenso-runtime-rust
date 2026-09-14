use lenso_capability_http_endpoint as http;
use lenso_kernel::{InvocationContext, NativeRequestFuture, PrepareContext, RuntimeFailure};
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Debug, serde::Deserialize)]
struct Config {
    fail: bool,
}

#[lenso::plugin(lifecycle, configuration_schema = "config.schema.json")]
#[derive(Clone, Debug)]
struct Echo {
    #[config]
    config: Config,
    count: Rc<Cell<u32>>,
}
impl lenso::Lifecycle for Echo {
    async fn prepare(&self, _: PrepareContext) -> Result<(), RuntimeFailure> {
        if self.config.fail {
            return Err(RuntimeFailure::PluginFailure {
                detail: "intentional prepare rejection".into(),
            });
        }
        Ok(())
    }
}
#[lenso::provides(http::Endpoint)]
impl http::EndpointProvider for Echo {
    fn describe(
        &self,
        _: InvocationContext,
        _: http::DescribeRequest,
    ) -> NativeRequestFuture<http::EndpointDescribe> {
        Box::pin(async { Ok(Ok(http::DescribeResponse { routes: vec![] })) })
    }
    fn handle(
        &self,
        _: InvocationContext,
        request: http::HandleRequest,
    ) -> NativeRequestFuture<http::EndpointHandle> {
        self.count.set(self.count.get() + 1);
        let count = self.count.get();
        Box::pin(async move {
            if request.route_id == "async-pending" {
                std::future::pending::<()>().await;
            }
            assert!(
                request.route_id != "async-panic",
                "intentional async Plugin panic"
            );
            if request.route_id == "async-trap" {
                core::arch::wasm32::unreachable();
            }
            Ok(Ok(http::HandleResponse {
                status: 200,
                headers: vec![http::HandleResponseHeadersItem {
                    name: "x-invocations".into(),
                    value: count.to_string(),
                }],
                body: request.body,
            }))
        })
    }
}
pub fn link() {
    __lenso_link_echo();
}
