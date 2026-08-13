//! The filmstrip: thumbnails across the whole video, with draggable in and out
//! handles over them.
//!
//! This is the one widget in the app that draws itself. There is no GTK control
//! shaped like "a range with two ends over a picture of the thing being
//! ranged", and faking one out of two `GtkScale`s puts the numbers back in
//! charge — the point here is to pick a range *by eye*.
//!
//! It owns no truth. Dragging a handle calls back to the window, which writes
//! the timecode into the Start/End rows exactly as if it had been typed; those
//! rows remain what the job is built from. The window then hands the strip its
//! values back through [`set_range`]. Setters never fire the callbacks, only
//! gestures do, so that round trip cannot loop.
//!
//! [`set_range`]: FilmStrip::set_range

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

/// How many thumbnails to spread across the video. Twelve is enough to
//  recognise a scene change at the width this sits at, and costs about two and
/// a half seconds of ffmpeg to fill.
pub const TILE_COUNT: usize = 12;

/// How close to a handle a press has to land to grab it, in pixels. Generous,
/// because the handle is drawn 3px wide and nobody can hit that.
const GRAB_SLOP: f64 = 14.0;

/// Blank margin at each end of the strip, in pixels.
///
/// Without it the two handles of an untrimmed video — which is every video, on
/// opening — sit exactly on the widget's edges, where half of each is clipped
/// away and what survives reads as part of the frame. Nothing then says the
/// range can be dragged at all. The tiles are laid inside the same inset, so a
/// handle still stands over the moment it points at.
const EDGE: f64 = 8.0;

/// Which part of the strip a drag is moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grip {
    Start,
    End,
    Playhead,
}

/// Told the time under a moving drag.
type ScrubFn = Box<dyn Fn(f64)>;
/// Told the new (start, end) when a handle drag changes the range.
type RangeFn = Box<dyn Fn(f64, f64)>;

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct FilmStrip {
        pub duration: Cell<f64>,
        /// One slot per tile, filled in as ffmpeg delivers them.
        pub tiles: RefCell<Vec<Option<gdk::Texture>>>,
        pub start: Cell<f64>,
        pub end: Cell<f64>,
        pub playhead: Cell<f64>,
        pub(super) grip: Cell<Option<Grip>>,
        /// Fired continuously while a drag moves, with the time under it.
        pub on_scrub: RefCell<Option<ScrubFn>>,
        /// Fired when a handle drag changes the range, with (start, end).
        pub on_range: RefCell<Option<RangeFn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FilmStrip {
        const NAME: &'static str = "MuybridgeFilmStrip";
        type Type = super::FilmStrip;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for FilmStrip {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup_gestures();
        }
    }

    impl WidgetImpl for FilmStrip {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().draw(snapshot);
        }
    }
}

