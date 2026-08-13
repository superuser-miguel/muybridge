# Muybridge — working plan

The spec this repo is built to. Where this document and the code disagree, this
document is the intent and the code is the bug.

## 1. What it is

A GTK4/libadwaita front end for sampling a video into a numbered sequence of
still images with ffmpeg. One job at a time, described on one page, run on one
button.

The project started from two command lines the author was typing by hand:

```sh
ffmpeg -i .../3454287235535885570.mp4 -vf fps=9.99 \
       .../VlcSnapshots/3454287235535885570_%04d.png

ffmpeg -ss 00:17:11.448 -to 00:38:54.374 -i .../3676914608518943927.mp4 \
       -vf fps=3.33 .../VlcSnapshots/3676914608518943927_%04d_test.png
```

Everything in the UI exists to write one of those, correctly, without the parts
that are easy to get wrong by hand: a mistyped timecode, a `%` in a filename, a
range that runs backwards, an overwrite prompt nobody sees.

**Non-goals.** Not a video editor, not a transcoder, not a player. No filters
beyond `fps` and (later) `scale`. If a job needs more than that, it needs
ffmpeg, not this.

## 2. The command contract

`crates/ffmpeg-frames` owns it. Exactly one function builds a command line —
`Job::build_argv` — and its tests assert both reference commands argument for
argument. Nothing else in the codebase assembles ffmpeg arguments.

```text
ffmpeg -hide_banner -nostdin (-y|-n) [-ss START] [-to END] -i INPUT
       -vf fps=RATE [-q:v Q] -progress pipe:1 -nostats OUTDIR/NAME_%0Nd[SUFFIX].EXT
```

Deviations from the hand-typed originals, each for a reason:

| Flag | Why |
| --- | --- |
| `-hide_banner` | the build banner is noise on a merged output pipe |
| `-nostdin` | without it ffmpeg reads a terminal that does not exist |
| `-y` / `-n` | always explicit: the interactive overwrite prompt would hang the app |
| `-progress pipe:1 -nostats` | parseable progress blocks instead of a redrawing status line |

`-ss`/`-to` stay **before** `-i`: that seeks the input instead of decoding and
discarding everything up to the start, and makes `-to` an absolute position in
the source, which is what the reference command means by it.

Verified against real ffmpeg 8.1.1 (2026-07-29): whole-video and trimmed runs
both produce the frame counts the estimator predicts, and `out_time` in a
trimmed run is relative to the selected span — so the progress fraction is
span-relative and correct.

**One other command exists**, `preview::thumbnail_argv`, and only for reading a
single frame back to look at:

```text
ffmpeg -hide_banner -nostdin -v error -ss AT -i INPUT
       -frames:v 1 -an -vf scale=… -f image2pipe -c:v png -
```

It writes nothing to disk, and it uses the same `-ss`-before-`-i` seek as the
real job so the frame on screen is the frame an extraction starting there would
produce. A preview that disagreed with the output would be worse than none.
`image2pipe` and `png` are already in the trimmed bundle — verified 2026-08-13
with `flatpak run --command=ffmpeg` — so this cost nothing in the manifest.

## 3. Architecture

```
crates/ffmpeg-frames    UI-free engine. Job -> argv, ffprobe argv + parser,
                        -progress stream parser, timecodes, frame estimation,
                        single-frame preview argv. NEVER add gtk to this crate.
crates/muybridge        the app. window.rs (composite template + all signal
                        handling), runner.rs (gio::Subprocess, async reads),
                        filmstrip.rs (the one self-drawing widget).
src/ui/window.blp       the entire layout. Rust builds no widget tree.
data/                   desktop entry, AppStream metainfo, icon, gresource
build-aux/              the Meson -> cargo bridge
```

Subprocess-wrapper architecture, as with the author's other tools: the UI never
links ffmpeg's libraries, it spawns the CLI and parses its output. That keeps
the engine swappable and the contract inspectable.

