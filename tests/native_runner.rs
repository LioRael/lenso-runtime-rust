use std::process::Command;

#[test]
fn native_runner_reports_the_kernel_terminal_outcome() {
    let output = Command::new(env!("CARGO_BIN_EXE_lenso-runner"))
        .output()
        .expect("native Runner should start");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout should be UTF-8"),
        "Kernel terminal outcome: Completed\n"
    );
}
