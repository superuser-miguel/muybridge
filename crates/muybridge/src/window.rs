//! The main window: a composite template bound to `src/ui/window.blp`.
//!
//! All layout lives in the Blueprint. Rust reaches widgets only through the
//! `#[template_child]` bindings below and drives them with signal handlers;
//! this file builds no widget tree of its own.
//!
//! The shape of the thing: every control feeds [`current_job`], which produces
//! either a validated [`Job`] — whose command is what runs, and whose first
//! filename and frame estimate are what the “Will write” row shows — or the one
//! sentence explaining why it isn't ready. Nothing else decides what ffmpeg is
//! asked to do.
//!
//! [`current_job`]: MuybridgeWindow::current_job

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};

use std::path::{Path, PathBuf};
use std::time::Duration;

use ffmpeg_frames::preview::{self, Scale};
use ffmpeg_frames::{format_number, Format, Job, JobError, Probe, Progress, Timecode};

use crate::filmstrip::{FilmStrip, TILE_COUNT};
use crate::runner::{self, FrameGrab, Outcome, Runner, Status};

/// Suffix patterns offered alongside `video/*`; the portal picker matches on
/// MIME type, a plain GTK picker on the glob.
const VIDEO_PATTERNS: &[&str] = &[
    "*.mp4", "*.m4v", "*.mkv", "*.mov", "*.webm", "*.avi", "*.wmv", "*.flv", "*.mpg", "*.mpeg",
    "*.ts", "*.m2ts", "*.ogv", "*.3gp",
];

/// Set on the settings box while a drag the window would accept is over it.
const DROP_HIGHLIGHT: &str = "muybridge-drop";

// Starting state, restored by "New Job". These must match the values the
// Blueprint sets up, or a reset window would differ from a fresh one.
const EMPTY_VIDEO_TITLE: &str = "No video chosen";
const EMPTY_VIDEO_SUBTITLE: &str = "Click to pick a video, or drop one on the window";
const EMPTY_DETAILS: &str = "—";
const DEFAULT_START: &str = "00:00:00.000";
const DEFAULT_FPS: f64 = 3.33;
const DEFAULT_QUALITY: f64 = 3.0;
const DEFAULT_DIGITS: f64 = 4.0;

/// How long a scrub has to settle before ffmpeg is asked for the frame.
///
/// A drag emits motion far faster than a frame can be decoded (~200 ms), so
/// without this every pixel of travel would spawn a process that is obsolete
/// before it finishes. Short enough that letting go feels immediate.
const SCRUB_SETTLE: Duration = Duration::from_millis(120);

mod imp {
    use super::*;
    use std::cell::RefCell;

    /// Everything the window knows that isn't already a widget property.
    #[derive(Debug, Default)]
    pub struct State {
        pub video: Option<PathBuf>,
        pub probe: Option<Probe>,
        pub output_dir: Option<PathBuf>,
        /// Present only while ffmpeg is running.
        pub runner: Option<Runner>,
        /// Length of the running job's span, for the progress fraction.
        pub run_span: Option<f64>,
        pub run_estimate: Option<u64>,
        /// Where the last finished run wrote, for the banner's Open Folder.
        pub last_output_dir: Option<PathBuf>,

        // -- scrub preview and filmstrip --------------------------------
        /// Bumped whenever the strip is rebuilt. Thumbnails still in flight
        /// from the previous video carry the old number and are dropped.
        pub strip_run: u64,
        /// The thumbnail currently being decoded, so it can be abandoned.
        pub tile_grab: Option<FrameGrab>,
        pub preview_grab: Option<FrameGrab>,
        /// Where the scrub has got to, and the timer waiting for it to settle.
        pub pending_scrub: Option<f64>,
        pub scrub_timer: Option<glib::SourceId>,
    }

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/superuser_miguel/Muybridge/window.ui")]
    pub struct MuybridgeWindow {
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub extract_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub cancel_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub result_banner: TemplateChild<adw::Banner>,
        #[template_child]
        pub settings_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub video_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub details_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub preview_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub frame_picture: TemplateChild<gtk::Picture>,
        #[template_child]
        pub film_strip: TemplateChild<FilmStrip>,
        #[template_child]
        pub scrub_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub range_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub trim_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub start_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub end_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub fps_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub format_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub quality_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub folder_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub stem_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub suffix_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub digits_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub overwrite_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub preview_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub progress_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub progress_bar: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub status_label: TemplateChild<gtk::Label>,

        pub state: RefCell<State>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MuybridgeWindow {
        const NAME: &'static str = "MuybridgeWindow";
        type Type = super::MuybridgeWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            // The Blueprint names $MuybridgeFilmStrip; GTK can only build a type
            // it has already seen registered.
            FilmStrip::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MuybridgeWindow {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }
    }

