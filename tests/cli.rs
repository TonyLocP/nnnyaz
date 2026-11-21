//! CLI smoke tests that run without a terminal.

use std::process::Command;

fn nnnyaz() -> Command {
    Command::new(env!("CARGO_BIN_EXE_nnnyaz"))
}

#[test]
fn help_and_version_exit_before_terminal_setup() {
    let help = nnnyaz().arg("--help").output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("Usage: nnnyaz [OPTIONS] [PATH]"));
    assert!(help.contains("--chooser-file"));

    let version = nnnyaz().arg("--version").output().unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("nnnyaz {}", env!("CARGO_PKG_VERSION"))
    );
}
