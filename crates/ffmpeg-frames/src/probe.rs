//! Asking ffprobe what a video *is*, so the UI can show duration, size and
//! source frame rate — and so a frame-count estimate has a duration to work
//! from.
//!
//! We use ffprobe's flat `key=value` output rather than JSON: it needs no
//! parser dependency and the whole contract fits in the test at the bottom of
//! this file.

use std::ffi::OsString;
use std::path::Path;

use crate::timecode::Timecode;

/// What ffprobe reports about the first video stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Probe {
    pub duration: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codec: Option<String>,
    /// Source frame rate, already divided out of ffprobe's `30000/1001` form.
    pub frame_rate: Option<f64>,
}

impl Probe {
    /// One-line summary for the details row: `1080 × 1920 · h264 · 29.97 fps · 21:43`.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let (Some(w), Some(h)) = (self.width, self.height) {
            parts.push(format!("{w} × {h}"));
        }
        if let Some(codec) = &self.codec {
            parts.push(codec.clone());
        }
        if let Some(rate) = self.frame_rate {
            // Two decimals is how frame rates are spoken about: 29.97, not
            // 29.97003. The full precision only matters inside a command.
            let rounded = format!("{rate:.2}");
            let rounded = rounded.trim_end_matches('0').trim_end_matches('.');
            parts.push(format!("{rounded} fps"));
        }
        if let Some(duration) = self.duration {
            parts.push(Timecode::from_seconds(duration).format_short());
        }
        if parts.is_empty() {
            "Unknown".to_string()
        } else {
            parts.join(" · ")
        }
    }
}

/// `ffprobe -v error -select_streams v:0 -show_entries … -of default=noprint_wrappers=1 INPUT`
pub fn probe_argv(input: &Path) -> Vec<OsString> {
    let mut argv: Vec<OsString> = [
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height,codec_name,avg_frame_rate:format=duration",
        "-of",
        "default=noprint_wrappers=1",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    argv.push(input.as_os_str().to_os_string());
    argv
}

/// Parse the flat output of [`probe_argv`]. Unknown keys and `N/A` values are
/// ignored rather than treated as errors — a video missing one field should
/// still report the others.
pub fn parse_probe(text: &str) -> Probe {
    let mut probe = Probe::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() || value == "N/A" {
            continue;
        }
        match key.trim() {
            "width" => probe.width = value.parse().ok(),
            "height" => probe.height = value.parse().ok(),
            "codec_name" => probe.codec = Some(value.to_string()),
            "avg_frame_rate" => probe.frame_rate = parse_rational(value),
            "duration" => probe.duration = value.parse().ok().filter(|d: &f64| *d > 0.0),
            _ => {}
        }
    }
    probe
}

/// `30000/1001` → `29.97`. A zero denominator (ffprobe's "unknown") yields None.
fn parse_rational(value: &str) -> Option<f64> {
    let (num, den) = value.split_once('/')?;
    let num: f64 = num.trim().parse().ok()?;
    let den: f64 = den.trim().parse().ok()?;
    if den == 0.0 || num <= 0.0 {
        return None;
    }
    Some(num / den)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
codec_name=h264
width=1080
height=1920
avg_frame_rate=30000/1001
duration=1303.448000
";

    #[test]
    fn parses_a_real_ffprobe_block() {
        let probe = parse_probe(SAMPLE);
        assert_eq!(probe.codec.as_deref(), Some("h264"));
        assert_eq!(probe.width, Some(1080));
        assert_eq!(probe.height, Some(1920));
        assert_eq!(probe.duration, Some(1303.448));
        let rate = probe.frame_rate.unwrap();
        assert!((rate - 29.97).abs() < 0.01, "{rate}");
    }

    #[test]
    fn missing_and_na_fields_are_dropped_not_faked() {
        let probe = parse_probe("codec_name=vp9\nwidth=N/A\navg_frame_rate=0/0\nduration=0\n");
        assert_eq!(probe.codec.as_deref(), Some("vp9"));
        assert_eq!(probe.width, None);
        assert_eq!(probe.frame_rate, None);
        assert_eq!(probe.duration, None);
    }

    #[test]
    fn summary_reads_as_one_line() {
        assert_eq!(
            parse_probe(SAMPLE).summary(),
            "1080 × 1920 · h264 · 29.97 fps · 21:43"
        );
        assert_eq!(Probe::default().summary(), "Unknown");
    }

    #[test]
    fn probe_argv_ends_with_the_input_path() {
        let argv = probe_argv(Path::new("/videos/clip.mp4"));
        assert_eq!(argv.last().unwrap(), "/videos/clip.mp4");
        assert!(argv.iter().any(|a| a == "-select_streams"));
    }
}