    impl WidgetImpl for MuybridgeWindow {}
    impl WindowImpl for MuybridgeWindow {}
    impl ApplicationWindowImpl for MuybridgeWindow {}
    impl AdwApplicationWindowImpl for MuybridgeWindow {}
}

glib::wrapper! {
    pub struct MuybridgeWindow(ObjectSubclass<imp::MuybridgeWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gtk::gio::ActionGroup, gtk::gio::ActionMap, gtk::Accessible,
                    gtk::Buildable, gtk::ConstraintTarget, gtk::Native, gtk::Root,
                    gtk::ShortcutManager;
}

impl MuybridgeWindow {
    pub fn new(app: &adw::Application) -> Self {
        glib::Object::builder().property("application", app).build()
    }

    /// Load a video handed over on the command line or by a file manager.
    /// Everything past this point is the same as having picked it.
    pub fn open_video(&self, path: PathBuf) {
        if path.is_file() {
            self.set_video(path);
        } else {
            self.toast("That file isn't reachable — use the picker instead");
        }
    }

    fn setup(&self) {
        let imp = self.imp();

        // Rows that display user data (filenames, paths, error text) must not
        // read it as Pango markup — an "&" in a filename is just an "&".
        for row in [&*imp.video_row, &*imp.details_row, &*imp.folder_row, &*imp.preview_row] {
            row.set_use_markup(false);
        }

        if let Some(dir) = default_output_dir() {
            imp.state.borrow_mut().output_dir = Some(dir);
        }
        self.update_output_row();
        self.sync_format_rows();
        self.setup_actions();
        self.setup_drop_target();
        self.setup_filmstrip();
        self.setup_timecode_stepping();

        imp.video_row.connect_activated(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.choose_video()
        ));
        imp.folder_row.connect_activated(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.choose_output_dir()
        ));
        imp.extract_button.connect_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.start_extraction()
        ));
        imp.cancel_button.connect_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.cancel_extraction()
        ));
        imp.result_banner.connect_button_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.open_last_output_dir()
        ));

        // Everything that can change the command re-derives the preview, so
        // what the user reads is always what would run.
        imp.trim_row.connect_active_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.on_trim_toggled()
        ));
        imp.format_row.connect_selected_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| {
                win.sync_format_rows();
                win.update_preview();
            }
        ));
        for row in [&*imp.fps_row, &*imp.digits_row, &*imp.quality_row] {
            row.connect_value_notify(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_| win.update_preview()
            ));
        }
        for row in [&*imp.start_row, &*imp.end_row, &*imp.stem_row, &*imp.suffix_row] {
            row.connect_changed(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_| win.update_preview()
            ));
        }
        imp.overwrite_row.connect_active_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.update_preview()
        ));

        self.update_preview();
    }

    fn setup_actions(&self) {
        // "New Job" (win.new-job) — same action name and menu placement as
        // Foresight's, so the two apps behave alike.
        let new_job = gio::SimpleAction::new("new-job", None);
        new_job.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.clear_job()
        ));
        self.add_action(&new_job);
    }

    // ----------------------------------------------------- timecode stepping

    /// Arrow keys nudge the part of the timecode the cursor is on.
    ///
    /// Typing `01:47:23.500` into a two-hour video to move it thirty seconds is
    /// not editing, it is transcription. With this, the cursor goes on the
    /// minutes and Up is a minute — and the filmstrip follows, because these
    /// rows are still what everything reads from.
    ///
    /// **Capture phase**, because the `GtkText` inside the row has its own key
    /// handler and would see the arrows first — the same class of problem as
    /// its built-in drop target in [`setup_drop_target`](Self::setup_drop_target).
    fn setup_timecode_stepping(&self) {
        for row in [self.imp().start_row.get(), self.imp().end_row.get()] {
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            keys.connect_key_pressed(glib::clone!(
                #[weak(rename_to = win)]
                self,
                #[weak]
                row,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |_, key, _, _| win.step_timecode(&row, key)
            ));
            row.add_controller(keys);
        }
    }

    fn step_timecode(&self, row: &adw::EntryRow, key: gdk::Key) -> glib::Propagation {
        // Page Up/Down step by ten, matching the page-increment the spin rows
        // in this same form already use.
        let steps = match key {
            gdk::Key::Up | gdk::Key::KP_Up => 1,
            gdk::Key::Down | gdk::Key::KP_Down => -1,
            gdk::Key::Page_Up | gdk::Key::KP_Page_Up => 10,
            gdk::Key::Page_Down | gdk::Key::KP_Page_Down => -10,
            _ => return glib::Propagation::Proceed,
        };

        let cursor = row.position().max(0) as usize;
        // Text that doesn't parse has nothing to step: let the key through
        // rather than swallowing it and appearing broken.
        let Some((value, caret)) = ffmpeg_frames::step_at_cursor(&row.text(), cursor, steps) else {
            return glib::Propagation::Proceed;
        };

        // Stepping past the end of the video is never what was meant. Zero is
        // handled in the engine; the duration is the bound only the UI knows.
        let seconds = match self.video_duration() {
            Some(duration) => value.seconds().min(duration),
            None => value.seconds(),
        };

        let text = Timecode::from_seconds(seconds).format();
        row.set_text(&text);
        row.set_position(caret.min(text.len()) as i32);
        glib::Propagation::Stop
    }

    // ------------------------------------------------ scrub preview & strip

    /// Wire the filmstrip to the timecode rows in both directions.
    ///
    /// The strip never becomes the source of truth. Dragging a handle writes
    /// into Start and End exactly as typing would, and everything downstream —
    /// validation, the frame estimate, the command itself — carries on reading
    /// those rows. What comes back the other way is [`sync_strip_from_rows`],
    /// called from `update_preview`, so a typed timecode moves the handle too.
    ///
    /// [`sync_strip_from_rows`]: Self::sync_strip_from_rows
    fn setup_filmstrip(&self) {
        let imp = self.imp();

        imp.film_strip.connect_range_changed(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |start, end| win.apply_strip_range(start, end)
        ));
        imp.film_strip.connect_scrub(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |seconds| win.scrub_to(seconds)
        ));
    }

    /// A handle was dragged. Reaching for a handle *is* asking to trim, so the
    /// switch follows rather than making the drag do nothing until it is found.
    fn apply_strip_range(&self, start: f64, end: f64) {
        let imp = self.imp();
        if !imp.trim_row.is_active() {
            imp.trim_row.set_active(true);
        }
        // These fire `changed`, which re-derives the preview and syncs the
        // strip back — with the values it just sent, so it settles at once.
        imp.start_row
            .set_text(&Timecode::from_seconds(start).format());
        imp.end_row.set_text(&Timecode::from_seconds(end).format());
    }

    /// Move the scrub position: the readout follows immediately, the frame
    /// after the drag settles. Doing it the other way round would leave the
    /// number lagging behind the handle by a decode.
    fn scrub_to(&self, seconds: f64) {
        let imp = self.imp();
        imp.scrub_label
            .set_label(&Timecode::from_seconds(seconds).format());

        let mut state = imp.state.borrow_mut();
        state.pending_scrub = Some(seconds);
        if let Some(timer) = state.scrub_timer.take() {
            timer.remove();
        }
        drop(state);

        let win = self.downgrade();
        let timer = glib::timeout_add_local_once(SCRUB_SETTLE, move || {
            let Some(win) = win.upgrade() else { return };
            // The source has fired and is gone; forget it before anything can
            // try to remove it a second time.
            win.imp().state.borrow_mut().scrub_timer = None;
            win.grab_scrub_frame();
        });
        imp.state.borrow_mut().scrub_timer = Some(timer);
    }

    /// Ask ffmpeg for the frame the scrub settled on.
    fn grab_scrub_frame(&self) {
        let imp = self.imp();
        let (video, at) = {
            let state = imp.state.borrow();
            (state.video.clone(), state.pending_scrub)
        };
        let (Some(video), Some(at)) = (video, at) else {
            return;
        };

        // Whatever was being decoded is for a position the user has left.
        let previous = imp.state.borrow_mut().preview_grab.take();
        if let Some(grab) = previous {
            grab.cancel();
        }

        let (width, height) = preview::PREVIEW_BOX;
        let argv = ffmpeg_frames::thumbnail_argv(&video, at, Scale::Fit { width, height });

        let win = self.downgrade();
        let grab = runner::grab_frame(argv, move |bytes| {
            let Some(win) = win.upgrade() else { return };
            win.imp().state.borrow_mut().preview_grab = None;
            // No frame is the ordinary answer past the end of the video: leave
            // the last good one up rather than blanking the preview.
            if let Some(texture) = bytes.and_then(|b| gdk::Texture::from_bytes(&b).ok()) {
                win.imp().frame_picture.set_paintable(Some(&texture));
            }
        });
        if let Ok(grab) = grab {
            imp.state.borrow_mut().preview_grab = Some(grab);
        }
    }

    /// Fill the strip for the current video, one thumbnail at a time.
    ///
    /// Sequential, not parallel: twelve ffmpegs at once would fight over the
    /// same cores for no gain, and this way the tiles appear left to right,
    /// which reads as progress rather than as a stall.
    fn build_filmstrip(&self) {
        let imp = self.imp();
        let (video, duration) = (self.imp().state.borrow().video.clone(), self.video_duration());
        let (Some(video), Some(duration)) = (video, duration) else {
            return;
        };

        let run = {
            let mut state = imp.state.borrow_mut();
            state.strip_run += 1;
            state.strip_run
        };
        imp.film_strip.reset(duration);
        self.sync_strip_from_rows();
        self.request_tile(video, ffmpeg_frames::tile_times(duration, TILE_COUNT), 0, run);
    }

    fn request_tile(&self, video: PathBuf, times: Vec<f64>, index: usize, run: u64) {
        let imp = self.imp();
        if imp.state.borrow().strip_run != run {
            return; // a different video is being laid out now
        }
        let Some(&at) = times.get(index) else {
            return; // the strip is full
        };

        let argv = ffmpeg_frames::thumbnail_argv(&video, at, Scale::Height(preview::TILE_HEIGHT));
        let win = self.downgrade();
        let grab = runner::grab_frame(argv, move |bytes| {
            let Some(win) = win.upgrade() else { return };
            if win.imp().state.borrow().strip_run != run {
                return;
            }
            // A tile that will not decode just stays empty; the strip is a
            // guide, not a promise.
            if let Some(texture) = bytes.and_then(|b| gdk::Texture::from_bytes(&b).ok()) {
                win.imp().film_strip.set_tile(index, texture);
            }
            win.request_tile(video, times, index + 1, run);
        });
        match grab {
            Ok(grab) => imp.state.borrow_mut().tile_grab = Some(grab),
            Err(_) => imp.state.borrow_mut().tile_grab = None,
        }
    }

    /// Push the timecode rows onto the strip. Silent — the strip's setters do
    /// not call back, which is what stops this from looping with
    /// [`apply_strip_range`](Self::apply_strip_range).
    fn sync_strip_from_rows(&self) {
        let imp = self.imp();
        let Some(duration) = self.video_duration() else {
            return;
        };
        let (start, end) = if imp.trim_row.is_active() {
            let row_time = |row| {
                parse_row_time(row)
                    .ok()
                    .flatten()
                    .map(Timecode::seconds)
            };
            (
                row_time(&imp.start_row).unwrap_or(0.0),
                row_time(&imp.end_row).unwrap_or(duration),
            )
        } else {
            // Trimming off means the whole video, and the strip should say so.
            (0.0, duration)
        };
        let (start, end) = (start.clamp(0.0, duration), end.clamp(0.0, duration));
        imp.film_strip.set_range(start, end);

        imp.range_label.set_label(&format!(
            "{} of {}",
            Timecode::from_seconds((end - start).max(0.0)).format_short(),
            Timecode::from_seconds(duration).format_short(),
        ));
    }

    /// Stop everything the preview has in flight. Called before a new video,
    /// before an extraction, and on reset — a stale frame arriving afterwards
    /// would contradict what is on screen.
    fn cancel_previews(&self) {
        let imp = self.imp();
        let mut state = imp.state.borrow_mut();
        state.strip_run += 1;
        state.pending_scrub = None;
        if let Some(timer) = state.scrub_timer.take() {
            timer.remove();
        }
        let grabs = [state.tile_grab.take(), state.preview_grab.take()];
        drop(state);
        for grab in grabs.into_iter().flatten() {
            grab.cancel();
        }
    }

    // -------------------------------------------------------- drag and drop

    /// Drop a video onto the window instead of picking it.
    ///
    /// One target, on the whole window content, because a drop aimed at a
    /// 60-pixel row is a drop that misses. The rule has no overlap and needs no
    /// explaining: a dropped **file** is the video, a dropped **folder** is
    /// where the frames go.
    ///
    /// The entries have to be disarmed first — see [`strip_drop_targets`].
    fn setup_drop_target(&self) {
        let imp = self.imp();

        // Every AdwEntryRow and AdwSpinRow wraps a GtkText, and GtkText ships
        // its own drop target for strings. A file drag advertises text/plain
        // alongside the file, so without this a drop landing a few pixels off
        // the mark quietly types "file:///…" into Name instead of loading the
        // video. These rows hold filenames and timecodes; they have no use for
        // a dropped URI, and with the target gone the drag falls through to
        // ours below.
        for row in [
            imp.start_row.upcast_ref::<gtk::Widget>(),
            imp.end_row.upcast_ref(),
            imp.stem_row.upcast_ref(),
            imp.suffix_row.upcast_ref(),
            imp.fps_row.upcast_ref(),
            imp.digits_row.upcast_ref(),
            imp.quality_row.upcast_ref(),
        ] {
            strip_drop_targets(row);
        }

        // A multi-file drag arrives as a FileList; some sources offer only a
        // single GFile.
        let drop = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY);
        drop.set_types(&[gdk::FileList::static_type(), gio::File::static_type()]);

        drop.connect_enter(glib::clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |_, _, _| {
                // Nothing is droppable mid-run: the settings are frozen and the
                // command is already out of the user's hands.
                if win.imp().state.borrow().runner.is_some() {
                    return gdk::DragAction::empty();
                }
                win.imp().settings_box.add_css_class(DROP_HIGHLIGHT);
                gdk::DragAction::COPY
            }
        ));
        drop.connect_leave(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.imp().settings_box.remove_css_class(DROP_HIGHLIGHT)
        ));
        drop.connect_drop(glib::clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            false,
            move |_, value, _, _| {
                win.imp().settings_box.remove_css_class(DROP_HIGHLIGHT);
                if win.imp().state.borrow().runner.is_some() {
                    return false;
                }
                win.accept_drop(&dropped_files(value))
            }
        ));

        imp.toast_overlay.add_controller(drop);
    }

    /// Apply a drop: the first file becomes the video, the first folder becomes
    /// the output folder. Returns whether anything was taken — GTK shows the
    /// drop as rejected when it wasn't.
    fn accept_drop(&self, files: &[gio::File]) -> bool {
        let (mut video, mut folder) = (None, None);
        let mut ignored = 0usize;

        for file in files {
            // A file with no local path is a remote or unexported one; ffmpeg
            // needs a path on this filesystem.
            let Some(path) = file.path() else {
                ignored += 1;
                continue;
            };
            let slot = if path.is_dir() { &mut folder } else { &mut video };
            if slot.is_none() {
                *slot = Some(path);
            } else {
                ignored += 1;
            }
        }

        let took_folder = folder.is_some();
        if let Some(path) = folder {
            self.imp().state.borrow_mut().output_dir = Some(path);
            self.update_output_row();
        }

        let had_video = video.is_some();
        let took_video = match video {
            // The path exists as a string but not as a file: in the sandbox
            // that means the drag was never exported to us. Say so, rather than
            // loading it and letting ffprobe deliver the bad news.
            Some(path) if !path.is_file() => {
                self.toast("That file isn't reachable — use the picker instead");
                false
            }
            Some(path) => {
                self.set_video(path);
                true
            }
            None => false,
        };

        let took_something = took_video || took_folder;
        if !took_something {
            // An unreachable file has already said why; only speak up if
            // nothing else did.
            if !had_video {
                self.toast("Nothing in that drop to use");
            }
        } else if ignored > 0 {
            self.toast("One video at a time — took the first");
        } else if took_folder && !took_video {
            self.toast("Output folder set");
        }

        // A folder-only drop leaves the preview stale; a video drop refreshes
        // it through `set_video`.
        if took_folder && !took_video {
            self.update_preview();
        }
        took_something
    }

    /// Reset the form for a fresh job. Ignored while a run is live, so it can
    /// never yank the settings out from under a running ffmpeg.
    ///
    /// The **output folder deliberately survives**, unlike Foresight's
    /// destination. There, a new sync usually has a new destination; here the
    /// video changes and the dump folder does not — and in the sandbox the
    /// folder is the one setting that costs a portal round-trip to re-pick.
    fn clear_job(&self) {
        let imp = self.imp();
        if imp.state.borrow().runner.is_some() {
            return;
        }

        // The video and everything derived from it.
        {
            let mut state = imp.state.borrow_mut();
            state.video = None;
            state.probe = None;
            state.run_span = None;
            state.run_estimate = None;
        }
        imp.video_row.set_title(EMPTY_VIDEO_TITLE);
        imp.video_row.set_subtitle(EMPTY_VIDEO_SUBTITLE);
        imp.details_row.set_subtitle(EMPTY_DETAILS);

        // The preview and the strip belong to the video that just went away.
        self.cancel_previews();
        imp.preview_box.set_visible(false);
        imp.frame_picture.set_paintable(gdk::Paintable::NONE);
        imp.film_strip.reset(0.0);
        imp.scrub_label.set_label("");
        imp.range_label.set_label("");

        // Range.
        imp.trim_row.set_active(false);
        imp.start_row.set_text(DEFAULT_START);
        imp.end_row.set_text("");

        // Frames.
        imp.fps_row.set_value(DEFAULT_FPS);
        imp.format_row.set_selected(Format::Png.index());
        imp.quality_row.set_value(DEFAULT_QUALITY);

        // Output — the folder stays, see above.
        imp.stem_row.set_text("");
        imp.suffix_row.set_text("");
        imp.digits_row.set_value(DEFAULT_DIGITS);
        imp.overwrite_row.set_active(true);

        // Whatever the last run left on screen.
        imp.result_banner.set_revealed(false);
        imp.progress_bar.set_fraction(0.0);
        imp.status_label.set_label("");

        self.sync_format_rows();
        self.update_preview();
    }

    // ------------------------------------------------------------ the job

    /// Read every control into a validated [`Job`], or explain in one sentence
    /// why there isn't one yet. This is the only place the UI is turned into a
    /// command.
    fn current_job(&self) -> Result<Job, String> {
        let imp = self.imp();
        let (video, output_dir) = {
            let state = imp.state.borrow();
            (state.video.clone(), state.output_dir.clone())
        };
        let video = video.ok_or_else(|| JobError::NoInput.to_string())?;
        let output_dir = output_dir.ok_or_else(|| JobError::NoOutputDir.to_string())?;

        let mut job = Job::new(video, output_dir);
        job.stem = imp.stem_row.text().to_string();
        job.suffix = imp.suffix_row.text().to_string();
        job.fps = imp.fps_row.value();
        job.digits = imp.digits_row.value().round() as u8;
        job.format = Format::from_index(imp.format_row.selected());
        job.quality = imp.quality_row.value().round() as u8;
        job.overwrite = imp.overwrite_row.is_active();

        let mut timecode_error = None;
        let (mut start_bad, mut end_bad) = (false, false);
        if imp.trim_row.is_active() {
            match parse_row_time(&imp.start_row) {
                Ok(value) => job.start = value,
                Err(message) => {
                    start_bad = true;
                    timecode_error = Some(format!("Start — {message}"));
                }
            }
            match parse_row_time(&imp.end_row) {
                Ok(value) => job.end = value,
                Err(message) => {
                    end_bad = true;
                    timecode_error.get_or_insert(format!("End — {message}"));
                }
            }
        }
        set_error_style(&imp.start_row, start_bad);
        set_error_style(&imp.end_row, end_bad);
        if let Some(message) = timecode_error {
            return Err(message);
        }

        job.validate().map_err(|e| e.to_string())?;
        Ok(job)
    }

    /// Refresh the “Will write” row and the Extract button from the current
    /// state of the controls.
    fn update_preview(&self) {
        let imp = self.imp();
        let running = imp.state.borrow().runner.is_some();

        // The handles answer to the rows, so they are refreshed from the same
        // place and at the same moment as everything else derived from them.
        self.sync_strip_from_rows();

        match self.current_job() {
            Ok(job) => {
                let duration = self.video_duration();
                let mut text = job.example_filename();
                if let Some(frames) = job.estimated_frames(duration) {
                    text.push_str(&format!("  ·  about {frames} frames"));
                }
                imp.preview_row.set_subtitle(&text);
                imp.extract_button.set_sensitive(!running);
            }
            Err(message) => {
                imp.preview_row.set_subtitle(&message);
                imp.extract_button.set_sensitive(false);
            }
        }
    }

    fn video_duration(&self) -> Option<f64> {
        self.imp()
            .state
            .borrow()
            .probe
            .as_ref()
            .and_then(|probe| probe.duration)
    }

    // ------------------------------------------------------------ choosing

    fn choose_video(&self) {
        let dialog = gtk::FileDialog::builder()
            .title("Choose a Video")
            .modal(true)
            .build();

        let videos = gtk::FileFilter::new();
        videos.set_name(Some("Video files"));
        videos.add_mime_type("video/*");
        for pattern in VIDEO_PATTERNS {
            videos.add_pattern(pattern);
        }
        let all = gtk::FileFilter::new();
        all.set_name(Some("All files"));
        all.add_pattern("*");

        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&videos);
        filters.append(&all);
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&videos));

        if let Some(folder) = self
            .imp()
            .state
            .borrow()
            .video
            .as_deref()
            .and_then(Path::parent)
        {
            dialog.set_initial_folder(Some(&gio::File::for_path(folder)));
        }

        dialog.open(
            Some(self),
            gio::Cancellable::NONE,
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |result| match result.map(|file| file.path()) {
                    Ok(Some(path)) => win.set_video(path),
                    // A remote file has no local path; ffmpeg needs one.
                    Ok(None) => win.toast("That file isn't available on this system"),
                    Err(_) => {} // dismissed
                }
            ),
        );
    }

    fn set_video(&self, path: PathBuf) {
        let imp = self.imp();
        imp.video_row.set_title(&display_name(&path));
        imp.video_row.set_subtitle(&display_parent(&path));
        imp.stem_row.set_text(&ffmpeg_frames::default_stem(&path));
        imp.details_row.set_subtitle("Reading…");
        imp.end_row.set_text("");

        // Nothing can be laid out along a video of unknown length, so the strip
        // waits for ffprobe rather than showing an empty one.
        self.cancel_previews();
        imp.preview_box.set_visible(false);
        imp.frame_picture.set_paintable(gdk::Paintable::NONE);
        imp.scrub_label.set_label("");
        imp.range_label.set_label("");

        {
            let mut state = imp.state.borrow_mut();
            state.video = Some(path.clone());
            state.probe = None;
        }
        self.update_preview();

        runner::run_ffprobe(
            ffmpeg_frames::probe_argv(&path),
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |probe| win.apply_probe(probe)
            ),
        );
    }

    fn apply_probe(&self, probe: Option<Probe>) {
        let imp = self.imp();
        match probe {
            Some(probe) => {
                imp.details_row.set_subtitle(&probe.summary());
                // Pre-fill the end of the range with the end of the video, so
                // turning trimming on gives a valid range to narrow down.
                if imp.end_row.text().trim().is_empty() {
                    if let Some(duration) = probe.duration {
                        imp.end_row
                            .set_text(&Timecode::from_seconds(duration).format());
                    }
                }
                imp.state.borrow_mut().probe = Some(probe);
            }
            None => imp
                .details_row
                .set_subtitle("ffprobe could not read this file"),
        }
        self.update_preview();

        // A duration is what makes a filmstrip possible; without one there is
        // nothing to measure the tiles against.
        if self.video_duration().is_some() {
            imp.preview_box.set_visible(true);
            self.build_filmstrip();
            // Open on the first frame rather than on black, so the preview is
            // showing the video before it is touched.
            self.scrub_to(0.0);
        }
    }

    fn choose_output_dir(&self) {
        let dialog = gtk::FileDialog::builder()
            .title("Choose an Output Folder")
            .modal(true)
            .build();
        if let Some(current) = self.imp().state.borrow().output_dir.as_deref() {
            dialog.set_initial_folder(Some(&gio::File::for_path(current)));
        }

        dialog.select_folder(
            Some(self),
            gio::Cancellable::NONE,
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |result| match result.map(|file| file.path()) {
                    Ok(Some(path)) => {
                        win.imp().state.borrow_mut().output_dir = Some(path);
                        win.update_output_row();
                        win.update_preview();
                    }
                    Ok(None) => win.toast("That folder isn't available on this system"),
                    Err(_) => {}
                }
            ),
        );
    }

    fn update_output_row(&self) {
        let imp = self.imp();
        let subtitle = imp
            .state
            .borrow()
            .output_dir
            .as_deref()
            .map(display_path)
            .unwrap_or_else(|| "Not chosen".to_string());
        imp.folder_row.set_subtitle(&subtitle);
    }

    fn on_trim_toggled(&self) {
        let imp = self.imp();
        if imp.trim_row.is_active() {
            if imp.start_row.text().trim().is_empty() {
                imp.start_row.set_text("00:00:00.000");
            }
            if imp.end_row.text().trim().is_empty() {
                if let Some(duration) = self.video_duration() {
                    imp.end_row
                        .set_text(&Timecode::from_seconds(duration).format());
                }
            }
        }
        self.update_preview();
    }

    fn sync_format_rows(&self) {
        let imp = self.imp();
        let format = Format::from_index(imp.format_row.selected());
        imp.quality_row.set_visible(format.is_lossy());
    }

    // ------------------------------------------------------------ running

    fn start_extraction(&self) {
        let imp = self.imp();
        let job = match self.current_job() {
            Ok(job) => job,
            Err(message) => {
                self.toast(&message);
                return;
            }
        };

        if let Err(error) = std::fs::create_dir_all(&job.output_dir) {
            self.toast(&format!("Cannot write to that folder: {error}"));
            return;
        }

        // The extraction gets the cores to itself; a half-built filmstrip is
        // not worth slowing it down for.
        self.cancel_previews();

        let duration = self.video_duration();
        {
            let mut state = imp.state.borrow_mut();
            state.run_span = job.range_seconds(duration);
            state.run_estimate = job.estimated_frames(duration);
        }

        let output_dir = job.output_dir.clone();
        let argv = job.build_argv();

        imp.result_banner.set_revealed(false);
        imp.progress_bar.set_fraction(0.0);
        imp.progress_bar.set_text(Some("0%"));
        imp.status_label.set_label("Starting ffmpeg…");
        self.set_running(true);

        let spawned = runner::spawn_ffmpeg(
            argv,
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |progress| win.on_progress(progress)
            ),
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |outcome| win.finish_run(outcome, output_dir.clone())
            ),
        );

        match spawned {
            Ok(runner) => imp.state.borrow_mut().runner = Some(runner),
            Err(error) => {
                self.set_running(false);
                self.toast(&format!("Could not start ffmpeg: {error}"));
            }
        }
    }

    fn cancel_extraction(&self) {
        if let Some(runner) = self.imp().state.borrow().runner.as_ref() {
            runner.cancel();
            self.imp().status_label.set_label("Stopping ffmpeg…");
        }
    }

    fn on_progress(&self, progress: Progress) {
        let imp = self.imp();
        let (span, estimate) = {
            let state = imp.state.borrow();
            (state.run_span, state.run_estimate)
        };

        match (span, progress.out_time) {
            (Some(span), Some(out_time)) if span > 0.0 => {
                let fraction = (out_time / span).clamp(0.0, 1.0);
                imp.progress_bar.set_fraction(fraction);
                imp.progress_bar
                    .set_text(Some(&format!("{}%", (fraction * 100.0).round())));
            }
            // No duration to measure against — show motion, not a false number.
            _ => {
                imp.progress_bar.pulse();
                imp.progress_bar.set_text(Some("Working…"));
            }
        }

        let mut parts = Vec::new();
        if let Some(frame) = progress.frame {
            parts.push(match estimate {
                Some(total) => format!("Frame {frame} of about {total}"),
                None => format!("Frame {frame}"),
            });
        }
        if let Some(speed) = progress.speed {
            parts.push(format!("{}× speed", format_number(speed)));
        }
        imp.status_label.set_label(&parts.join("  ·  "));
    }

    fn finish_run(&self, outcome: Outcome, output_dir: PathBuf) {
        let imp = self.imp();
        imp.state.borrow_mut().runner = None;
        self.set_running(false);

        let frames = outcome.frames.unwrap_or(0);
        let wrote_something = frames > 0;

        let title = match outcome.status {
            Status::Success if wrote_something => format!(
                "Wrote {frames} {} to {}",
                plural(frames, "frame", "frames"),
                display_path(&output_dir)
            ),
            Status::Success => {
                "ffmpeg wrote no frames — check the range and the frame rate".to_string()
            }
            Status::Cancelled => format!(
                "Cancelled — kept {frames} {}",
                plural(frames, "frame", "frames")
            ),
            Status::Failed => outcome.message.clone(),
        };

        imp.result_banner.set_title(&title);
        imp.result_banner
            .set_button_label(wrote_something.then_some("Open Folder"));
        imp.result_banner.set_revealed(true);

        if outcome.status == Status::Failed {
            self.toast("Extraction failed");
        }

        imp.state.borrow_mut().last_output_dir = Some(output_dir);
        self.update_preview();
    }

    fn set_running(&self, running: bool) {
        let imp = self.imp();
        imp.extract_button.set_sensitive(!running);
        imp.cancel_button.set_visible(running);
        imp.settings_box.set_sensitive(!running);
        imp.progress_box.set_visible(running);
    }

    fn open_last_output_dir(&self) {
        let dir = self.imp().state.borrow().last_output_dir.clone();
        if let Some(dir) = dir {
            let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(dir)));
            launcher.launch(Some(self), gio::Cancellable::NONE, |_| {});
        }
    }

    fn toast(&self, message: &str) {
        self.imp().toast_overlay.add_toast(adw::Toast::new(message));
    }
}

