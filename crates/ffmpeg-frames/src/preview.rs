//! Pulling a single still out of a video, for the scrub preview and the
//! filmstrip under the range controls.
//!
//! Same trick as the extraction itself — spawn ffmpeg, read what it writes —
//! only here the frame comes back on **stdout** instead of going to disk:
//!
//! ```text
//! ffmpeg -ss 00:00:42.000 -i INPUT -frames:v 1 -an -vf scale=… -f image2pipe -c:v png -
//! ```
//!
//! Two things make this cheap enough to do while a handle is being dragged.
//! `-ss` sits *before* `-i`, so ffmpeg seeks through the container index rather
//! than decoding up to the mark: grabbing a frame eighteen minutes in costs the
//! same as grabbing one at three seconds — measured at ~200 ms either way on a
//! 1080p h264 file. And the scale filter shrinks the frame inside ffmpeg, so
//! what crosses the pipe is a thumbnail, not a full-size image.
//!
//! The muxer and encoder used here — `image2pipe` and `png` — are the two the
//! bundled ffmpeg is already built with for the extraction proper, so the
//! preview costs nothing in the manifest.
//!
//! Because seeking is accurate (ffmpeg decodes forward from the keyframe to the
//! requested position), the frame shown is the frame that a job starting there
//! would write. A preview that disagreed with the output would be worse than no
//! preview at all.

use std::ffi::OsString;
use std::path::Path;

use crate::timecode::Timecode;

/// Height of one filmstrip tile, in pixels. Width follows the source's aspect.
pub const TILE_HEIGHT: u32 = 72;

/// Box the large scrub preview is fitted inside. Portrait video is common
/// enough (phone clips are the reason this app exists) that bounding only the
/// width would hand back something 1138 pixels tall.
pub const PREVIEW_BOX: (u32, u32) = (640, 360);

/// How ffmpeg should size the frame on its way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    /// Fit inside a box without cropping: neither side exceeds it, aspect kept.
    Fit { width: u32, height: u32 },
    /// Fixed height, width follows the source aspect.
    Height(u32),
}

impl Scale {
    /// The `-vf` value. `-2` rather than `-1` for the free dimension keeps it
    /// even, which some encoders insist on and none object to.
    pub fn filter(self) -> String {
        match self {
            Self::Fit { width, height } => {
                format!("scale=w={width}:h={height}:force_original_aspect_ratio=decrease")
            }
            Self::Height(height) => format!("scale=-2:{height}"),
        }
    }
}

/// Arguments for grabbing the frame at `seconds` as a PNG on stdout.
///
/// stderr must **not** be merged into stdout by the caller: the payload here is
/// binary, and one line of ffmpeg chatter in the middle of it is a corrupt
/// image. `-v error` keeps that chatter down to what a failure needs anyway.
pub fn thumbnail_argv(input: &Path, seconds: f64, scale: Scale) -> Vec<OsString> {
    let at = Timecode::from_seconds(seconds.max(0.0));
    let mut argv: Vec<OsString> = Vec::new();
    let mut push = |s: &str| argv.push(OsString::from(s));

    push("-hide_banner");
    push("-nostdin");
    push("-v");
    push("error");

    // Before -i: seek the input rather than decode-and-discard up to it.
    push("-ss");
    push(&at.format());

    argv.push(OsString::from("-i"));
    argv.push(input.as_os_str().to_os_string());

    // image2pipe drops audio on its own, but saying so costs nothing and means
    // the command does not depend on that.
    for arg in ["-frames:v", "1", "-an", "-vf"] {
        argv.push(OsString::from(arg));
    }
    argv.push(OsString::from(scale.filter()));
    for arg in ["-f", "image2pipe", "-c:v", "png", "-"] {
        argv.push(OsString::from(arg));
    }

    argv
}

