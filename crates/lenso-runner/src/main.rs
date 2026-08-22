use lenso_app_plan::ResolvedAppPlan;
use lenso_kernel::ExecutionAdapterCatalog;
use lenso_runner::{TokioDriver, run};
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let local = tokio::task::LocalSet::new();
    let driver = TokioDriver::new();
    driver.request_shutdown();
    let outcome = local
        .run_until(run(
            ResolvedAppPlan::empty(),
            driver,
            ExecutionAdapterCatalog::new(),
            Duration::from_secs(1),
        ))
        .await;

    match outcome {
        Ok(outcome) => println!("Kernel terminal outcome: {outcome:?}"),
        Err(error) => {
            eprintln!("Kernel rejected the Resolved App Plan: {error:?}");
            std::process::exit(2);
        }
    }
}