/// Pull the files out of a drop, whichever of the two shapes it arrived in.
fn dropped_files(value: &glib::Value) -> Vec<gio::File> {
    if let Ok(list) = value.get::<gdk::FileList>() {
        return list.files();
    }
    if let Ok(file) = value.get::<gio::File>() {
        return vec![file];
    }
    Vec::new()
}

/// Remove every drop target from a widget and its descendants.
///
/// GTK adds one to `GtkText` internally, so this is the only way to stop an
/// entry from swallowing a file drag and pasting its URI. Removing it costs the
/// ability to drop *text* into these rows, which is a trade worth making for
/// fields that hold filenames and timecodes.
fn strip_drop_targets(widget: &gtk::Widget) {
    // Collect first: removing a controller mutates the list being iterated.
    let doomed: Vec<gtk::EventController> = widget
        .observe_controllers()
        .iter::<glib::Object>()
        .flatten()
        .filter_map(|object| object.downcast::<gtk::EventController>().ok())
        .filter(|c| c.is::<gtk::DropTarget>() || c.is::<gtk::DropTargetAsync>())
        .collect();
    for controller in doomed {
        widget.remove_controller(&controller);
    }

    let mut child = widget.first_child();
    while let Some(widget) = child {
        strip_drop_targets(&widget);
        child = widget.next_sibling();
    }
}

