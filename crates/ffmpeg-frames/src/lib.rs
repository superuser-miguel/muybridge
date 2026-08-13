//! The engine behind Muybridge: everything about *what ffmpeg gets asked to
//! do*, with no GTK anywhere in sight. Never add a UI dependency to this crate
//! — the whole point is that the command contract is testable on its own.
//!
//! The app builds exactly one kind of command line, the one the project was
//! started from:
//!
//! ```text
//! ffmpeg -i INPUT -vf fps=9.99 OUTDIR/NAME_%04d.png
//! ffmpeg -ss 00:17:11.448 -to 00:38:54.374 -i INPUT -vf fps=3.33 OUTDIR/NAME_%04d_test.png
//! ```
//!
//! [`Job::build_argv`] is the single source of truth for it. Two flags are
//! added that a GUI cannot do without: `-nostdin` (so ffmpeg never blocks
//! waiting on a terminal that isn't there) and `-progress pipe:1 -nostats`
//! (machine-readable progress instead of the redrawing status line). Overwrite
//! is always stated explicitly as `-y` or `-n`, because the interactive
//! "File exists. Overwrite?" prompt would hang the app forever.
//!
//! One other command exists, and only one: [`preview::thumbnail_argv`], which
//! reads a single frame back on stdout so the range can be picked by eye
//! instead of typed. It writes nothing and is built from the same `-ss`-before
//! `-i` seek as the real job, so what the preview shows is what an extraction
//! starting there would produce.
//!
//! Commands are spawned as an argv vector, never as a shell string, so a
//! filename with a space, a quote or a `$` in it is just a filename.

pub mod preview;
pub mod probe;
pub mod progress;
pub mod timecode;

pub use preview::{thumbnail_argv, tile_times, Scale};
pub use probe::{parse_probe, probe_argv, Probe};
pub use progress::{Event, Progress, StreamParser};
pub use timecode::{field_at_cursor, step_at_cursor, Field, Timecode, TimecodeError};

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

/// Output image format. PNG is lossless and the default; JPEG is there for
/// when a few thousand frames of PNG is more disk than the job is worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Png,
    Jpeg,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
        }
    }

    /// Whether the quality setting applies (ffmpeg's `-q:v`).
    pub fn is_lossy(self) -> bool {
        matches!(self, Self::Jpeg)
    }

    /// Index order used by the format combo row in the UI.
    pub fn from_index(index: u32) -> Self {
        match index {
            1 => Self::Jpeg,
            _ => Self::Png,
        }
    }

    pub fn index(self) -> u32 {
        match self {
            Self::Png => 0,
            Self::Jpeg => 1,
        }
    }
}

/// One configured extraction.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub input: PathBuf,
    /// Trim start. Placed *before* `-i` so ffmpeg seeks the input rather than
    /// decoding-and-discarding everything up to that point.
    pub start: Option<Timecode>,
    /// Trim end, an absolute position in the source (not a duration).
    pub end: Option<Timecode>,
    /// Frames to sample per second of video — the `fps=` filter value.
    pub fps: f64,
    pub output_dir: PathBuf,
    /// Base name for the written frames, before the counter.
    pub stem: String,
    /// Optional tail after the counter: `NAME_0001_test.png`.
    pub suffix: String,
    /// Counter width: 4 gives `%04d`.
    pub digits: u8,
    pub format: Format,
    /// ffmpeg `-q:v` for lossy formats: 2 is best, 31 is smallest.
    pub quality: u8,
    pub overwrite: bool,
}

