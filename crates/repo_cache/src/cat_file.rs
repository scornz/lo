//! A long-lived `git cat-file --batch`, which reads one object after another without starting a
//! git for each.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::Error;
use crate::git::GitCommand;

pub(crate) struct CatFile {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// What git has written to stderr so far, kept for the error when it dies.
    stderr: Arc<Mutex<String>>,
    stderr_reader: Option<JoinHandle<()>>,
    display: String,
}

/// An object read from the batch.
pub(crate) struct Object {
    pub kind: String,
    pub data: Vec<u8>,
}

/// How a read went wrong.
pub(crate) enum ReadError {
    /// The process is gone: it died earlier, or while answering. Its stderr says why.
    Died {
        stderr: String,
    },
    Io(std::io::Error),
}

impl CatFile {
    /// Starts `command`, which must be `git cat-file --batch`.
    pub fn spawn(command: GitCommand) -> Result<Self, Error> {
        let (mut command, display) = command.into_command();
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(Error::io(format!("running {display}")))?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let mut stderr_pipe = child.stderr.take().expect("stderr is piped");
        let stderr = Arc::new(Mutex::new(String::new()));
        let stderr_reader = std::thread::spawn({
            let stderr = stderr.clone();
            move || {
                let mut buffer = [0u8; 4096];
                while let Ok(read) = stderr_pipe.read(&mut buffer) {
                    if read == 0 {
                        break;
                    }
                    let text = String::from_utf8_lossy(&buffer[..read]);
                    let mut stderr = stderr.lock().unwrap_or_else(|e| e.into_inner());
                    // Keep the end, which says why git stopped.
                    if stderr.len() > 64 * 1024 {
                        stderr.drain(..32 * 1024);
                    }
                    stderr.push_str(&text);
                }
            }
        });
        log::debug!("started {display}");
        Ok(Self {
            child,
            stdin,
            stdout,
            stderr,
            stderr_reader: Some(stderr_reader),
            display,
        })
    }

    /// Reads `object`, any name git understands, such as `<commit>:<path>`. `None` when there's
    /// no such object. The name must not contain a newline.
    pub fn read(&mut self, object: &str) -> Result<Option<Object>, ReadError> {
        debug_assert!(!object.contains(['\n', '\r']));
        let request = format!("{object}\n");
        if let Err(error) = self
            .stdin
            .write_all(request.as_bytes())
            .and_then(|()| self.stdin.flush())
        {
            return Err(self.died_or(error));
        }
        let mut header = String::new();
        match self.stdout.read_line(&mut header) {
            Ok(0) => return Err(self.died()),
            Ok(_) => {}
            Err(error) => return Err(self.died_or(error)),
        }
        let header = header.trim_end_matches('\n');
        // "<object> missing", or "<object> ambiguous" for a short ID matching several.
        if header.ends_with(" missing") || header.ends_with(" ambiguous") {
            return Ok(None);
        }
        // "<oid> <type> <size>"
        let mut parts = header.split(' ');
        let (Some(_oid), Some(kind), Some(size), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(ReadError::Io(std::io::Error::other(format!(
                "{} printed an unexpected header {header:?}",
                self.display
            ))));
        };
        let Ok(size) = size.parse::<usize>() else {
            return Err(ReadError::Io(std::io::Error::other(format!(
                "{} printed an unexpected size {size:?}",
                self.display
            ))));
        };
        let mut data = vec![0; size + 1];
        if let Err(error) = self.stdout.read_exact(&mut data) {
            return Err(self.died_or(error));
        }
        // The object is followed by a newline.
        data.pop();
        Ok(Some(Object {
            kind: kind.to_string(),
            data,
        }))
    }

    /// The error for a process that stopped answering: waits for it and returns its stderr.
    fn died(&mut self) -> ReadError {
        let _ = self.child.wait();
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
        let stderr = self
            .stderr
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        ReadError::Died { stderr }
    }

    /// [`Self::died`] when the process has exited, otherwise `error`.
    fn died_or(&mut self, error: std::io::Error) -> ReadError {
        match self.child.try_wait() {
            Ok(Some(_)) => self.died(),
            _ if error.kind() == std::io::ErrorKind::BrokenPipe => self.died(),
            _ => ReadError::Io(error),
        }
    }

    pub fn display(&self) -> &str {
        &self.display
    }
}

impl Drop for CatFile {
    fn drop(&mut self) {
        // Closing stdin ends the batch; kill in case it's in the middle of a download.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
