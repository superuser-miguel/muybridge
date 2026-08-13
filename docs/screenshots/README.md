# Screenshots

These filenames are referenced by **two** files. Add images with exactly these
names and both start working; rename them and both break:

- `data/io.github.superuser_miguel.Muybridge.metainfo.xml` — the `<screenshots>`
  block, as `https://superuser-miguel.github.io/muybridge/screenshots/<name>`
- `docs/index.html` — the gallery

| File | What it should show |
| --- | --- |
| `01-preview.png` | A video loaded: the preview frame, the filmstrip, the details row |
| `02-trim.png` | A trimmed range — handles dragged in, dimming outside them, Start/End filled |
| `03-frames.png` | The Frames and Output groups: rate, format, name, and the "Will write" estimate |
| `04-progress.png` | An extraction running: progress bar, frame counter, Cancel in the header |
| `05-done.png` | The result banner after a finished run, with Open Folder |

PNG, and shot at the default 720×800 window size so they sit together evenly.
Crop to the window (drop the desktop background) — GNOME's screenshot tool does
this with the window option.

`01-preview.png` is the `type="default"` one: it is what software centres show
first, so it is the one worth getting right.
