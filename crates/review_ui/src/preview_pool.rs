//! Running the preview helper, `rozed --render-preview` (see [`crate::image_preview`]), for each
//! image a diff shows, and watching it.
//!
//! At most [`HELPERS`] run at once, from a queue. Each gets [`Limits::deadline`] of wall-clock
//! time and [`Limits::max_resident`] of memory, polled while it runs, and is killed past either.
//! Dropping a request takes it off the queue, or kills its helper. Whatever goes wrong, the image
//! shows why it couldn't be previewed; rozed itself never decodes a pull request's image.

use std::future::Future;
use std::io::{Read as _, Write as _};
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, LazyLock, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::channel::oneshot;
use gpui::ImageFormat;

use crate::image_preview::{self, MAX_INPUT, PREVIEW_SIDE, Preview, REPLY_MAGIC};

/// How many helpers run at once.
pub(crate) const HELPERS: usize = 2;

/// How much a helper may use before it's killed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// From the helper's start to its reply.
    pub(crate) deadline: Duration,
    /// Its resident memory.
    pub(crate) max_resident: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // An unoptimized build decodes several times slower.
            deadline: if cfg!(debug_assertions) {
                Duration::from_secs(8)
            } else {
                Duration::from_secs(3)
            },
            max_resident: 512 * 1024 * 1024,
        }
    }
}

/// How often a running helper is checked.
const POLL: Duration = Duration::from_millis(10);

/// What a helper's run cost, for tests and logs.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Cost {
    pub(crate) elapsed: Duration,
    pub(crate) peak_resident: u64,
}

/// A queue of requests and the threads that run their helpers.
pub(crate) struct Pool {
    queue: Mutex<mpsc::Sender<Job>>,
    #[cfg(test)]
    most_running: Arc<AtomicUsize>,
}

/// The pool every diff's images go through.
static POOL: LazyLock<Pool> = LazyLock::new(|| Pool::new(HELPERS, Limits::default(), Vec::new()));

/// Asks the helper for a preview of `bytes`, an image of `format`, that fits `fit` pixels.
pub(crate) fn request(format: ImageFormat, bytes: Arc<[u8]>, fit: (u32, u32)) -> PreviewRequest {
    POOL.request(format, bytes, fit)
}

struct Job {
    format: ImageFormat,
    bytes: Arc<[u8]>,
    fit: (u32, u32),
    cancelled: Arc<AtomicBool>,
    pid: Arc<Mutex<Option<u32>>>,
    reply: oneshot::Sender<(Result<Preview, String>, Cost)>,
}

impl Pool {
    /// A pool of `helpers` threads, each running one helper at a time within `limits`, with
    /// `env` added to each helper's environment.
    pub(crate) fn new(helpers: usize, limits: Limits, env: Vec<(String, String)>) -> Self {
        let env = Arc::new(env);
        let (sender, receiver) = mpsc::channel::<Job>();
        let receiver = Arc::new(Mutex::new(receiver));
        let running = Arc::new(AtomicUsize::new(0));
        let most_running = Arc::new(AtomicUsize::new(0));
        for index in 0..helpers {
            let (receiver, running, most_running, env) = (
                receiver.clone(),
                running.clone(),
                most_running.clone(),
                env.clone(),
            );
            std::thread::Builder::new()
                .name(format!("preview-{index}"))
                .spawn(move || {
                    loop {
                        let job = match receiver.lock() {
                            Ok(receiver) => receiver.recv(),
                            Err(_) => return,
                        };
                        let Ok(job) = job else {
                            return;
                        };
                        // Taken off the queue before it started.
                        if job.cancelled.load(Ordering::Relaxed) {
                            continue;
                        }
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        most_running.fetch_max(now, Ordering::SeqCst);
                        log::debug!("{now} preview helpers running");
                        let result = run(&job, limits, &env);
                        running.fetch_sub(1, Ordering::SeqCst);
                        job.reply.send(result).ok();
                    }
                })
                .expect("starting a preview thread");
        }
        Self {
            queue: Mutex::new(sender),
            #[cfg(test)]
            most_running,
        }
    }