glib::wrapper! {
    pub struct FilmStrip(ObjectSubclass<imp::FilmStrip>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for FilmStrip {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl FilmStrip {
    // ------------------------------------------------------------ the state

    /// Point the strip at a new video. Clears the tiles: they belong to the old
    /// one and showing them under a new video would be a lie.
    pub fn reset(&self, duration: f64) {
        let imp = self.imp();
        imp.duration.set(duration.max(0.0));
        imp.tiles.replace(vec![None; TILE_COUNT]);
        imp.start.set(0.0);
        imp.end.set(duration.max(0.0));
        imp.playhead.set(0.0);
        self.queue_draw();
    }

    /// Hand over one finished thumbnail. Out-of-range indices are ignored so a
    /// late arrival from a previous video cannot paint over the current one.
    pub fn set_tile(&self, index: usize, texture: gdk::Texture) {
        let mut tiles = self.imp().tiles.borrow_mut();
        if let Some(slot) = tiles.get_mut(index) {
            *slot = Some(texture);
            drop(tiles);
            self.queue_draw();
        }
    }

    pub fn duration(&self) -> f64 {
        self.imp().duration.get()
    }

    /// Show a range. Silent by design — see the module docs.
    pub fn set_range(&self, start: f64, end: f64) {
        let imp = self.imp();
        imp.start.set(start);
        imp.end.set(end);
        self.queue_draw();
    }

    pub fn set_playhead(&self, seconds: f64) {
        self.imp().playhead.set(seconds);
        self.queue_draw();
    }

    pub fn connect_scrub(&self, f: impl Fn(f64) + 'static) {
        self.imp().on_scrub.replace(Some(Box::new(f)));
    }

    pub fn connect_range_changed(&self, f: impl Fn(f64, f64) + 'static) {
        self.imp().on_range.replace(Some(Box::new(f)));
    }

    // ------------------------------------------------------------- geometry

    fn x_of(&self, seconds: f64) -> f64 {
        x_of(seconds, self.duration(), self.width() as f64)
    }

    fn time_at(&self, x: f64) -> f64 {
        time_at(x, self.duration(), self.width() as f64)
    }

    fn grip_at(&self, x: f64) -> Grip {
        let imp = self.imp();
        grip_at(
            x,
            self.x_of(imp.start.get()),
            self.x_of(imp.end.get()),
        )
    }

    // ---------------------------------------------------------- interaction

    fn setup_gestures(&self) {
        let drag = gtk::GestureDrag::new();
        drag.connect_drag_begin(glib::clone!(
            #[weak(rename_to = strip)]
            self,
            move |_, x, _| {
                if strip.duration() <= 0.0 {
                    return;
                }
                let grip = strip.grip_at(x);
                strip.imp().grip.set(Some(grip));
                strip.apply_drag(grip, x);
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = strip)]
            self,
            move |gesture, dx, _| {
                let Some(grip) = strip.imp().grip.get() else {
                    return;
                };
                if let Some((start_x, _)) = gesture.start_point() {
                    strip.apply_drag(grip, start_x + dx);
                }
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak(rename_to = strip)]
            self,
            move |_, _, _| strip.imp().grip.set(None)
        ));
        self.add_controller(drag);

        // Say which handle the pointer would take before it is pressed.
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = strip)]
            self,
            move |_, x, _| {
                let name = match strip.duration() > 0.0 && strip.grip_at(x) != Grip::Playhead {
                    true => "col-resize",
                    false => "pointer",
                };
                strip.set_cursor_from_name(Some(name));
            }
        ));
        self.add_controller(motion);
    }

    /// Move whatever is being dragged to `x` and tell the window.
    ///
    /// The handles cannot cross: pushing Start past End leaves them touching
    /// rather than inverted, which is a range the job would refuse anyway.
    fn apply_drag(&self, grip: Grip, x: f64) {
        let imp = self.imp();
        let time = self.time_at(x);

        match grip {
            Grip::Start => imp.start.set(time.min(imp.end.get())),
            Grip::End => imp.end.set(time.max(imp.start.get())),
            Grip::Playhead => {}
        }
        imp.playhead.set(time);
        self.queue_draw();

        if grip != Grip::Playhead {
            if let Some(f) = imp.on_range.borrow().as_ref() {
                f(imp.start.get(), imp.end.get());
            }
        }
        if let Some(f) = imp.on_scrub.borrow().as_ref() {
            f(time);
        }
    }

    // ------------------------------------------------------------- painting

    /// Rounded to match the cards and the preview above it. Everything is drawn
    /// inside one clip, so the split from [`draw_contents`] is what keeps the
    /// push and the pop balanced across its early returns.
    ///
    /// [`draw_contents`]: Self::draw_contents
    fn draw(&self, snapshot: &gtk::Snapshot) {
        let (w, h) = (self.width() as f32, self.height() as f32);
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let bounds = graphene::Rect::new(0.0, 0.0, w, h);
        snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(bounds, 8.0));
        self.draw_contents(snapshot, w, h);
        snapshot.pop();
    }

    fn draw_contents(&self, snapshot: &gtk::Snapshot, w: f32, h: f32) {
        let imp = self.imp();

        // A slot for every tile, whether or not its thumbnail has arrived, so
        // the strip has its final shape from the first frame instead of growing
        // into it.
        snapshot.append_color(
            &gdk::RGBA::new(0.0, 0.0, 0.0, 0.16),
            &graphene::Rect::new(0.0, 0.0, w, h),
        );

        let (left, span) = track(w as f64);
        let (left, span) = (left as f32, span as f32);

        let tiles = imp.tiles.borrow();
        if !tiles.is_empty() {
            // Laid across the track, not the widget, so tile and handle agree
            // about where a moment is.
            let tile_w = span / tiles.len() as f32;
            for (i, tile) in tiles.iter().enumerate() {
                let Some(texture) = tile else { continue };
                let slot = graphene::Rect::new(left + i as f32 * tile_w, 0.0, tile_w, h);
                // Thumbnails come back at the video's own aspect ratio, which
                // is wider than its slice of the strip. Fill the slot's height
                // and let the clip take the overhang, rather than squashing the
                // picture to fit.
                let aspect = texture.width() as f32 / texture.height().max(1) as f32;
                let drawn_w = h * aspect;
                snapshot.push_clip(&slot);
                snapshot.append_texture(
                    texture,
                    &graphene::Rect::new(slot.x() + (tile_w - drawn_w) / 2.0, 0.0, drawn_w, h),
                );
                snapshot.pop();
            }
        }
        drop(tiles);

        if self.duration() <= 0.0 {
            return;
        }

        // Everything outside the selection goes dim. This is what makes the
        // chosen range readable at a glance; the handles only say where it ends.
        let (start_x, end_x) = (self.x_of(imp.start.get()) as f32, self.x_of(imp.end.get()) as f32);
        let shade = gdk::RGBA::new(0.0, 0.0, 0.0, 0.55);
        snapshot.append_color(&shade, &graphene::Rect::new(0.0, 0.0, start_x, h));
        snapshot.append_color(&shade, &graphene::Rect::new(end_x, 0.0, w - end_x, h));

        self.draw_playhead(snapshot, h);

        let accent = adw::StyleManager::default().accent_color_rgba();
        self.draw_handle(snapshot, start_x, h, &accent);
        self.draw_handle(snapshot, end_x, h, &accent);
    }

    /// A hairline where the last scrub landed, so the big preview above has a
    /// visible source. White over black so it reads on any frame.
    fn draw_playhead(&self, snapshot: &gtk::Snapshot, h: f32) {
        let x = self.x_of(self.imp().playhead.get()) as f32;
        snapshot.append_color(
            &gdk::RGBA::new(0.0, 0.0, 0.0, 0.65),
            &graphene::Rect::new(x - 1.5, 0.0, 3.0, h),
        );
        snapshot.append_color(
            &gdk::RGBA::new(1.0, 1.0, 1.0, 0.95),
            &graphene::Rect::new(x - 0.5, 0.0, 1.0, h),
        );
    }

    /// A full-height bar with a rounded grip at its middle.
    ///
    /// Both are laid over a dark edge one pixel proud of them. The accent
    /// colour alone disappears against a bright frame — and half these
    /// thumbnails are bright — so the outline is what keeps the handle legible
    /// whatever it happens to be standing on.
    fn draw_handle(&self, snapshot: &gtk::Snapshot, x: f32, h: f32, accent: &gdk::RGBA) {
        const BAR: f32 = 3.0;
        const GRIP_W: f32 = 9.0;
        const GRIP_H: f32 = 26.0;

        let edge = gdk::RGBA::new(0.0, 0.0, 0.0, 0.55);
        snapshot.append_color(
            &edge,
            &graphene::Rect::new(x - BAR / 2.0 - 1.0, 0.0, BAR + 2.0, h),
        );
        snapshot.append_color(accent, &graphene::Rect::new(x - BAR / 2.0, 0.0, BAR, h));

        let grip_h = GRIP_H.min(h);
        let grip = graphene::Rect::new(x - GRIP_W / 2.0, (h - grip_h) / 2.0, GRIP_W, grip_h);
        let outline = graphene::Rect::new(
            grip.x() - 1.0,
            grip.y() - 1.0,
            GRIP_W + 2.0,
            grip_h + 2.0,
        );
        snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(outline, (GRIP_W + 2.0) / 2.0));
        snapshot.append_color(&edge, &outline);
        snapshot.pop();
        snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(grip, GRIP_W / 2.0));
        snapshot.append_color(accent, &grip);
        snapshot.pop();
    }
}

