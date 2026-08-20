use lenso_app_plan::ResolvedAppPlan;
use lenso_kernel::Kernel;
use lenso_runner::TokioDriver;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let local = tokio::task::LocalSet::new();
    let outcome = local
        .run_until(async { Kernel::boot(ResolvedAppPlan::empty(), TokioDriver::new()).await })
        .await;

    match outcome {
        Ok(outcome) => println!("Kernel terminal outcome: {outcome:?}"),
        Err(error) => {
            eprintln!("Kernel rejected the Resolved App Plan: {error:?}");
            std::process::exit(2);
        }
    }
}