    pub(crate) fn request(
        &self,
        format: ImageFormat,
        bytes: Arc<[u8]>,
        fit: (u32, u32),
    ) -> PreviewRequest {
        let (reply, receiver) = oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(Mutex::new(None));
        let job = Job {
            format,
            bytes,
            fit,
            cancelled: cancelled.clone(),
            pid: pid.clone(),
            reply,
        };
        if let Ok(queue) = self.queue.lock() {
            queue.send(job).ok();
        }
        PreviewRequest {
            receiver,
            cancelled,
            #[cfg(test)]
            pid,
        }
    }

    /// The most helpers that ever ran at once.
    #[cfg(test)]
    pub(crate) fn most_running(&self) -> usize {
        self.most_running.load(Ordering::SeqCst)
    }
}

/// A preview on its way: it resolves to the preview, or why there's none. Dropping it takes the
/// request off the queue, or kills its helper.
pub(crate) struct PreviewRequest {
    receiver: oneshot::Receiver<(Result<Preview, String>, Cost)>,
    cancelled: Arc<AtomicBool>,
    #[cfg(test)]
    pid: Arc<Mutex<Option<u32>>>,
}

impl PreviewRequest {
    /// The reply, once there is one, without waiting. gpui's tests refuse a task woken from the
    /// pool's threads, so the app checks on its requests rather than await them.
    pub(crate) fn try_take(&mut self) -> Option<(Result<Preview, String>, Cost)> {
        match self.receiver.try_recv() {
            Ok(Some(reply)) => Some(reply),
            Ok(None) => None,
            Err(_) => Some((Err("the preview stopped".to_string()), Cost::default())),
        }
    }

    /// The helper's process, while it runs.
    #[cfg(test)]
    pub(crate) fn pid(&self) -> Option<u32> {
        *self.pid.lock().unwrap()
    }
}

impl Future for PreviewRequest {
    type Output = (Result<Preview, String>, Cost);

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.receiver).poll(cx).map(|reply| {
            reply.unwrap_or_else(|_| (Err("the preview stopped".to_string()), Cost::default()))
        })
    }
}

impl Drop for PreviewRequest {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

/// The helper's command line.
#[cfg(not(test))]
fn helper_command() -> std::io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg(image_preview::HELPER_FLAG);
    Ok(command)
}

/// In tests, the test binary stands in for rozed: its `helper_process` test serves the request
/// when it's told to.
#[cfg(test)]
fn helper_command() -> std::io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "preview_pool::tests::helper_process",
            "--exact",
            "--nocapture",
            "--test-threads=1",
            "-q",
        ])
        .env(tests::HELPER_ENV, "1");
    Ok(command)
}

/// Runs one helper for `job`, watching it until it replies, is cancelled or goes past `limits`.
fn run(job: &Job, limits: Limits, env: &[(String, String)]) -> (Result<Preview, String>, Cost) {
    let started = Instant::now();
    let mut cost = Cost::default();
    let result = run_watched(job, limits, env, started, &mut cost);
    cost.elapsed = started.elapsed();
    // The helper's own "too large to preview" says it all.
    let result = result.map_err(|why| {
        if why.starts_with("too large") {
            why
        } else {
            format!("couldn't preview this image: {why}")
        }
    });
    if let Err(error) = &result {
        log::info!(
            "{error} ({:?}, {} KiB)",
            cost.elapsed,
            cost.peak_resident / 1024
        );
    }
    (result, cost)
}