/// Read a timecode entry: empty means "not set", which is valid.
fn parse_row_time(row: &adw::EntryRow) -> Result<Option<Timecode>, String> {
    let text = row.text();
    if text.trim().is_empty() {
        return Ok(None);
    }
    Timecode::parse(&text).map(Some).map_err(|e| e.to_string())
}

fn set_error_style(row: &adw::EntryRow, bad: bool) {
    if bad {
        row.add_css_class("error");
    } else {
        row.remove_css_class("error");
    }
}

/// `~/Pictures/VlcSnapshots` when it is actually reachable, else `~/Pictures`,
/// else nothing. Inside the Flatpak sandbox these paths exist but are not
/// readable until the portal hands them over, and `is_dir()` says so — which is
/// exactly what we want: no default the app cannot actually write to.
fn default_output_dir() -> Option<PathBuf> {
    let pictures = glib::user_special_dir(glib::UserDirectory::Pictures)?;
    let snapshots = pictures.join("VlcSnapshots");
    if snapshots.is_dir() {
        Some(snapshots)
    } else if pictures.is_dir() {
        Some(pictures)
    } else {
        None
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn display_parent(path: &Path) -> String {
    path.parent().map(display_path).unwrap_or_default()
}

/// Paths shortened for display: `$HOME` becomes `~`, and a document-portal
/// path is reduced to its folder name — the `a1b2c3` in
/// `/run/user/1000/doc/a1b2c3/Frames` is a handle the portal minted, not a
/// place, and showing it only misleads.
fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    if text.starts_with("/run/user/") && text.contains("/doc/") {
        return display_name(path);
    }
    if let Some(home) = glib::home_dir().to_str() {
        if let Some(rest) = text.strip_prefix(home) {
            return format!("~{rest}");
        }
    }
    text
}

fn plural(count: u64, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 {
        one
    } else {
        many
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plural_agrees_with_the_count() {
        assert_eq!(plural(1, "frame", "frames"), "frame");
        assert_eq!(plural(0, "frame", "frames"), "frames");
        assert_eq!(plural(412, "frame", "frames"), "frames");
    }

    #[test]
    fn display_name_is_the_file_not_the_path() {
        assert_eq!(display_name(Path::new("/a/b/clip.mp4")), "clip.mp4");
    }
}
