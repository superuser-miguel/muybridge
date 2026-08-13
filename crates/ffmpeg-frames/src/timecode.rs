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

/// One component of a timecode, for stepping it with the arrow keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Hours,
    Minutes,
    Seconds,
    Millis,
}

impl Field {
    /// What one step of this component is worth, in seconds.
    pub fn unit(self) -> f64 {
        match self {
            Self::Hours => 3600.0,
            Self::Minutes => 60.0,
            Self::Seconds => 1.0,
            Self::Millis => 0.001,
        }
    }
}

/// Which component of `text` a cursor sitting `cursor` characters in belongs to.
///
/// Read from the right, because that is the only end that means the same thing
/// in every form a person might type: the last component is always seconds,
/// whether the text is `00:17:11.448`, `17:11` or `11`.
///
/// A cursor on a separator counts as being in the component *before* it, which
/// is what makes stepping repeatable — see [`step_at_cursor`].
pub fn field_at_cursor(text: &str, cursor: usize) -> Option<Field> {
    // Character and byte offsets have to agree for the arithmetic below, and a
    // timecode that isn't ASCII will not parse anyway.
    if text.trim().is_empty() || !text.is_ascii() {
        return None;
    }

    let mut ends: Vec<usize> = text.match_indices(':').map(|(i, _)| i).collect();
    ends.push(text.len());
    if ends.len() > 3 {
        return None;
    }

    let last = ends.len() - 1;
    let index = ends.iter().position(|end| cursor <= *end).unwrap_or(last);

    // Only the final component carries a fraction.
    if index == last {
        if let Some(dot) = text.rfind('.') {
            if cursor > dot {
                return Some(Field::Millis);
            }
        }
    }

    Some(match last - index {
        0 => Field::Seconds,
        1 => Field::Minutes,
        _ => Field::Hours,
    })
}

/// Step one component of a timecode up or down — what an arrow key in the Start
/// or End row does.
///
/// Returns the new value together with where the cursor belongs in its
/// formatted form, so holding an arrow down keeps stepping the *same*
/// component instead of wandering as the text is rewritten. Never goes
/// negative; the caller clamps to the end of the video, which is the only other
/// bound and the only one this crate cannot know.
pub fn step_at_cursor(text: &str, cursor: usize, steps: i32) -> Option<(Timecode, usize)> {
    let field = field_at_cursor(text, cursor)?;
    let current = Timecode::parse(text).ok()?;

    let seconds = (current.seconds() + f64::from(steps) * field.unit()).max(0.0);
    let stepped = Timecode::from_seconds(seconds);
    let caret = cursor_for_field(&stepped.format(), field);
    Some((stepped, caret))
}

/// Where to put the cursor so the next press finds the same component. Derived
/// from the formatted string rather than assumed, because a video long enough
/// to need three-digit hours would shift every offset along.
fn cursor_for_field(formatted: &str, field: Field) -> usize {
    let colons: Vec<usize> = formatted.match_indices(':').map(|(i, _)| i).collect();
    let dot = formatted.rfind('.').unwrap_or(formatted.len());
    match field {
        Field::Hours => colons.first().copied().unwrap_or(0),
        Field::Minutes => colons.get(1).copied().unwrap_or(dot),
        Field::Seconds => dot,
        Field::Millis => formatted.len(),
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

    // ------------------------------------------------ arrow-key stepping

    const FULL: &str = "01:23:45.678";
    //                  0123456789..     colons at 2 and 5, dot at 8

    #[test]
    fn the_cursor_picks_out_the_component_it_is_on() {
        for (cursor, expected) in [
            (0, Field::Hours),
            (1, Field::Hours),
            (2, Field::Hours), // on the separator that ends hours
            (3, Field::Minutes),
            (5, Field::Minutes),
            (6, Field::Seconds),
            (8, Field::Seconds), // on the decimal point
            (9, Field::Millis),
            (12, Field::Millis),
        ] {
            assert_eq!(field_at_cursor(FULL, cursor), Some(expected), "at {cursor}");
        }
    }

    /// Whatever a person actually typed, the rightmost component is seconds.
    #[test]
    fn shorter_forms_are_read_from_the_right() {
        assert_eq!(field_at_cursor("17:11", 0), Some(Field::Minutes));
        assert_eq!(field_at_cursor("17:11", 4), Some(Field::Seconds));
        assert_eq!(field_at_cursor("90", 1), Some(Field::Seconds));
        assert_eq!(field_at_cursor("2:03.5", 5), Some(Field::Millis));
    }

    #[test]
    fn nothing_to_step_is_not_an_error() {
        assert_eq!(field_at_cursor("", 0), None);
        assert_eq!(field_at_cursor("   ", 1), None);
        assert_eq!(field_at_cursor("1:2:3:4", 0), None);
        assert_eq!(step_at_cursor("nonsense", 2, 1), None);
        // A cursor past the end of the text lands in the last component.
        assert_eq!(field_at_cursor(FULL, 99), Some(Field::Millis));
    }

    #[test]
    fn each_component_steps_by_its_own_unit() {
        let at = |cursor| step_at_cursor(FULL, cursor, 1).unwrap().0.format();
        assert_eq!(at(1), "02:23:45.678"); // hours
        assert_eq!(at(4), "01:24:45.678"); // minutes
        assert_eq!(at(7), "01:23:46.678"); // seconds
        assert_eq!(at(10), "01:23:45.679"); // milliseconds
    }

    #[test]
    fn stepping_down_carries_between_components() {
        // 01:23:45.678 minus a minute of minutes' worth, and a borrow.
        let (value, _) = step_at_cursor("01:00:45.678", 4, -1).unwrap();
        assert_eq!(value.format(), "00:59:45.678");
    }

    /// The reason the cursor comes back at all: hold Up on the minutes and it
    /// has to still be on the minutes for the next press.
    #[test]
    fn the_cursor_stays_in_the_component_it_stepped() {
        let mut text = String::from("00:00:00.000");
        let mut cursor = 4; // minutes
        for expected in ["00:01:00.000", "00:02:00.000", "00:03:00.000"] {
            let (value, caret) = step_at_cursor(&text, cursor, 1).unwrap();
            text = value.format();
            cursor = caret;
            assert_eq!(text, expected);
            assert_eq!(field_at_cursor(&text, cursor), Some(Field::Minutes));
        }
    }

    #[test]
    fn a_timecode_never_steps_below_zero() {
        let (value, _) = step_at_cursor("00:00:30.000", 1, -1).unwrap();
        assert_eq!(value.seconds(), 0.0);
    }

    /// Page Up/Down hand in ten at a time — ten minutes a press is what makes a
    /// two-hour video navigable.
    #[test]
    fn a_bigger_step_is_just_more_of_the_same_unit() {
        let (value, _) = step_at_cursor("00:00:00.000", 4, 10).unwrap();
        assert_eq!(value.format(), "00:10:00.000");
    }

    /// Long videos push the hours out to three digits; the cursor has to follow
    /// rather than sit at a hard-coded offset.
    #[test]
    fn the_cursor_follows_a_widening_hours_field() {
        let (value, caret) = step_at_cursor("99:00:00.000", 1, 1).unwrap();
        assert_eq!(value.format(), "100:00:00.000");
        assert_eq!(field_at_cursor(&value.format(), caret), Some(Field::Hours));
    }
}