// ------------------------------------------------------------------ geometry
//
// Free functions rather than methods so the mapping between a moment in the
// video and a pixel on screen can be tested without a display. Everything above
// that has to reason about position goes through these three.

/// The band the video is laid out across: the full width less [`EDGE`] at each
/// end. Narrow widgets give up the margin rather than collapse the track.
fn track(width: f64) -> (f64, f64) {
    let inset = EDGE.min(width / 4.0).max(0.0);
    (inset, (width - inset * 2.0).max(0.0))
}

/// Where a time sits horizontally. A video of no length puts everything at the
/// start of the track rather than dividing by zero.
fn x_of(seconds: f64, duration: f64, width: f64) -> f64 {
    let (left, span) = track(width);
    if duration <= 0.0 {
        return left;
    }
    left + (seconds / duration).clamp(0.0, 1.0) * span
}

/// The moment under a pixel. Presses outside the track — a drag can travel past
/// either edge, and the margins are outside it too — clamp to the ends instead
/// of running off the video.
fn time_at(x: f64, duration: f64, width: f64) -> f64 {
    let (left, span) = track(width);
    if span <= 0.0 {
        return 0.0;
    }
    ((x - left) / span).clamp(0.0, 1.0) * duration
}

/// What a press at `x` is reaching for: whichever handle is within
/// [`GRAB_SLOP`], nearest first, otherwise a plain seek.
fn grip_at(x: f64, start_x: f64, end_x: f64) -> Grip {
    let (to_start, to_end) = ((x - start_x).abs(), (x - end_x).abs());
    if to_start.min(to_end) > GRAB_SLOP {
        return Grip::Playhead;
    }
    if to_start <= to_end {
        Grip::Start
    } else {
        Grip::End
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: f64 = 600.0;
    const DURATION: f64 = 1200.0; // 20 minutes

    #[test]
    fn time_and_pixels_are_inverses() {
        for seconds in [0.0, 1.0, 600.0, 1199.0, DURATION] {
            let round_tripped = time_at(x_of(seconds, DURATION, WIDTH), DURATION, WIDTH);
            assert!(
                (round_tripped - seconds).abs() < 0.001,
                "{seconds} came back as {round_tripped}"
            );
        }
    }

    #[test]
    fn positions_outside_the_widget_clamp_to_the_video() {
        // A drag does not stop at the edge, and neither handle may leave the
        // video: -50px is the start, and past the right edge is the end.
        assert_eq!(time_at(-50.0, DURATION, WIDTH), 0.0);
        assert_eq!(time_at(WIDTH + 50.0, DURATION, WIDTH), DURATION);
        assert_eq!(x_of(-5.0, DURATION, WIDTH), EDGE);
        assert_eq!(x_of(DURATION * 2.0, DURATION, WIDTH), WIDTH - EDGE);
    }

    /// The whole point of the inset: on opening, both handles sit at the ends
    /// of an untrimmed video, and both have to be drawable in full.
    #[test]
    fn handles_at_the_extremes_stay_clear_of_the_edges() {
        assert!(x_of(0.0, DURATION, WIDTH) >= EDGE);
        assert!(x_of(DURATION, DURATION, WIDTH) <= WIDTH - EDGE);
        // The margins belong to no moment; a press in one clamps to the end.
        assert_eq!(time_at(2.0, DURATION, WIDTH), 0.0);
        assert_eq!(time_at(WIDTH - 2.0, DURATION, WIDTH), DURATION);
    }

    /// Before a video is loaded there is no scale to map anything onto, and
    /// the widget may not have been allocated a width yet.
    #[test]
    fn no_duration_and_no_width_are_answered_rather_than_divided_by() {
        assert_eq!(x_of(30.0, 0.0, WIDTH), EDGE);
        assert_eq!(time_at(300.0, DURATION, 0.0), 0.0);
    }

    /// A widget too narrow for two margins must still map times, not collapse
    /// the track to nothing and divide by zero.
    #[test]
    fn a_cramped_widget_gives_up_its_margins() {
        let narrow = 12.0;
        let (left, span) = track(narrow);
        assert!(span > 0.0, "track collapsed at {narrow}px");
        assert!(left * 2.0 < narrow);
        assert_eq!(time_at(-5.0, DURATION, narrow), 0.0);
        assert_eq!(time_at(narrow + 5.0, DURATION, narrow), DURATION);
    }

    #[test]
    fn a_press_takes_the_nearer_handle() {
        let (start_x, end_x) = (100.0, 400.0);
        assert_eq!(grip_at(102.0, start_x, end_x), Grip::Start);
        assert_eq!(grip_at(396.0, start_x, end_x), Grip::End);
        // Dead centre between two handles that are close together: Start wins,
        // deterministically, rather than by float luck.
        assert_eq!(grip_at(15.0, 10.0, 20.0), Grip::Start);
    }

    #[test]
    fn a_press_in_open_ground_seeks_instead_of_grabbing() {
        assert_eq!(grip_at(250.0, 100.0, 400.0), Grip::Playhead);
        // Just outside the slop, and just inside it.
        assert_eq!(grip_at(100.0 + GRAB_SLOP + 0.1, 100.0, 400.0), Grip::Playhead);
        assert_eq!(grip_at(100.0 + GRAB_SLOP - 0.1, 100.0, 400.0), Grip::Start);
    }

    /// With both handles at the same place — a fresh video before trimming, or
    /// a range squeezed shut — a press must still resolve to one of them.
    #[test]
    fn coincident_handles_still_grab() {
        assert_eq!(grip_at(300.0, 300.0, 300.0), Grip::Start);
    }
}
