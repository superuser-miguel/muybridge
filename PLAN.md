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

## 3. Architecture

```
crates/ffmpeg-frames    UI-free engine. Job -> argv, ffprobe argv + parser,
                        -progress stream parser, timecodes, frame estimation.
                        NEVER add gtk to this crate.
crates/muybridge        the app. window.rs (composite template + all signal
                        handling), runner.rs (gio::Subprocess, async reads).
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

## 4. The UI

One scrolling page of preference groups, in the order a job is described:

1. **Video** — the file, plus what ffprobe says about it
2. **Range** — off means the whole video; on reveals start and end
3. **Frames** — rate, format, and (for JPEG) quality
4. **Output** — folder, name, suffix, counter digits, overwrite
5. **Will write** — the first filename and the frame estimate, live

Every control feeds `current_job()`, which returns either a validated `Job` or
the one sentence explaining why it isn't ready. That sentence goes in the
"Will write" row and the Extract button greys out. There is no second path
from the widgets to the command.

While a run is on, the settings go insensitive, a progress bar and a status
line appear at the bottom, and Extract is replaced by Cancel. Cancel sends
SIGTERM: ffmpeg finishes the frame it is on and the ones already written stay.

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
- **M2 — next.** In rough order:
  1. Excludes-style **backlog polish**: remember the output folder between
     runs, and re-request it through the portal on the next launch
  2. **Scale** — an optional output size, `-vf fps=…,scale=…`
  3. **"Every N seconds"** as an alternative to frames-per-second, since that
     is how the job is often actually described
  4. **Batch** — a queue of videos sharing one set of settings
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

### Other

- Scene-change sampling (`-vf select='gt(scene,0.4)'`) as a mode next to fps
- WebP output
- Drag and drop a video onto the window
- Start-of-range preview thumbnail, so a trim can be aimed
- Contact-sheet output (`-vf tile=`)
- Remember the last used settings (a KeyFile under `<config>/muybridge/`,
  paths deliberately excluded — the same rule Foresight's presets follow)