fn run_watched(
    job: &Job,
    limits: Limits,
    env: &[(String, String)],
    started: Instant,
    cost: &mut Cost,
) -> Result<Preview, String> {
    if job.bytes.len() > MAX_INPUT {
        return Err("it's too large to read".to_string());
    }
    let header = image_preview::request_header(job.format, job.fit)
        .ok_or("rozed doesn't preview this format")?;
    let mut command = helper_command().map_err(|error| format!("no helper: {error}"))?;
    command
        .env_remove("ROZED_PARENT_PID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(not(test))]
    command.env_clear();
    command.envs(env.iter().map(|(name, value)| (name, value)));
    let mut child = command
        .spawn()
        .map_err(|error| format!("couldn't start the helper: {error}"))?;
    *job.pid.lock().unwrap_or_else(|error| error.into_inner()) = Some(child.id());
    let outcome = watch(&mut child, job, header, limits, started, cost);
    *job.pid.lock().unwrap_or_else(|error| error.into_inner()) = None;
    outcome
}

fn watch(
    child: &mut Child,
    job: &Job,
    header: String,
    limits: Limits,
    started: Instant,
    cost: &mut Cost,
) -> Result<Preview, String> {
    let (mut stdin, mut stdout) = (child.stdin.take(), child.stdout.take());
    // The helper reads all of its input before it writes, but the pipes are small, so each end
    // has its own thread and the watch below is never blocked.
    let bytes = job.bytes.clone();
    let writer = std::thread::spawn(move || {
        if let Some(stdin) = stdin.as_mut() {
            stdin
                .write_all(header.as_bytes())
                .and_then(|()| stdin.write_all(&bytes))
                .ok();
        }
    });
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        if let Some(stdout) = stdout.as_mut() {
            let most = (PREVIEW_SIDE as u64).pow(2) * 4 + 4096;
            stdout.take(most).read_to_end(&mut output).ok();
        }
        output
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        let resident = resident_memory(child.id()).unwrap_or(0);
        cost.peak_resident = cost.peak_resident.max(resident);
        let stop = if job.cancelled.load(Ordering::Relaxed) {
            Some("it was cancelled".to_string())
        } else if started.elapsed() > limits.deadline {
            Some(format!(
                "it took longer than {} s",
                limits.deadline.as_secs_f32()
            ))
        } else if resident > limits.max_resident {
            Some(format!(
                "it needed more than {} MiB",
                limits.max_resident / 1024 / 1024
            ))
        } else {
            None
        };
        if let Some(why) = stop {
            child.kill().ok();
            child.wait().ok();
            writer.join().ok();
            reader.join().ok();
            return Err(why);
        }
        std::thread::sleep(POLL);
    };
    writer.join().ok();
    let output = reader.join().unwrap_or_default();
    let reply = parse_reply(&output);
    match (reply, status) {
        (Some(reply), _) => reply,
        (None, Some(status)) if !status.success() => Err(match signal_of(status) {
            Some(signal) => format!("the decoder crashed (signal {signal})"),
            None => format!("the decoder failed ({status})"),
        }),
        (None, _) => Err("the decoder didn't reply".to_string()),
    }
}

#[cfg(unix)]
fn signal_of(status: std::process::ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(&status)
}

/// The helper's reply: its preview, why there's none, or `None` when it didn't finish one.
fn parse_reply(output: &[u8]) -> Option<Result<Preview, String>> {
    let start = output
        .windows(REPLY_MAGIC.len())
        .position(|window| window == REPLY_MAGIC)?
        + REPLY_MAGIC.len();
    let rest = &output[start..];
    let end = rest.iter().position(|byte| *byte == b'\n')?;
    let line = std::str::from_utf8(&rest[..end]).ok()?;
    let pixels = &rest[end + 1..];
    if let Some(error) = line.strip_prefix("err ") {
        return Some(Err(error.to_string()));
    }
    let mut size = line.strip_prefix("ok ")?.split(' ');
    let width: u32 = size.next()?.parse().ok()?;
    let height: u32 = size.next()?.parse().ok()?;
    if width == 0 || height == 0 || width > PREVIEW_SIDE || height > PREVIEW_SIDE {
        return None;
    }
    let length = (width * height * 4) as usize;
    let pixels = pixels.get(..length)?.to_vec();
    Some(Ok(Preview::from_raw(width, height, pixels)?))
}

