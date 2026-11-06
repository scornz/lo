//! rozed's command-line flags that return without opening a window.
//!
//! Each child runs with the guard that stops a rozed started by another rozed, so a broken flag
//! fails the test instead of opening the app.

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixListener;
use std::process::{Command, Output, Stdio};

fn rozed_command(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rozed"));
    command
        .args(args)
        .env("ROZED_PARENT_PID", "test")
        .env("ROZED_CLI_TEST", "printed");
    command
}

fn rozed(args: &[&str]) -> Output {
    rozed_command(args).output().expect("running rozed")
}

#[test]
fn printenv_prints_the_environment_as_json() {
    // Zed's shell-environment capture runs `rozed --printenv` inside the user's shell and reads
    // this JSON back.
    let output = rozed(&["--printenv"]);
    assert!(output.status.success(), "{output:?}");
    let env: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&output.stdout).expect("--printenv printed a JSON object");
    assert_eq!(env["ROZED_CLI_TEST"], "printed");
}

#[test]
fn version_prints_the_version() {
    let output = rozed(&["--version"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("rozed {}\n", env!("CARGO_PKG_VERSION"))
    );
}

/// Plays the running rozed's side of askpass: reads one prompt from the socket, which ends with a
/// NUL byte, and answers it.
fn answer_one_prompt(
    listener: UnixListener,
    answer: &'static str,
) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut prompt = Vec::new();
        let mut byte = [0];
        while stream.read(&mut byte).unwrap() == 1 && byte[0] != 0 {
            prompt.push(byte[0]);
        }
        stream.write_all(answer.as_bytes()).unwrap();
        String::from_utf8(prompt).unwrap()
    })
}

#[test]
fn askpass_relays_a_prompt_and_prints_the_answer() {
    // Git and ssh run Zed's askpass script, which pipes the prompt into `rozed --askpass=<socket>`.
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("askpass.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let prompt = answer_one_prompt(listener, "hunter2\n");

    let mut child = rozed_command(&[&format!("--askpass={}", socket.display())])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"Password for 'https://github.com': \0")
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        prompt.join().unwrap(),
        "Password for 'https://github.com': "
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "hunter2\n");
}

#[test]
fn askpass_without_a_socket_is_an_error() {
    let output = rozed(&["--askpass="]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--askpass needs a socket"), "{stderr}");
}

#[test]
fn an_unknown_option_is_an_error() {
    let output = rozed(&["--no-such-flag"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("unknown option `--no-such-flag`"),
        "{stderr}"
    );
}

#[test]
fn rozed_does_not_start_inside_rozed() {
    let output = rozed(&[]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("not starting"), "{stderr}");
}