impl Job {
    /// A job with the defaults the UI starts from.
    pub fn new(input: impl Into<PathBuf>, output_dir: impl Into<PathBuf>) -> Self {
        let input = input.into();
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            input,
            start: None,
            end: None,
            fps: 3.33,
            output_dir: output_dir.into(),
            stem,
            suffix: String::new(),
            digits: 4,
            format: Format::Png,
            quality: 3,
            overwrite: true,
        }
    }

    /// The filename ffmpeg is given, counter token included: `NAME_%04d.png`.
    pub fn file_pattern(&self) -> OsString {
        let mut name = OsString::from(&self.stem);
        name.push(format!("_%0{}d", self.digits));
        name.push(&self.suffix);
        name.push(".");
        name.push(self.format.extension());
        name
    }

    /// Full output path pattern handed to ffmpeg.
    pub fn output_pattern(&self) -> PathBuf {
        self.output_dir.join(self.file_pattern())
    }

    /// What the first written frame will be called — for showing the user
    /// before anything runs.
    pub fn example_filename(&self) -> String {
        format!(
            "{}_{:0width$}{}.{}",
            self.stem,
            1,
            self.suffix,
            self.format.extension(),
            width = self.digits as usize,
        )
    }

    /// The one and only ffmpeg command line.
    pub fn build_argv(&self) -> Vec<OsString> {
        let mut argv: Vec<OsString> = Vec::new();
        let mut push = |s: &str| argv.push(OsString::from(s));

        push("-hide_banner");
        push("-nostdin");
        push(if self.overwrite { "-y" } else { "-n" });

        // Input options: seeking before -i is the fast path, and -to is then
        // an absolute position in the source, matching the reference command.
        if let Some(start) = self.start {
            push("-ss");
            push(&start.format());
        }
        if let Some(end) = self.end {
            push("-to");
            push(&end.format());
        }

        argv.push(OsString::from("-i"));
        argv.push(self.input.as_os_str().to_os_string());

        argv.push(OsString::from("-vf"));
        argv.push(OsString::from(format!("fps={}", format_number(self.fps))));

        if self.format.is_lossy() {
            argv.push(OsString::from("-q:v"));
            argv.push(OsString::from(self.quality.to_string()));
        }

        // Machine-readable progress on stdout instead of the redrawing
        // terminal status line; see the progress module.
        argv.push(OsString::from("-progress"));
        argv.push(OsString::from("pipe:1"));
        argv.push(OsString::from("-nostats"));

        argv.push(self.output_pattern().into_os_string());
        argv
    }

    /// `ffprobe` arguments for this job's input.
    pub fn probe_argv(&self) -> Vec<OsString> {
        probe_argv(&self.input)
    }

    /// Length of the selected span in seconds, given the video's full duration
    /// (which may be unknown). None when it cannot be worked out.
    pub fn range_seconds(&self, total_duration: Option<f64>) -> Option<f64> {
        let start = self.start.map(Timecode::seconds).unwrap_or(0.0);
        let end = self
            .end
            .map(Timecode::seconds)
            .or(total_duration)
            .filter(|e| *e > 0.0)?;
        let span = end - start;
        (span > 0.0).then_some(span)
    }

    /// Roughly how many frames this will write. Approximate by nature: the
    /// exact count depends on where the source's frames actually fall.
    pub fn estimated_frames(&self, total_duration: Option<f64>) -> Option<u64> {
        let span = self.range_seconds(total_duration)?;
        let frames = (span * self.fps).round();
        (frames >= 1.0).then_some(frames as u64)
    }

    /// Catch what would otherwise become a confusing ffmpeg error, or worse, a
    /// command that silently does the wrong thing.
    pub fn validate(&self) -> Result<(), JobError> {
        if self.input.as_os_str().is_empty() {
            return Err(JobError::NoInput);
        }
        if self.output_dir.as_os_str().is_empty() {
            return Err(JobError::NoOutputDir);
        }
        if self.stem.trim().is_empty() {
            return Err(JobError::EmptyName);
        }
        // A '%' in the name would be read by ffmpeg as another counter token.
        if self.stem.contains('%') || self.suffix.contains('%') {
            return Err(JobError::PercentInName);
        }
        if self.stem.contains('/') || self.suffix.contains('/') {
            return Err(JobError::SlashInName);
        }
        if !self.fps.is_finite() || self.fps <= 0.0 {
            return Err(JobError::BadFps);
        }
        if self.digits == 0 || self.digits > 9 {
            return Err(JobError::BadDigits);
        }
        if let (Some(start), Some(end)) = (self.start, self.end) {
            if end.seconds() <= start.seconds() {
                return Err(JobError::RangeInverted);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobError {
    NoInput,
    NoOutputDir,
    EmptyName,
    PercentInName,
    SlashInName,
    BadFps,
    BadDigits,
    RangeInverted,
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::NoInput => "Choose a video first",
            Self::NoOutputDir => "Choose an output folder",
            Self::EmptyName => "Give the frames a name",
            Self::PercentInName => "The name cannot contain “%” — ffmpeg reads it as a counter",
            Self::SlashInName => "The name cannot contain “/”",
            Self::BadFps => "Frames per second must be greater than zero",
            Self::BadDigits => "Counter digits must be between 1 and 9",
            Self::RangeInverted => "The end time must come after the start time",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for JobError {}

/// Format a float the way a person would write it: `9.99`, `3.33`, `30`, not
/// `9.9900000000000002`. Used for the `fps=` filter value, where the string is
/// part of the command contract.
pub fn format_number(value: f64) -> String {
    let mut s = format!("{value:.6}");
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    s
}

/// Display form of a command line, for logs and the “what will run” tooltip.
/// Not a shell string — arguments are quoted only so they read clearly.
pub fn argv_display(program: &str, argv: &[OsString]) -> String {
    let mut out = String::from(program);
    for arg in argv {
        let text = arg.to_string_lossy();
        out.push(' ');
        if text.contains(' ') || text.contains('\'') {
            out.push_str(&format!("\"{text}\""));
        } else {
            out.push_str(&text);
        }
    }
    out
}

/// Convenience for callers that need the input path's stem as a default name.
pub fn default_stem(input: &Path) -> String {
    input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn as_strs(argv: &[OsString]) -> Vec<&str> {
        argv.iter().map(|a| a.to_str().unwrap()).collect()
    }

    /// The first reference command:
    /// `ffmpeg -i .../3454287235535885570.mp4 -vf fps=9.99 .../3454287235535885570_%04d.png`
    #[test]
    fn reproduces_the_whole_video_reference_command() {
        let mut job = Job::new(
            "/home/definitive_group/Pictures/Instagram/__nadiag/3454287235535885570.mp4",
            "/home/definitive_group/Pictures/VlcSnapshots",
        );
        job.fps = 9.99;

        assert_eq!(
            as_strs(&job.build_argv()),
            [
                "-hide_banner",
                "-nostdin",
                "-y",
                "-i",
                "/home/definitive_group/Pictures/Instagram/__nadiag/3454287235535885570.mp4",
                "-vf",
                "fps=9.99",
                "-progress",
                "pipe:1",
                "-nostats",
                "/home/definitive_group/Pictures/VlcSnapshots/3454287235535885570_%04d.png",
            ]
        );
    }

    /// The second reference command, trimmed and suffixed:
    /// `ffmpeg -ss 00:17:11.448 -to 00:38:54.374 -i … -vf fps=3.33 …_%04d_test.png`
    #[test]
    fn reproduces_the_trimmed_reference_command() {
        let mut job = Job::new(
            "/home/definitive_group/Pictures/Instagram/veradijkmansofficial/3676914608518943927.mp4",
            "/home/definitive_group/Pictures/VlcSnapshots",
        );
        job.start = Some(Timecode::parse("00:17:11.448").unwrap());
        job.end = Some(Timecode::parse("00:38:54.374").unwrap());
        job.fps = 3.33;
        job.suffix = "_test".to_string();

        let built = job.build_argv();
        assert_eq!(
            as_strs(&built),
            [
                "-hide_banner",
                "-nostdin",
                "-y",
                "-ss",
                "00:17:11.448",
                "-to",
                "00:38:54.374",
                "-i",
                "/home/definitive_group/Pictures/Instagram/veradijkmansofficial/3676914608518943927.mp4",
                "-vf",
                "fps=3.33",
                "-progress",
                "pipe:1",
                "-nostats",
                "/home/definitive_group/Pictures/VlcSnapshots/3676914608518943927_%04d_test.png",
            ]
        );
    }

    /// Seeking must stay in front of `-i`: after it, ffmpeg decodes and throws
    /// away everything before the start instead of seeking to it.
    #[test]
    fn seek_options_precede_the_input() {
        let mut job = Job::new("/v/clip.mp4", "/out");
        job.start = Some(Timecode::from_seconds(10.0));
        job.end = Some(Timecode::from_seconds(20.0));
        let built = job.build_argv();
        let argv = as_strs(&built);
        let i = argv.iter().position(|a| *a == "-i").unwrap();
        assert!(argv.iter().position(|a| *a == "-ss").unwrap() < i);
        assert!(argv.iter().position(|a| *a == "-to").unwrap() < i);
    }

    #[test]
    fn overwrite_is_always_explicit_so_ffmpeg_never_prompts() {
        let mut job = Job::new("/v/clip.mp4", "/out");
        assert!(as_strs(&job.build_argv()).contains(&"-y"));
        job.overwrite = false;
        let built = job.build_argv();
        let argv = as_strs(&built);
        assert!(argv.contains(&"-n"));
        assert!(!argv.contains(&"-y"));
    }

    #[test]
    fn quality_only_applies_to_lossy_formats() {
        let mut job = Job::new("/v/clip.mp4", "/out");
        assert!(!as_strs(&job.build_argv()).contains(&"-q:v"));

        job.format = Format::Jpeg;
        job.quality = 5;
        let built = job.build_argv();
        let argv = as_strs(&built);
        let q = argv.iter().position(|a| *a == "-q:v").unwrap();
        assert_eq!(argv[q + 1], "5");
        assert!(argv.last().unwrap().ends_with("_%04d.jpg"));
    }

    #[test]
    fn digits_set_the_counter_width() {
        let mut job = Job::new("/v/clip.mp4", "/out");
        job.digits = 6;
        assert_eq!(job.file_pattern().to_str().unwrap(), "clip_%06d.png");
        assert_eq!(job.example_filename(), "clip_000001.png");
    }

    #[test]
    fn stem_defaults_to_the_video_name() {
        let job = Job::new("/v/3454287235535885570.mp4", "/out");
        assert_eq!(job.stem, "3454287235535885570");
        assert_eq!(job.example_filename(), "3454287235535885570_0001.png");
    }

    #[test]
    fn paths_with_spaces_stay_single_argv_entries() {
        let job = Job::new("/v/my clip 'one'.mp4", "/out dir");
        let argv = job.build_argv();
        assert!(argv.iter().any(|a| a == "/v/my clip 'one'.mp4"));
        assert!(argv
            .iter()
            .any(|a| a == "/out dir/my clip 'one'_%04d.png"));
    }

    #[test]
    fn estimates_frames_from_the_selected_span() {
        let mut job = Job::new("/v/clip.mp4", "/out");
        job.fps = 3.33;
        // Whole video: 1303.448s at 3.33 fps.
        assert_eq!(job.estimated_frames(Some(1303.448)), Some(4340));
        // Trimmed: the reference command's 21m43s span.
        job.start = Some(Timecode::parse("00:17:11.448").unwrap());
        job.end = Some(Timecode::parse("00:38:54.374").unwrap());
        assert_eq!(job.range_seconds(None).map(|s| s.round()), Some(1303.0));
        assert_eq!(job.estimated_frames(None), Some(4339));
        // Unknown duration and no end: no estimate rather than a made-up one.
        job.end = None;
        assert_eq!(job.estimated_frames(None), None);
    }

    #[test]
    fn validation_catches_what_ffmpeg_would_only_complain_about_later() {
        let good = Job::new("/v/clip.mp4", "/out");
        assert!(good.validate().is_ok());

        let mut job = good.clone();
        job.stem = "100%".to_string();
        assert_eq!(job.validate(), Err(JobError::PercentInName));

        let mut job = good.clone();
        job.stem = "  ".to_string();
        assert_eq!(job.validate(), Err(JobError::EmptyName));

        let mut job = good.clone();
        job.suffix = "a/b".to_string();
        assert_eq!(job.validate(), Err(JobError::SlashInName));

        let mut job = good.clone();
        job.fps = 0.0;
        assert_eq!(job.validate(), Err(JobError::BadFps));

        let mut job = good.clone();
        job.start = Some(Timecode::from_seconds(30.0));
        job.end = Some(Timecode::from_seconds(10.0));
        assert_eq!(job.validate(), Err(JobError::RangeInverted));

        let mut job = good;
        job.input = PathBuf::new();
        assert_eq!(job.validate(), Err(JobError::NoInput));
    }

    #[test]
    fn fps_is_written_the_way_a_person_writes_it() {
        assert_eq!(format_number(9.99), "9.99");
        assert_eq!(format_number(3.33), "3.33");
        assert_eq!(format_number(30.0), "30");
        assert_eq!(format_number(29.97002997002997), "29.97003");
    }

    #[test]
    fn argv_display_quotes_only_what_needs_it() {
        let job = Job::new("/v/my clip.mp4", "/out");
        let shown = argv_display("ffmpeg", &job.build_argv());
        assert!(shown.starts_with("ffmpeg -hide_banner"));
        assert!(shown.contains("\"/v/my clip.mp4\""));
        assert!(shown.contains("-vf fps=3.33"));
    }
}
