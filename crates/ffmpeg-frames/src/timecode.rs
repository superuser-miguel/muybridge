//! Timecodes as the user types them, and as ffmpeg accepts them.
//!
//! ffmpeg's `-ss`/`-to` take `[HH:]MM:SS[.m...]` or a bare seconds count. We
//! accept both on input and always hand ffmpeg the fully-qualified
//! `HH:MM:SS.mmm` form, so what the entry shows is what the command runs.

use std::fmt;

/// A position in a video, held as seconds so ranges and frame counts are
/// arithmetic rather than string surgery.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Timecode {
    seconds: f64,
}

impl Timecode {
    pub fn from_seconds(seconds: f64) -> Self {
        Self { seconds }
    }

    pub fn seconds(self) -> f64 {
        self.seconds
    }

    /// Parse `HH:MM:SS.mmm`, `MM:SS.mmm` or plain `SS.mmm`.
    ///
    /// Only the last component may be fractional — `1:30.5` is a minute and a
    /// half, `1.5:30` is nonsense and is rejected rather than guessed at.
    pub fn parse(text: &str) -> Result<Self, TimecodeError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(TimecodeError::Empty);
        }
        if text.starts_with('-') {
            return Err(TimecodeError::Negative);
        }

        let parts: Vec<&str> = text.split(':').collect();
        if parts.len() > 3 {
            return Err(TimecodeError::TooManyParts);
        }

        let mut seconds = 0.0_f64;
        let last = parts.len() - 1;
        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                return Err(TimecodeError::Malformed);
            }
            let value: f64 = if i == last {
                part.parse().map_err(|_| TimecodeError::Malformed)?
            } else {
                // Whole hours/minutes only.
                part.parse::<u32>()
                    .map_err(|_| TimecodeError::Malformed)? as f64
            };
            if !value.is_finite() || value < 0.0 {
                return Err(TimecodeError::Malformed);
            }
            seconds = seconds * 60.0 + value;
        }

        Ok(Self { seconds })
    }

    /// The `HH:MM:SS.mmm` form handed to ffmpeg. Millisecond precision — finer
    /// than any frame interval this app can be asked for.
    pub fn format(self) -> String {
        let total_ms = (self.seconds * 1000.0).round().max(0.0) as u64;
        let ms = total_ms % 1000;
        let total_secs = total_ms / 1000;
        let (h, m, s) = (total_secs / 3600, (total_secs / 60) % 60, total_secs % 60);
        format!("{h:02}:{m:02}:{s:02}.{ms:03}")
    }

    /// Short `H:MM:SS` / `M:SS` form for status text, where milliseconds are noise.
    pub fn format_short(self) -> String {
        let total = self.seconds.max(0.0).round() as u64;
        let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
        if h > 0 {
            format!("{h}:{m:02}:{s:02}")
        } else {
            format!("{m}:{s:02}")
        }
    }
}

impl fmt::Display for Timecode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimecodeError {
    Empty,
    Negative,
    TooManyParts,
    Malformed,
}

impl fmt::Display for TimecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::Empty => "Enter a time like 00:17:11.448",
            Self::Negative => "A time cannot be negative",
            Self::TooManyParts => "Too many parts — use HH:MM:SS.mmm",
            Self::Malformed => "Not a valid time — use HH:MM:SS.mmm, MM:SS or seconds",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for TimecodeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_full_form_from_the_reference_command() {
        assert_eq!(
            Timecode::parse("00:17:11.448").unwrap().seconds(),
            17.0 * 60.0 + 11.448
        );
        assert_eq!(
            Timecode::parse("00:38:54.374").unwrap().seconds(),
            38.0 * 60.0 + 54.374
        );
    }

    #[test]
    fn parses_short_forms() {
        assert_eq!(Timecode::parse("90").unwrap().seconds(), 90.0);
        assert_eq!(Timecode::parse("1:30").unwrap().seconds(), 90.0);
        assert_eq!(Timecode::parse("1:00:00").unwrap().seconds(), 3600.0);
        assert_eq!(Timecode::parse(" 2:03.5 ").unwrap().seconds(), 123.5);
    }

    #[test]
    fn round_trips_through_ffmpeg_form() {
        let tc = Timecode::parse("00:17:11.448").unwrap();
        assert_eq!(tc.format(), "00:17:11.448");
        assert_eq!(Timecode::parse(&tc.format()).unwrap(), tc);
    }

    #[test]
    fn rejects_garbage_rather_than_guessing() {
        for bad in ["", "  ", "-5", "1:2:3:4", "abc", "1.5:30", "1::2"] {
            assert!(Timecode::parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn short_form_drops_milliseconds() {
        assert_eq!(Timecode::from_seconds(1303.448).format_short(), "21:43");
        assert_eq!(Timecode::from_seconds(3723.0).format_short(), "1:02:03");
    }
}
