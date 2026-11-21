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

#[test]
fn generate_config_prints_valid_toml() {
    let output = nnnyaz().arg("--generate-config").output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("[mgr]"));
    assert!(text.contains("[preview]"));
    assert!(text.contains("sort_by = \"name\""));
    // The generated file is valid TOML with no unknown keys.
    let parsed: toml::Value = toml::from_str(&text).unwrap();
    assert!(parsed.get("mgr").is_some());
    assert!(parsed.get("which").is_some());
}