stdout and stderr are **merged** into one pipe so a single parser sees progress
blocks and error text in order; any line that isn't a known progress key is
kept as a message, and the last one is what a failed run reports.

The frame grab is the one exception and does the opposite: its stdout is a PNG,
so stderr is silenced outright rather than merged — one line of ffmpeg chatter
landing in the middle of the payload is a corrupt image.

## 4. The UI

One scrolling page of preference groups, in the order a job is described:

1. **Video** — the file, plus what ffprobe says about it
2. **Preview** — a frame, a filmstrip and the selected span (only once ffprobe
   has reported a duration; without one there is nothing to lay tiles along)
3. **Range** — off means the whole video; on reveals start and end
4. **Frames** — rate, format, and (for JPEG) quality
5. **Output** — folder, name, suffix, counter digits, overwrite
6. **Will write** — the first filename and the frame estimate, live

Every control feeds `current_job()`, which returns either a validated `Job` or
the one sentence explaining why it isn't ready. That sentence goes in the
"Will write" row and the Extract button greys out. There is no second path
from the widgets to the command.

While a run is on, the settings go insensitive, a progress bar and a status
line appear at the bottom, and Extract is replaced by Cancel. Cancel sends
SIGTERM: ffmpeg finishes the frame it is on and the ones already written stay.

**Preview and filmstrip.** A long video cannot be trimmed by typing timecodes
at it — you have to see where you are. Twelve thumbnails span the duration with
draggable in and out handles over them, and a larger frame above shows whatever
the last drag touched.

Decisions worth keeping:

- **ffmpeg, not GStreamer.** `gtk4paintablesink` is in the runtime (checked, it
  is what Showtime renders through) so the playback route would have bundled
  nothing either. It lost on three counts: it would preview through a *different
  decoder* than the one doing the work, it puts a media pipeline inside the UI
  process against the architecture in §3, and the feature is scrubbing, not
  playing. If real playback is ever wanted, that door is still open.
- **The strip owns nothing.** Dragging a handle writes into the Start and End
  rows exactly as typing would, and those rows remain what the job is built
  from. `sync_strip_from_rows` pushes the other way from `update_preview`, so a
  typed timecode moves the handle. The strip's setters are silent and only
  gestures call back, which is what stops the round trip looping.
- **Grabbing a handle turns trimming on.** Reaching for one *is* asking to trim;
  making the drag inert until a switch is found would be a puzzle.
- **Seeks are debounced by 120 ms and cancelled, not queued.** A drag emits
  motion far faster than a frame decodes (~200 ms), so every pixel of travel
  would otherwise spawn a process obsolete before it finished. The readout
  follows the handle immediately; only the picture waits.
- **Tiles fill sequentially**, left to right. Twelve ffmpegs at once would fight
  over the same cores for no gain, and in order it reads as progress.
- A generation counter (`strip_run`) means thumbnails still in flight when the
  video changes are dropped rather than painted over the new one.

Measured 2026-08-13 on a 1080p h264 file: ~200 ms per frame regardless of seek
depth — `-ss` before `-i` goes through the container index, so eighteen minutes
in costs what three seconds in costs.

**Arrow keys in Start and End.** Up and Down step whichever component of the
timecode the cursor is on — hours, minutes, seconds or milliseconds — and Page
Up/Down step by ten, matching the page-increment the spin rows already use.
Typing `01:47:23.500` at a two-hour video to move it thirty seconds is
transcription, not editing.

`step_at_cursor` in the engine does the work and is tested there. Two details it
exists to get right: the component is read **from the right**, because the last
one is always seconds whatever form was typed (`00:17:11.448`, `17:11`, `11`);
and it hands back a cursor position along with the value, so holding an arrow
down keeps stepping the same component instead of wandering as the text is
rewritten under it. The controller sits on the row in the **capture** phase —
`GtkText` would otherwise take the arrows first, the same class of problem as
its built-in drop target below.

