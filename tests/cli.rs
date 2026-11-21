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
    assert!(help.contains("--check-config"));

    let version = nnnyaz().arg("--version").output().unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("nnnyaz {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn check_config_reports_warnings_and_exit_status() {
    let dir = std::env::temp_dir().join(format!(
        "nnnyaz-check-config-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    std::fs::write(dir.join("config.toml"), "[mgr]\nshow_hidden = true\n").unwrap();
    let ok = nnnyaz()
        .arg("--check-config")
        .env("NNNYAZ_CONFIG_HOME", &dir)
        .output()
        .unwrap();
    assert!(ok.status.success());
    assert_eq!(String::from_utf8_lossy(&ok.stdout).trim(), "config: OK");

    std::fs::write(dir.join("config.toml"), "[mgr]\ntypo = 1\n").unwrap();
    let bad = nnnyaz()
        .arg("--check-config")
        .env("NNNYAZ_CONFIG_HOME", &dir)
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(stderr.contains("unknown key 'typo'"), "{stderr}");

    std::fs::remove_dir_all(dir).unwrap();
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