/// When to sample each of `count` filmstrip tiles across a video of `duration`.
///
/// Tiles sit at the *middle* of the slice they represent, not its start: the
/// first tile of a video that opens on black is otherwise always black, and the
/// last one lands past the final frame and comes back empty.
pub fn tile_times(duration: f64, count: usize) -> Vec<f64> {
    // `is_finite` first: a NaN duration fails every comparison silently, and an
    // infinite one would hand back infinite timestamps.
    if !duration.is_finite() || duration <= 0.0 || count == 0 {
        return Vec::new();
    }
    (0..count)
        .map(|i| duration * (i as f64 + 0.5) / count as f64)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn as_strs(argv: &[OsString]) -> Vec<&str> {
        argv.iter().map(|a| a.to_str().unwrap()).collect()
    }

    #[test]
    fn builds_the_measured_single_frame_command() {
        let argv = thumbnail_argv(
            Path::new("/videos/clip.mp4"),
            42.0,
            Scale::Fit {
                width: 640,
                height: 360,
            },
        );
        assert_eq!(
            as_strs(&argv),
            [
                "-hide_banner",
                "-nostdin",
                "-v",
                "error",
                "-ss",
                "00:00:42.000",
                "-i",
                "/videos/clip.mp4",
                "-frames:v",
                "1",
                "-an",
                "-vf",
                "scale=w=640:h=360:force_original_aspect_ratio=decrease",
                "-f",
                "image2pipe",
                "-c:v",
                "png",
                "-",
            ]
        );
    }

    /// The whole reason a scrub is affordable: seeking in front of the input.
    #[test]
    fn seek_precedes_the_input() {
        let argv = thumbnail_argv(Path::new("/v/clip.mp4"), 1200.0, Scale::Height(TILE_HEIGHT));
        let argv = as_strs(&argv);
        let i = argv.iter().position(|a| *a == "-i").unwrap();
        assert!(argv.iter().position(|a| *a == "-ss").unwrap() < i);
    }

    #[test]
    fn writes_to_stdout_through_the_bundled_muxer_and_encoder() {
        let argv = thumbnail_argv(Path::new("/v/clip.mp4"), 0.0, Scale::Height(72));
        let argv = as_strs(&argv);
        assert_eq!(argv.last().unwrap(), &"-");
        // Both are already in the Flatpak's trimmed ffmpeg; nothing new is
        // needed to render a preview.
        let f = argv.iter().position(|a| *a == "-f").unwrap();
        assert_eq!(argv[f + 1], "image2pipe");
        let c = argv.iter().position(|a| *a == "-c:v").unwrap();
        assert_eq!(argv[c + 1], "png");
    }

    #[test]
    fn scale_filters_keep_the_aspect_ratio() {
        assert_eq!(
            Scale::Fit {
                width: 640,
                height: 360
            }
            .filter(),
            "scale=w=640:h=360:force_original_aspect_ratio=decrease"
        );
        assert_eq!(Scale::Height(72).filter(), "scale=-2:72");
    }

    #[test]
    fn a_negative_seek_is_clamped_rather_than_passed_on() {
        let argv = thumbnail_argv(Path::new("/v/clip.mp4"), -3.0, Scale::Height(72));
        let argv = as_strs(&argv);
        let ss = argv.iter().position(|a| *a == "-ss").unwrap();
        assert_eq!(argv[ss + 1], "00:00:00.000");
    }

    #[test]
    fn tiles_sample_the_middle_of_their_slice() {
        // Never 0.0 (often black) and never the duration itself (past the end,
        // where ffmpeg exits cleanly having written nothing).
        let times = tile_times(120.0, 4);
        assert_eq!(times, vec![15.0, 45.0, 75.0, 105.0]);
        assert!(times.first().unwrap() > &0.0);
        assert!(times.last().unwrap() < &120.0);
    }

    #[test]
    fn tiles_need_a_real_duration_and_a_real_count() {
        assert!(tile_times(0.0, 12).is_empty());
        assert!(tile_times(f64::NAN, 12).is_empty());
        assert!(tile_times(120.0, 0).is_empty());
    }
}