**Opening a video from outside.** The app takes `ApplicationFlags::HANDLES_OPEN`
and the desktop entry declares `Exec=muybridge %U` plus a `MimeType=video/…`
list, which is all Vitrine does and all that is needed: `flatpak build-export`
sees the `%U` and rewrites the exported `Exec` to add `--file-forwarding` and
the `@@u %U @@` markers by itself. Files' *Open With* then hands the video over
through the document portal — verified 2026-08-13 against a file in `~/Videos`,
which the sandbox cannot otherwise see.

The one case that does *not* work is a bare `flatpak run … ~/clip.mp4` from a
terminal, which bypasses the desktop entry and hands over a path the sandbox
cannot read. That toasts "isn't reachable", which is correct; the working
incantation is `flatpak run --file-forwarding … @@u FILE @@`.

**Drag and drop.** One target covering the whole window content, not the rows:
a drop aimed at a 60-pixel row is a drop that misses. The rule has no overlap —
a dropped **file** is the video, a dropped **folder** is where the frames go —
and drops are refused outright while a run is on.

The part that is not obvious: every `AdwEntryRow` and `AdwSpinRow` wraps a
`GtkText`, and `GtkText` carries its own drop target for strings. A file drag
advertises `text/plain` next to the file, so the entries win the drop and paste
`file:///…` into Name — which is what "drag and drop is finicky" actually means
in a form like this one. `strip_drop_targets` removes those targets from the
seven text-bearing rows so the drag falls through to ours. The cost is that
*text* can no longer be dropped into them, which is no loss for fields holding
filenames and timecodes.

## 5. Sandbox and portals

Portals-first, as with the author's other apps. `finish-args` grants the
display, IPC and DRI, and **nothing else** — no `--filesystem`, no network.
The video and the output folder reach the app as document-portal paths, which
is all the access it gets.

Two consequences that are deliberate, not oversights:

- The default output folder is `~/Pictures/VlcSnapshots` only when that path is
  actually reachable. In the sandbox it is not until the portal hands it over,
  and `is_dir()` says so — better no default than one that cannot be written.
- A portal path like `/run/user/1000/doc/a1b2c3/Frames` is displayed by its
  folder name alone. The hash is a handle the portal minted, not a location,
  and showing it only misleads.

ffmpeg is bundled and built with `--disable-network`, so the engine cannot
reach the network even if the sandbox let it.

## 6. Milestones

- **M0 — scaffold. DONE (2026-07-29).** Workspace, Meson→cargo bridge,
  Blueprint→GResource, desktop/metainfo/icon, Flatpak manifest with bundled
  ffmpeg 8.1.2.
- **M1 — extraction. DONE (2026-07-29).** Command contract with tests, portal
  pickers, ffprobe details, trim range, frame estimate, live progress, cancel,
  result banner with Open Folder, PNG/JPEG. 30 tests, clippy clean.
  Since: New Job (2026-07-31), drag and drop (2026-08-01), scrub preview and
  filmstrip (2026-08-13), arrow-key timecode stepping (2026-08-13). 54 tests.
- **M2 — next.** In rough order:
  1. Excludes-style **backlog polish**: remember the output folder between
     runs, and re-request it through the portal on the next launch
  2. **Scale** — an optional output size, `-vf fps=…,scale=…`
  3. **"Every N seconds"** as an alternative to frames-per-second, since that
     is how the job is often actually described
  4. **More output formats** — AVIF, WebP, TIFF. Sized and measured already;
     see §8 for the numbers, the per-format argv shapes and the test list
  5. **Batch** — a queue of videos sharing one set of settings
  6. **Trim and export the video itself** (`-c copy`) — *proposed, not agreed*.
     The range controls already describe a cut; this would write it out as
     video instead of as frames. Measured and specced in §8, including the
     scope question it raises against §1
- **M3 — distribution.** AppStream screenshots, GitHub Pages landing page,
  first `.flatpak` bundle on GitHub Releases. Flathub is not the target, same
  decision as Foresight and Vitrine.

## 7. Release flow

Not done yet; when it is, follow the house pipeline:

