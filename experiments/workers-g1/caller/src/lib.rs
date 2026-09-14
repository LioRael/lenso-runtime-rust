use lenso_capability_http_endpoint as http;
#[lenso::plugin(consumer)]
#[derive(Debug)]
struct Caller {
    endpoint: lenso::Port<http::EndpointClient>,
}
pub fn link() {
    __lenso_link_caller();
}
