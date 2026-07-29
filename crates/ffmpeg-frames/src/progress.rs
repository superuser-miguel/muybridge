//! Reading ffmpeg's `-progress pipe:1` stream.
//!
//! With `-progress pipe:1 -nostats`, ffmpeg writes repeating blocks of
//! `key=value` lines terminated by `progress=continue` (or `progress=end` for
//! the final one):
//!
//! ```text
//! frame=128
//! fps=41.2
//! out_time_us=38438438
//! speed=7.14x
//! progress=continue
//! ```
//!
//! stderr is merged into the same pipe so a single parser sees ffmpeg's error
//! text too; any line that is not a known progress key comes back as
//! [`Event::Message`], which is what makes a failed run explainable.

/// One `-progress` block: a snapshot of the run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
    pub frame: Option<u64>,
    /// Encoding rate (frames written per second), not the extraction rate.
    pub fps: Option<f64>,
    /// Position within the *selected range*, in seconds.
    pub out_time: Option<f64>,
    /// Playback speed multiple, e.g. `7.14` for ffmpeg's `7.14x`.
    pub speed: Option<f64>,
    /// True for the block ffmpeg emits as it exits (`progress=end`).
    pub finished: bool,
}

/// Anything the merged output stream can tell us.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Progress(Progress),
    /// A line from ffmpeg's normal (stderr) output — warnings and errors.
    Message(String),
}

/// Keys we consume from a progress block. Anything else on the pipe is
/// ffmpeg's own chatter and is surfaced as a message.
const PROGRESS_KEYS: &[&str] = &[
    "frame",
    "fps",
    "stream_0_0_q",
    "bitrate",
    "total_size",
    "out_time_us",
    "out_time_ms",
    "out_time",
    "dup_frames",
    "drop_frames",
    "speed",
    "progress",
];

/// Incremental parser: bytes arrive in arbitrary chunks, so partial lines are
/// held back until their newline shows up.
#[derive(Debug, Default)]
pub struct StreamParser {
    buf: String,
    current: Progress,
    have_fields: bool,
}

impl StreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of merged stdout/stderr; returns the events it completed.
    pub fn feed(&mut self, chunk: &str) -> Vec<Event> {
        self.buf.push_str(chunk);
        let mut events = Vec::new();
        while let Some(idx) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=idx).collect();
            self.consume_line(line.trim_end_matches(['\n', '\r']), &mut events);
        }
        events
    }

    /// Flush at EOF: emits a trailing partial line, and any progress fields
    /// that never got their terminating `progress=` line.
    pub fn finish(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            self.consume_line(line.trim_end_matches(['\n', '\r']), &mut events);
        }
        if self.have_fields {
            events.push(Event::Progress(std::mem::take(&mut self.current)));
            self.have_fields = false;
        }
        events
    }

    fn consume_line(&mut self, line: &str, events: &mut Vec<Event>) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }

        let Some((key, value)) = trimmed.split_once('=') else {
            events.push(Event::Message(trimmed.to_string()));
            return;
        };
        let (key, value) = (key.trim(), value.trim());
        if !PROGRESS_KEYS.contains(&key) {
            events.push(Event::Message(trimmed.to_string()));
            return;
        }
        if value == "N/A" {
            return;
        }

        match key {
            "frame" => {
                self.current.frame = value.parse().ok();
                self.have_fields = true;
            }
            "fps" => {
                self.current.fps = value.parse().ok();
                self.have_fields = true;
            }
            // ffmpeg's `out_time_ms` is microseconds too (long-standing quirk);
            // we read `out_time_us` and fall back to the timecode string.
            "out_time_us" => {
                self.current.out_time = value.parse::<f64>().ok().map(|us| us / 1_000_000.0);
                self.have_fields = true;
            }
            "out_time" => {
                if self.current.out_time.is_none() {
                    self.current.out_time = crate::Timecode::parse(value).ok().map(|t| t.seconds());
                    self.have_fields = true;
                }
            }
            "speed" => {
                self.current.speed = value.trim_end_matches('x').parse().ok();
                self.have_fields = true;
            }
            "progress" => {
                let mut done = std::mem::take(&mut self.current);
                done.finished = value == "end";
                self.have_fields = false;
                events.push(Event::Progress(done));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: &str = "\
frame=128
fps=41.20
stream_0_0_q=-0.0
bitrate=N/A
total_size=N/A
out_time_us=38438438
out_time_ms=38438438
out_time=00:00:38.438438
dup_frames=0
drop_frames=0
speed=7.14x
progress=continue
";

    fn progresses(events: Vec<Event>) -> Vec<Progress> {
        events
            .into_iter()
            .filter_map(|e| match e {
                Event::Progress(p) => Some(p),
                Event::Message(_) => None,
            })
            .collect()
    }

    #[test]
    fn parses_one_block() {
        let mut parser = StreamParser::new();
        let got = progresses(parser.feed(BLOCK));
        assert_eq!(got.len(), 1);
        let p = &got[0];
        assert_eq!(p.frame, Some(128));
        assert_eq!(p.fps, Some(41.20));
        assert_eq!(p.speed, Some(7.14));
        assert!((p.out_time.unwrap() - 38.438438).abs() < 1e-6);
        assert!(!p.finished);
    }

    /// The pipe delivers arbitrary chunk boundaries, including mid-line.
    #[test]
    fn survives_split_chunks() {
        let mut parser = StreamParser::new();
        let mut events = Vec::new();
        for chunk in BLOCK.as_bytes().chunks(7) {
            events.extend(parser.feed(std::str::from_utf8(chunk).unwrap()));
        }
        let got = progresses(events);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].frame, Some(128));
    }

    #[test]
    fn end_block_is_marked_finished() {
        let mut parser = StreamParser::new();
        let got = progresses(parser.feed("frame=412\nprogress=end\n"));
        assert_eq!(got.len(), 1);
        assert!(got[0].finished);
        assert_eq!(got[0].frame, Some(412));
    }

    #[test]
    fn ffmpeg_error_text_comes_back_as_messages() {
        let mut parser = StreamParser::new();
        let events = parser.feed(
            "/videos/clip.mp4: No such file or directory\n\
             [image2 @ 0x55] Could not open file\n",
        );
        let messages: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::Message(m) => Some(m.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            messages,
            [
                "/videos/clip.mp4: No such file or directory",
                "[image2 @ 0x55] Could not open file"
            ]
        );
    }

    #[test]
    fn finish_flushes_a_block_that_never_terminated() {
        let mut parser = StreamParser::new();
        assert!(progresses(parser.feed("frame=9\nout_time_us=1000000\n")).is_empty());
        let got = progresses(parser.finish());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].frame, Some(9));
    }
}