- Split the manifest: the current dev one (`type: dir`, `--share=network` so
  cargo can reach crates.io) and a release one pinning a git tag + commit with
  no network, using a vendored `cargo-sources.json` from
  flatpak-cargo-generator.
- `flatpak-builder --repo=repo-release` → `flatpak build-bundle` →
  `gh release create v0.1.0 Muybridge.flatpak`, with a GPG-signed annotated tag.
- The release-manifest commit pin necessarily lands in the commit *after* the
  tag — the same benign circularity as the other projects.

## 8. Backlog

### Hardware-accelerated decoding (`-hwaccel`) — investigated 2026-07-29, deferred

The bundled ffmpeg already has it, for free: the GNOME Sdk ships the
libva/vdpau/vulkan headers, so configure autodetected **vdpau, cuda, vaapi,
drm, vulkan**. (Host ffmpeg additionally has qsv, opencl and amf; none matter
here — amf is an *encoder*, and this app only ever encodes PNG/JPEG on the CPU.)

Measured inside the sandbox on the dev laptop (AMD Vega iGPU + RTX 3050 Ti),
with `--device=dri` as the only permission — no extra finish-args needed:

| method | result |
| --- | --- |
| `vaapi` | works — `radeonsi_drv_video.so`, VA-API 1.22.0, on the iGPU |
| `cuda` | works — NVDEC on the NVIDIA card, `pix_fmt: cuda` |
| `vulkan` | works |
| `drm` | `Device creation failed: -14` — not a decode path, expected |

Not wired into the UI, for three reasons that any future toggle must answer:

1. **ffmpeg does not fall back.** A failed hwaccel init ends the run with zero
   frames — the `drm` row above. A toggle therefore needs the runner to retry
   in software, not just add a flag.
2. **The win is narrower than it looks.** Frames still come back to system
   memory for the `fps` filter and the PNG/JPEG encode, and that transfer eats
   into the gain. Worth it for 4K H.264/HEVC/AV1 where decode dominates;
   invisible on modest files, where the image encode is the bottleneck.
3. **For sparse sampling the bigger lever is elsewhere** — `-skip_frame nokey`
   (keyframes only) or seeking. ffmpeg still decodes every frame today and the
   `fps` filter discards most of them.

Next step if picked up: benchmark a 1080p and a 4K clip, software vs vaapi vs
cuda, at both a dense (`fps=10`) and a sparse (`fps=0.5`) sample rate, before
writing any UI.

Side effect of trimming the muxers to `image2`: `-f null -` is unavailable in
the bundle, so benchmarks must write real frames.

### More output formats (M2.4) — measured 2026-07-29, to be built and tested

PNG and JPEG are the only options today because the bundle is configured with
`--disable-encoders --enable-encoder=png,mjpeg`. That is our restriction, not
ffmpeg's.

**Nothing new needs bundling.** Every library is already in
`org.gnome.Platform//49` — verified, not assumed:

```
libwebp.so.7   libjxl.so.0.11   libaom.so.3   libSvtAv1Enc.so.3   libopenjp2   libtiff.so.6
```

so `--enable-libwebp --enable-libjxl --enable-libsvtav1 --enable-libaom` cost a
rebuild and nothing else. A further tier is free outright — native encoders
needing only a configure flag: `tiff`, `bmp`, `qoi`, `ppm`/`pgm`, `sgi`,
`targa`, `dpx`, `exr`, `jpeg2000`, `jpegls`, `gif`, `apng`.

Measured on a 1080p clip, 20 frames, host ffmpeg 8.1.1:

| format | time | total size |
| --- | --- | --- |
| PNG (current default) | ~0s | 6200 KB |
| JPEG `-q:v 3` (current) | ~0s | 1953 KB |
| **AVIF** (libsvtav1) | 1s | **631 KB** |
| WebP | 2s | 902 KB |
| JPEG XL | 11s | 2263 KB |

**The catch, and the reason this is an engine change rather than a longer
list:** WebP and AVIF both default to writing *one animated file* instead of a
sequence, and each needs a different argv shape to stop it:

