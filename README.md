# Muybridge

Take still frames out of video, at whatever rate you ask for.

A GTK4/libadwaita front end for one thing ffmpeg does very well: sampling a
video into a numbered sequence of stills. Point it at a file, say how often to
sample, choose where the frames go, and watch it work.

Named after Eadweard Muybridge, who got there first with twelve cameras and a
trip-wire.

## What it runs

The whole app is a careful way of writing these two command lines:

```sh
ffmpeg -i video.mp4 -vf fps=9.99 out/video_%04d.png

ffmpeg -ss 00:17:11.448 -to 00:38:54.374 -i video.mp4 -vf fps=3.33 out/video_%04d_test.png
```

`crates/ffmpeg-frames` builds exactly that, and its tests pin both commands
argument for argument. Two flags are added that a GUI can't do without —
`-nostdin` so ffmpeg never waits on a terminal that isn't there, and
`-progress pipe:1 -nostats` for machine-readable progress instead of a
redrawing status line — plus an explicit `-y`/`-n`, because the interactive
"File exists. Overwrite?" prompt would hang the app forever.

Commands are spawned as an argv vector, never as a shell string. A filename
with a space, a quote or a `$` in it is just a filename.

## What it does

- **Any sample rate** — 0.01 to 240 frames per second of video
- **Trim** to a range with `HH:MM:SS.mmm` start and end times
- **Details up front** — size, codec, frame rate and length, read with ffprobe
- **An estimate before you commit** — roughly how many frames the job will write
- **Live progress**, and a cancel that stops cleanly and keeps what it wrote
- **Naming you control** — base name, counter width, and a suffix after the counter
- **PNG or JPEG**, with a quality setting for JPEG

## Status

v0.1.0, working. Not yet released as a bundle — build it yourself for now.

## Building

Everything needed is on a normal GNOME development host: GTK 4.12+,
libadwaita 1.4+, blueprint-compiler, meson, cargo, and ffmpeg on `PATH`.

```sh
meson setup builddir -Dprofile=debug
meson compile -C builddir
meson test -C builddir

MUYBRIDGE_GRESOURCE=builddir/muybridge.gresource ./builddir/muybridge
```

Or as a Flatpak, which bundles its own ffmpeg so nothing else is needed:

```sh
flatpak-builder --user --install --force-clean build-dir io.github.superuser_miguel.Muybridge.yml
flatpak run io.github.superuser_miguel.Muybridge
```

## How it fits together

```
crates/ffmpeg-frames   engine — argv construction, ffprobe parsing, progress
                       parsing, frame estimation. No GTK, ever.
crates/muybridge       the app — Blueprint UI, portal file pickers, and an
                       async gio::Subprocess runner.
src/ui/window.blp      the entire layout. Rust builds no widgets.
data/                  desktop entry, AppStream metainfo, icon, gresource
```

Meson drives the build and calls cargo, so Blueprint compilation, GResource
bundling and the Rust build all live in one build system.

In the Flatpak, files arrive through the desktop portals: the app can read the
video you picked and write to the folder you chose, and has no other access to
your disk — no `--filesystem`, no network.

## License

GPL-3.0-or-later.
