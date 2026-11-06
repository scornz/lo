//! Progress of a fetch, read from git's progress output, so the app can show the first clone of a
//! big repository coming in (design §11, "First open").

use std::io::Read as _;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;

use crate::Error;
use crate::git::{Finished, GitCommand};

/// One update from git, such as "Receiving objects: 45% (555/1234)".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    /// What git is doing, in its words: "Counting objects", "Receiving objects", "Resolving
    /// deltas"… The ones GitHub does are prefixed "remote: " by git and not here.
    pub stage: String,
    /// How many items are done.
    pub done: u64,
    /// How many there are, when git knows.
    pub total: Option<u64>,
}

impl Progress {
    /// How far through the stage git is, from 0 to 100, when it knows the total.
    pub fn percent(&self) -> Option<u8> {
        let total = self.total.filter(|total| *total > 0)?;
        Some((self.done.min(total) * 100 / total) as u8)
    }
}

/// A function that receives progress updates.
pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

/// Parses one line of git's progress output, such as
/// `remote: Counting objects:  25% (1/4)` or `Receiving objects:  45% (555/1234), 1.20 MiB | 2.40
/// MiB/s`. Other lines give `None`.
pub(crate) fn parse(line: &str) -> Option<Progress> {
    let line = line.trim();
    let line = line.strip_prefix("remote:").unwrap_or(line).trim_start();
    let (stage, rest) = line.split_once(": ")?;
    let rest = rest.trim_start();
    let (done, total) = if let Some((_, counts)) = rest.split_once('(') {
        // "45% (555/1234), 1.20 MiB | 2.40 MiB/s"
        let (counts, _) = counts.split_once(')')?;
        let (done, total) = counts.split_once('/')?;
        (done.parse().ok()?, Some(total.parse().ok()?))
    } else {
        // "4, done." or "4"
        let count = rest.split([',', ' ']).next()?;
        (count.parse().ok()?, None)
    };
    if stage.is_empty() || !stage.chars().all(|c| c.is_ascii_alphabetic() || c == ' ') {
        return None;
    }
    Some(Progress {
        stage: stage.to_string(),
        done,
        total,
    })
}

impl GitCommand {
    /// Runs the command, sending each progress line git writes to `progress`. Lines that aren't
    /// progress go into [`Finished::stderr`].
    pub(crate) fn finish_with_progress(self, progress: &ProgressFn) -> Result<Finished, Error> {
        let (mut command, display) = self.into_command();
        let started = Instant::now();
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(Error::io(format!("running {display}")))?;
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let (stdout, messages) = std::thread::scope(|scope| {
            let messages = scope.spawn(|| {
                let mut messages = String::new();
                let mut buffer = [0u8; 4096];
                let mut line = Vec::new();
                let flush = |line: &mut Vec<u8>, messages: &mut String| {
                    let text = String::from_utf8_lossy(line);
                    match parse(&text) {
                        Some(update) => progress(update),
                        None if !text.trim().is_empty() => {
                            messages.push_str(text.trim_end());
                            messages.push('\n');
                        }
                        None => {}
                    }
                    line.clear();
                };
                // git ends a progress update with \r and a finished stage with \n.
                while let Ok(read) = stderr.read(&mut buffer) {
                    if read == 0 {
                        break;
                    }
                    for &byte in &buffer[..read] {
                        if byte == b'\r' || byte == b'\n' {
                            flush(&mut line, &mut messages);
                        } else {
                            line.push(byte);
                        }
                    }
                }
                flush(&mut line, &mut messages);
                messages
            });
            let mut output = Vec::new();
            let read = stdout.read_to_end(&mut output);
            let messages = messages.join().unwrap_or_default();
            (read.map(|_| output), messages)
        });
        let stdout = stdout.map_err(Error::io(format!("reading {display}'s output")))?;
        let status = child
            .wait()
            .map_err(Error::io(format!("running {display}")))?;
        log::debug!("{display} exited with {status} in {:?}", started.elapsed());
        Ok(Finished {
            status,
            stdout,
            stderr: messages,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(stage: &str, done: u64, total: Option<u64>) -> Option<Progress> {
        Some(Progress {
            stage: stage.into(),
            done,
            total,
        })
    }

    #[test]
    fn parses_gits_progress_lines() {
        assert_eq!(
            parse("remote: Counting objects:  25% (1/4)        "),
            progress("Counting objects", 1, Some(4))
        );
        assert_eq!(
            parse("Receiving objects:  45% (555/1234), 1.20 MiB | 2.40 MiB/s"),
            progress("Receiving objects", 555, Some(1234))
        );
        assert_eq!(
            parse("Resolving deltas: 100% (10/10), done."),
            progress("Resolving deltas", 10, Some(10))
        );
        assert_eq!(
            parse("remote: Enumerating objects: 4, done.        "),
            progress("Enumerating objects", 4, None)
        );
        assert_eq!(
            parse("remote: Enumerating objects: 1234"),
            progress("Enumerating objects", 1234, None)
        );
    }

    #[test]
    fn other_lines_arent_progress() {
        for line in [
            "",
            "remote: Total 4 (delta 0), reused 0 (delta 0), pack-reused 0 (from 0)",
            "From https://github.com/sharkdp/fd",
            " * [new ref]         refs/pull/1/head -> refs/lo/pull/1/head",
            "fatal: couldn't find remote ref refs/pull/2/head",
            "remote: Repository not found.",
            "error: RPC failed; curl 56 Recv failure: Connection reset by peer",
        ] {
            assert_eq!(parse(line), None, "{line}");
        }
    }

    #[test]
    fn percent_is_done_over_total() {
        let percent = |done, total| {
            Progress {
                stage: "Receiving objects".into(),
                done,
                total,
            }
            .percent()
        };
        assert_eq!(percent(555, Some(1234)), Some(44));
        assert_eq!(percent(4, Some(4)), Some(100));
        assert_eq!(percent(4, None), None);
        assert_eq!(percent(0, Some(0)), None);
    }
}