| format | what the argv needs |
| --- | --- |
| PNG, JPEG, TIFF, QOI, BMP | extension alone is enough |
| AVIF | `-f image2 -c:v libsvtav1` — `-f image2` alone works, the codec pin keeps it off libaom's slow path |
| WebP | `-c:v libwebp -f image2` — **`-f image2` alone is not enough**, it still wrote a single file |

So `Job::build_argv` has to own per-format arguments, not just swap the
extension, and `Format` grows from an enum of two into a small table
(extension, codec pin, muxer pin, quality flag and range). That table is
exactly what the argv tests should pin.

**AVIF and JXL defaults are lossy**, so the size column above is not a
like-for-like comparison with PNG. Both need a quality control the way JPEG has
one, and **PNG stays the lossless default**.

Recommended set when this is picked up: **AVIF + WebP + TIFF**. AVIF for the
size win, WebP because it is cheap and widely useful, TIFF because it is free
and archival. JXL deferred purely on speed — 11× slower than AVIF for a larger
file — but the lib is there whenever that trade looks different.

To test, per format: that a run writes N *separate* files and not one animated
one; that the quality control actually moves the file size across its range;
that the estimator and progress still agree with reality; and that the
sandboxed build really has the encoder after the configure flags change
(`flatpak run --command=ffmpeg … -encoders`).

### Trim and export the video (M2.6) — measured 2026-08-13, to be discussed

Once a range can be picked by eye on a filmstrip, the app is most of a snippet
tool already. `ffmpeg -ss START -to END -i IN -c copy OUT.mp4` remuxes without
re-encoding: near-instant, no quality loss, no encoder needed.

**Three things have to be settled before this is worth building.**

**1. It cannot cut where you asked.** Stream copy can only start on a keyframe,
because everything after one is decoded relative to it. Measured on a 1080p
h264 screencast whose keyframes fall every 4–6 seconds:

| | |
| --- | --- |
| requested start | 20.000 |
| keyframes either side | 14.857 and 20.900 |
| **where the copy actually started** | **14.857 — 5.1 s early** |

That is not a bug to fix, it is what stream copy *is*. The honest options are:

- **Snap the handles to keyframes** and show it — the user picks from the cuts
  that are actually available rather than being silently given a different one.
  Keyframe positions are one `ffprobe -skip_frame nokey` away, and the
  filmstrip is exactly the place to draw them.
- **Re-encode**, which cuts anywhere but is slow and lossy, and drags in
  encoder selection, bitrate and every other transcoder question.
- **Smart cut** — re-encode only the leading partial GOP, copy the rest. This
  is what real editors do and it is a substantial amount of work.

The first is the only one that fits this app.

**2. The bundle cannot write video today.** ffmpeg is configured
`--disable-muxers --enable-muxer=image2,image2pipe`, so there is nowhere to put
an mp4. Enabling `mp4,mov,matroska,webm` is cheap — copy needs no *encoders*,
which is the expensive tier — but it must be verified in the sandbox, not
assumed, exactly as §8's format work requires.

**3. It argues with §1, and another project already claims it.** "Not a video
editor, not a transcoder." A stream copy is neither — it is a remux, and it
writes what was already there. But the row after it is always "can I re-encode
smaller", and that is the line.

More to the point, **Montage** (`~/my-progs/montage`, concept-stage, drafted
2026-08-12) is scoped as a GNOME-native video editor that *starts life as a
lossless cutter* — this exact feature, with an engine built for it. The
counter-argument is that the filmstrip and range controls already exist *here*
while Montage has no code yet.

**So the question to settle first is not how to build it, but where it lives.**
If the answer is Montage, the keyframe measurement above is an input to that
project rather than this one.

### Other

- Scene-change sampling (`-vf select='gt(scene,0.4)'`) as a mode next to fps
- Contact-sheet output (`-vf tile=`)
- Remember the last used settings (a KeyFile under `<config>/muybridge/`,
  paths deliberately excluded — the same rule Foresight's presets follow)
