//! rozed's command-line flags that return without opening a window.
//!
//! Each child runs with the guard that stops a rozed started by another rozed, so a broken flag
//! fails the test instead of opening the app.

use std::process::{Command, Output};

fn rozed(arg: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rozed"))
        .arg(arg)
        .env("ROZED_PARENT_PID", "test")
        .env("ROZED_CLI_TEST", "printed")
        .output()
        .expect("running rozed")
}

#[test]
fn printenv_prints_the_environment_as_json() {
    // Zed's shell-environment capture runs `rozed --printenv` inside the user's shell and reads
    // this JSON back.
    let output = rozed("--printenv");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.trim_start().starts_with('{'), "{stdout}");
    assert!(
        stdout.contains(r#""ROZED_CLI_TEST": "printed""#),
        "{stdout}"
    );
}

#[test]
fn version_prints_the_version() {
    let output = rozed("--version");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("rozed {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn rozed_does_not_start_inside_rozed() {
    let output = rozed("https://github.com/sharkdp/fd/pull/2212");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("not starting"), "{stderr}");
}

#[test]
fn an_argument_that_isnt_a_pull_request_link_is_refused_with_usage() {
    for arg in ["https://github.com/sharkdp/fd/issues/1", "--no-such-flag"] {
        let output = rozed(arg);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(&format!("can't open `{arg}`")), "{stderr}");
        assert!(stderr.contains("usage: rozed"), "{stderr}");
    }
}