/// A process's resident memory, in bytes.
pub(crate) fn resident_memory(pid: u32) -> Option<u64> {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: `info` is a `proc_taskinfo` of `size` bytes, which PROC_PIDTASKINFO fills.
    let filled = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            (&raw mut info).cast(),
            size,
        )
    };
    (filled == size).then_some(info.pti_resident_size)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use std::thread::JoinHandle;

    use crate::image_preview::tests::png_of_zeros;

    /// Set in the test binary's environment when it runs as the helper.
    pub(crate) const HELPER_ENV: &str = "ROZED_TEST_PREVIEW_HELPER";

    /// Makes the helper crash.
    const ABORT_ENV: &str = "ROZED_TEST_PREVIEW_ABORT";

    /// Makes the helper enter its sandbox and try to read this file and the network, exiting 0
    /// when neither works.
    const PROBE_ENV: &str = "ROZED_TEST_PREVIEW_PROBE";

    /// The helper, when the test binary runs as one; otherwise nothing.
    #[test]
    fn helper_process() {
        if std::env::var_os(HELPER_ENV).is_some() {
            if std::env::var_os(ABORT_ENV).is_some() {
                std::process::abort();
            }
            if let Some(path) = std::env::var_os(PROBE_ENV) {
                image_preview::sandbox::enter();
                let read = std::fs::read(&path).is_ok();
                let connected = std::net::TcpStream::connect(("127.0.0.1", 9)).is_ok()
                    || std::net::UdpSocket::bind("127.0.0.1:0").is_ok();
                std::process::exit(if read || connected { 3 } else { 0 });
            }
            image_preview::serve_helper();
        }
    }

    /// The most this process's resident memory grows over its start, sampled every millisecond.
    pub(crate) struct PeakSampler {
        stop: Arc<AtomicBool>,
        thread: JoinHandle<u64>,
        base: u64,
    }

    impl PeakSampler {
        pub(crate) fn start() -> Self {
            let pid = std::process::id();
            let base = resident_memory(pid).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let thread = std::thread::spawn({
                let stop = stop.clone();
                move || {
                    let mut peak = 0;
                    while !stop.load(Ordering::Relaxed) {
                        peak = peak.max(resident_memory(pid).unwrap_or(0));
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    peak
                }
            });
            Self { stop, thread, base }
        }

        pub(crate) fn stop(self) -> u64 {
            self.stop.store(true, Ordering::Relaxed);
            self.thread.join().unwrap().saturating_sub(self.base)
        }
    }

    const MIB: u64 = 1024 * 1024;

    fn svg(body: &str) -> Vec<u8> {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="512" height="512">{body}</svg>"#
        )
        .into_bytes()
    }

    /// `depth` levels of `open` and `close` around a rectangle.
    fn nested(depth: usize, open: &str, close: &str, rect: &str) -> Vec<u8> {
        svg(&format!(
            "{}{rect}{}",
            open.repeat(depth),
            close.repeat(depth)
        ))
    }

    /// A rectangle used 4 × 10⁵ times, through six levels of `<use>`.
    fn use_fan_out() -> Vec<u8> {
        let mut defs = String::from(
            r#"<rect id="l0" width="512" height="512" fill="red" stroke="blue" stroke-width="3" stroke-dasharray="1 1" opacity="0.5"/>"#,
        );
        for level in 1..=6 {
            let uses = if level == 6 { 4 } else { 10 };
            defs.push_str(&format!(r#"<g id="l{level}">"#));
            for _ in 0..uses {
                defs.push_str(&format!(r##"<use xlink:href="#l{}"/>"##, level - 1));
            }
            defs.push_str("</g>");
        }
        svg(&format!(r##"<defs>{defs}</defs><use xlink:href="#l6"/>"##))
    }

    /// Images a few hundred bytes to a couple of megabytes long that overflow a stack, take
    /// gigabytes or run for hours, from the review of this pool.
    fn bombs() -> Vec<(&'static str, ImageFormat, Vec<u8>)> {
        let rect = r#"<rect width="512" height="512" fill="red"/>"#;
        let huge_rect =
            r#"<rect x="-100000" y="-100000" width="200000" height="200000" fill="red"/>"#;
        vec![
            (
                "200 nested groups",
                ImageFormat::Svg,
                nested(200, "<g>", "</g>", rect),
            ),
            (
                "1000 nested groups",
                ImageFormat::Svg,
                nested(1000, "<g>", "</g>", rect),
            ),
            (
                "a pattern tile 20000 a side",
                ImageFormat::Svg,
                svg(
                    r#"<pattern id="p" patternUnits="userSpaceOnUse" width="20000" height="20000"><rect width="20000" height="20000" fill="red"/></pattern><rect width="512" height="512" fill="url(#p)"/>"#,
                ),
            ),
            (
                "60 nested opacity groups",
                ImageFormat::Svg,
                nested(60, r#"<g opacity="0.99">"#, "</g>", huge_rect),
            ),
            (
                "feMorphology radius 100000",
                ImageFormat::Svg,
                svg(
                    r#"<filter id="f"><feMorphology operator="dilate" radius="100000"/></filter><rect width="512" height="512" fill="red" filter="url(#f)"/>"#,
                ),
            ),
            (
                "feTurbulence of 10⁹ octaves",
                ImageFormat::Svg,
                svg(
                    r#"<filter id="f"><feTurbulence baseFrequency="0.05" numOctaves="1000000000"/></filter><rect width="512" height="512" filter="url(#f)"/>"#,
                ),
            ),
            ("a <use> fan-out", ImageFormat::Svg, use_fan_out()),
            // As in resvg's own tests of recursive links: usvg recurses without end.
            (
                "feImage links in a loop",
                ImageFormat::Svg,
                svg(
                    r##"<filter id="f1"><feImage xlink:href="#g1"/></filter><g id="g1" filter="url(#f2)"><rect width="9" height="9"/></g><filter id="f2"><feImage xlink:href="#g2"/></filter><g id="g2" filter="url(#f1)"><rect width="9" height="9"/></g><rect width="512" height="512" filter="url(#f1)"/>"##,
                ),
            ),
            (
                "a 1 × 1 PNG with a 480 MiB iCCP",
                ImageFormat::Png,
                png_of_zeros(1, 1, 8, Some(480 << 20)),
            ),
            (
                "a 16-bit PNG of 40 megapixels",
                ImageFormat::Png,
                png_of_zeros(6324, 6324, 16, None),
            ),
        ]
    }

    #[test]
    fn bombs_end_within_the_deadline_and_leave_rozed_s_memory_flat() {
        let limits = Limits {
            deadline: Duration::from_secs(2),
            max_resident: 512 * MIB,
        };
        let pool = Pool::new(HELPERS, limits, Vec::new());
        let bombs = bombs();
        let sampler = PeakSampler::start();
        let requests: Vec<_> = bombs
            .iter()
            .map(|(_, format, bytes)| pool.request(*format, bytes.as_slice().into(), (1024, 480)))
            .collect();
        let replies: Vec<_> = requests
            .into_iter()
            .map(futures::executor::block_on)
            .collect();
        let parent = sampler.stop();
        let mut report = String::new();
        for ((name, _, bytes), (result, cost)) in bombs.iter().zip(&replies) {
            let outcome = match result {
                Ok(preview) => format!("previewed {} × {}", preview.width(), preview.height()),
                Err(error) => error.clone(),
            };
            report.push_str(&format!(
                "{name} ({} bytes): {:.2} s, helper peak {} MiB: {outcome}\n",
                bytes.len(),
                cost.elapsed.as_secs_f32(),
                cost.peak_resident / MIB,
            ));
        }
        report.push_str(&format!(
            "rozed's own memory grew at most {} MiB\n",
            parent / MIB
        ));
        eprintln!("{report}");
        for ((name, _, _), (_, cost)) in bombs.iter().zip(&replies) {
            assert!(
                cost.elapsed < limits.deadline + Duration::from_millis(500),
                "{name} ran past the deadline\n{report}"
            );
            assert!(
                cost.peak_resident < limits.max_resident + 256 * MIB,
                "{name} grew past the limit\n{report}"
            );
        }
        assert!(parent < 128 * MIB, "{report}");
        // A deep nest renders, on the helper's large stack; the rest give up and say why.
        assert!(replies[0].0.is_ok() && replies[1].0.is_ok(), "{report}");
        for (name, (result, _)) in bombs.iter().map(|bomb| bomb.0).zip(&replies).skip(2) {
            if name.contains("iCCP") {
                continue;
            }
            let error = result.as_ref().unwrap_err();
            assert!(
                error.starts_with("couldn't preview this image: ")
                    || error.starts_with("too large to preview"),
                "{name}: {error}"
            );
        }
        assert_eq!(
            replies[9].0.as_ref().unwrap_err(),
            "too large to preview: 6324 × 6324"
        );
        let looped = replies[7].0.as_ref().unwrap_err();
        assert!(
            looped.starts_with("couldn't preview this image: the decoder crashed"),
            "{looped}"
        );
    }

    fn slow_svg() -> Arc<[u8]> {
        svg(r#"<filter id="f"><feMorphology operator="dilate" radius="100000"/></filter><rect width="512" height="512" fill="red" filter="url(#f)"/>"#).into()
    }

    #[test]
    fn at_most_two_helpers_run_at_once() {
        let limits = Limits {
            deadline: Duration::from_millis(500),
            ..Limits::default()
        };
        let pool = Pool::new(HELPERS, limits, Vec::new());
        let requests: Vec<_> = (0..5)
            .map(|_| pool.request(ImageFormat::Svg, slow_svg(), (512, 512)))
            .collect();
        for (result, _) in requests.into_iter().map(futures::executor::block_on) {
            assert_eq!(
                result.unwrap_err(),
                "couldn't preview this image: it took longer than 0.5 s"
            );
        }
        assert_eq!(pool.most_running(), HELPERS);
    }

    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 checks the process exists without sending anything.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    #[test]
    fn dropping_a_request_kills_its_helper() {
        let limits = Limits {
            deadline: Duration::from_secs(60),
            ..Limits::default()
        };
        let pool = Pool::new(1, limits, Vec::new());
        let request = pool.request(ImageFormat::Svg, slow_svg(), (512, 512));
        let started = Instant::now();
        let pid = loop {
            if let Some(pid) = request.pid() {
                break pid;
            }
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
        };
        // Long enough for it to be decoding.
        std::thread::sleep(Duration::from_millis(300));
        assert!(alive(pid));
        drop(request);
        let dropped = Instant::now();
        while alive(pid) {
            assert!(dropped.elapsed() < Duration::from_secs(1), "still running");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_helper_that_crashes_says_so() {
        let env = vec![(ABORT_ENV.to_string(), "1".to_string())];
        let pool = Pool::new(1, Limits::default(), env);
        let (result, _) = futures::executor::block_on(pool.request(
            ImageFormat::Png,
            image_preview::tests::png_of(2, 2).into(),
            (64, 64),
        ));
        assert_eq!(
            result.unwrap_err(),
            "couldn't preview this image: the decoder crashed (signal 6)"
        );
    }

    #[test]
    fn a_helper_previews_an_ordinary_image() {
        let pool = Pool::new(1, Limits::default(), Vec::new());
        let (result, _) = futures::executor::block_on(pool.request(
            ImageFormat::Png,
            image_preview::tests::png_of(300, 100).into(),
            (150, 150),
        ));
        let preview = result.unwrap();
        assert_eq!(preview.dimensions(), (150, 50));
        // BGRA: red comes third.
        assert_eq!(&preview.as_raw()[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn the_helper_can_read_no_file_once_it_has_its_image() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "secret").unwrap();
        let status = helper_command()
            .unwrap()
            .env(PROBE_ENV, file.path())
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(0));
    }

    #[test]
    fn replies_are_read_after_anything_printed_before_them() {
        let mut output = b"\nrunning 1 test\n".to_vec();
        output.extend_from_slice(REPLY_MAGIC);
        output.extend_from_slice(b"ok 2 1\n");
        output.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let preview = parse_reply(&output).unwrap().unwrap();
        assert_eq!(preview.dimensions(), (2, 1));
        assert_eq!(preview.as_raw(), &[1, 2, 3, 4, 5, 6, 7, 8]);

        let mut error = REPLY_MAGIC.to_vec();
        error.extend_from_slice(b"err it has no size\n");
        assert_eq!(parse_reply(&error), Some(Err("it has no size".to_string())));

        // Cut short, too large, or no reply at all.
        assert_eq!(parse_reply(&output[..output.len() - 1]), None);
        let mut huge = REPLY_MAGIC.to_vec();
        huge.extend_from_slice(b"ok 5000 1\n");
        assert_eq!(parse_reply(&huge), None);
        assert_eq!(parse_reply(b"running 1 test\n"), None);
    }
}
