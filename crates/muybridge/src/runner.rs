//! Running the bundled ffmpeg/ffprobe without ever blocking the main loop.
//!
//! Both are spawned as an argv vector through `gio::Subprocess` — no shell, so
//! no filename is ever re-interpreted on its way to the process. Output is read
//! incrementally on the main context and parsed as it arrives, which is what
//! makes the progress bar move during a long extraction.
//!
//! stderr is merged into stdout so one [`StreamParser`] sees ffmpeg's progress
//! blocks *and* its error text, in order. The last few error lines are what a
//! failed run reports back to the user.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::rc::Rc;

use ffmpeg_frames::{parse_probe, Event, Probe, Progress, StreamParser};
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

/// Signal sent on cancel. ffmpeg finalises what it has written and exits.
const SIGTERM: i32 = 15;

/// How many trailing output lines to keep for the failure message.
const MESSAGE_TAIL: usize = 8;

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Success,
    Failed,
    Cancelled,
}

/// The result of one ffmpeg run.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub status: Status,
    /// Human-readable: the summary on success, ffmpeg's own words on failure.
    pub message: String,
    /// Frames ffmpeg reported writing, from the last progress block.
    pub frames: Option<u64>,
}

/// A running ffmpeg. Dropping this does not stop it; [`cancel`](Self::cancel) does.
#[derive(Debug)]
pub struct Runner {
    proc: gio::Subprocess,
    cancelled: Rc<Cell<bool>>,
}

impl Runner {
    /// Ask ffmpeg to stop. The outcome arrives as [`Status::Cancelled`], with
    /// the frames written so far left on disk.
    pub fn cancel(&self) {
        self.cancelled.set(true);
        self.proc.send_signal(SIGTERM);
    }
}

/// Spawn `ffmpeg argv…` (resolved via PATH → `/app/bin` in the sandbox).
///
/// `on_progress` fires for every `-progress` block as it arrives; `on_done`
/// fires exactly once, after the process has exited and its output is drained.
pub fn spawn_ffmpeg<P, D>(
    argv: Vec<OsString>,
    on_progress: P,
    on_done: D,
) -> Result<Runner, glib::Error>
where
    P: Fn(Progress) + 'static,
    D: FnOnce(Outcome) + 'static,
{
    let proc = spawn("ffmpeg", &argv)?;
    let stdout = proc.stdout_pipe().expect("STDOUT_PIPE requested");
    let cancelled = Rc::new(Cell::new(false));
    let on_done = RefCell::new(Some(on_done));

    glib::spawn_future_local(glib::clone!(
        #[strong]
        proc,
        #[strong]
        cancelled,
        async move {
            let mut parser = StreamParser::new();
            let mut messages: VecDeque<String> = VecDeque::new();
            let mut frames: Option<u64> = None;

            // Scoped so `handle`'s borrows of `messages`/`frames` end with it.
            {
                let mut handle = |event: Event| match event {
                    Event::Progress(progress) => {
                        if let Some(f) = progress.frame {
                            frames = Some(f);
                        }
                        on_progress(progress);
                    }
                    Event::Message(line) => {
                        if messages.len() == MESSAGE_TAIL {
                            messages.pop_front();
                        }
                        messages.push_back(line);
                    }
                };

                loop {
                    match stdout
                        .read_bytes_future(8192, glib::Priority::DEFAULT)
                        .await
                    {
                        Ok(bytes) if bytes.is_empty() => break, // EOF
                        Ok(bytes) => {
                            let chunk = String::from_utf8_lossy(&bytes);
                            for event in parser.feed(&chunk) {
                                handle(event);
                            }
                        }
                        Err(_) => break,
                    }
                }
                for event in parser.finish() {
                    handle(event);
                }
            }

            let _ = proc.wait_future().await;

            let outcome = if cancelled.get() || proc.has_signaled() {
                Outcome {
                    status: Status::Cancelled,
                    message: "Extraction cancelled.".to_string(),
                    frames,
                }
            } else if proc.exit_status() == 0 {
                Outcome {
                    status: Status::Success,
                    message: String::new(),
                    frames,
                }
            } else {
                Outcome {
                    status: Status::Failed,
                    message: failure_message(&messages, proc.exit_status()),
                    frames,
                }
            };

            if let Some(done) = on_done.borrow_mut().take() {
                done(outcome);
            }
        }
    ));

    Ok(Runner { proc, cancelled })
}

/// Run `ffprobe` over a video and hand back what it says. A probe that fails
/// is not an error the user needs to see — the details row just stays empty and
/// the frame estimate is skipped.
pub fn run_ffprobe<D>(argv: Vec<OsString>, on_done: D)
where
    D: FnOnce(Option<Probe>) + 'static,
{
    let Ok(proc) = spawn("ffprobe", &argv) else {
        on_done(None);
        return;
    };
    let stdout = proc.stdout_pipe().expect("STDOUT_PIPE requested");
    let on_done = RefCell::new(Some(on_done));

    glib::spawn_future_local(async move {
        let mut text = String::new();
        loop {
            match stdout
                .read_bytes_future(4096, glib::Priority::DEFAULT)
                .await
            {
                Ok(bytes) if bytes.is_empty() => break,
                Ok(bytes) => text.push_str(&String::from_utf8_lossy(&bytes)),
                Err(_) => break,
            }
        }
        let _ = proc.wait_future().await;

        let probe = (proc.has_exited() && proc.exit_status() == 0).then(|| parse_probe(&text));
        if let Some(done) = on_done.borrow_mut().take() {
            done(probe);
        }
    });
}

fn spawn(program: &str, argv: &[OsString]) -> Result<gio::Subprocess, glib::Error> {
    let mut full: Vec<OsString> = Vec::with_capacity(argv.len() + 1);
    full.push(OsString::from(program));
    full.extend_from_slice(argv);
    let refs: Vec<&OsStr> = full.iter().map(OsString::as_os_str).collect();

    gio::Subprocess::newv(
        &refs,
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE,
    )
}

/// Pick the most useful line out of ffmpeg's trailing output. Its real error is
/// almost always the last non-empty line; the ones before it are stream info.
fn failure_message(messages: &VecDeque<String>, code: i32) -> String {
    messages
        .iter()
        .rev()
        .find(|line| !line.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| format!("ffmpeg exited with status {code}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_message_prefers_ffmpegs_last_word() {
        let messages: VecDeque<String> = ["Input #0, mov,mp4", "Output file is empty", "   "]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(failure_message(&messages, 1), "Output file is empty");
    }

    #[test]
    fn failure_message_falls_back_to_the_exit_code() {
        assert_eq!(
            failure_message(&VecDeque::new(), 234),
            "ffmpeg exited with status 234."
        );
    }
}
