// window.rs
//
// Copyright 2020 Christopher Davis <christopherdavis@gnome.org>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.
//
// SPDX-License-Identifier: GPL-3.0-or-later

use gst::prelude::*;
use gstreamer as gst;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use glib::translate::IntoGlib;
use glib::{clone, Enum};
use gtk::CompositeTemplate;

use glib::subclass;
use glib::subclass::prelude::*;
use gtk::prelude::IsA;
use gtk::subclass::prelude::*;
use libadwaita::subclass::prelude::*;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use crate::app::SolanumApplication;
use crate::config;
use crate::i18n::*;
use crate::rain;
use crate::sounds::{self, Cue, CuePlayer};
use crate::timer::Timer;

// The built-in looks of the clock that timer-font can name; the empty value
// is the default look, set in style.css without a class.
static TIMER_FONT_STYLES: &[&str] = &["serif", "mono"];

static RAIN_IMAGE_PATH: &str = "/org/gnome/Solanum/rain/background.jpg";

// File types picked up when cycling through the background images in a folder.
static BACKGROUND_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];

// The looping rain sound: a playbin3 playing the chosen file and the watch on
// its bus. Dropping it stops the sound.
#[derive(Debug)]
pub struct RainSound {
    playbin: gst::Element,
    path: String,
    _bus_watch: gst::bus::BusWatchGuard,
}

impl Drop for RainSound {
    fn drop(&mut self) {
        let _ = self.playbin.set_state(gst::State::Null);
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Enum)]
#[enum_type(name = "SolanumLapType")]
pub enum LapType {
    Pomodoro,
    Break,
}

impl Default for LapType {
    fn default() -> Self {
        Self::Pomodoro
    }
}

mod imp {
    use super::*;

    #[derive(Debug, CompositeTemplate)]
    #[template(resource = "/org/gnome/Solanum/window.ui")]
    pub struct SolanumWindow {
        pub pomodoro_count: Cell<u32>,
        pub timer: Timer,
        pub cues: CuePlayer,
        pub lap_type: Cell<LapType>,

        #[template_child]
        pub lap_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub timer_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub today_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub timer_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub sound_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub menu_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub large_text_bp: TemplateChild<libadwaita::Breakpoint>,
        #[template_child]
        pub huge_text_bp: TemplateChild<libadwaita::Breakpoint>,
        #[template_child]
        pub rain_box: TemplateChild<gtk::Box>,
        pub rain_view: RefCell<Option<gtk::Widget>>,
        pub rain_sound: RefCell<Option<RainSound>>,
        pub timer_font_provider: RefCell<Option<gtk::CssProvider>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SolanumWindow {
        const NAME: &'static str = "SolanumWindow";
        type Type = super::SolanumWindow;
        type ParentType = libadwaita::ApplicationWindow;

        fn new() -> Self {
            Self {
                pomodoro_count: Cell::new(1),
                timer: Timer::new(),
                cues: CuePlayer::new(),
                lap_type: Default::default(),
                lap_label: TemplateChild::default(),
                timer_label: TemplateChild::default(),
                today_label: TemplateChild::default(),
                timer_button: TemplateChild::default(),
                sound_button: TemplateChild::default(),
                menu_button: TemplateChild::default(),
                large_text_bp: TemplateChild::default(),
                huge_text_bp: TemplateChild::default(),
                rain_box: TemplateChild::default(),
                rain_view: RefCell::new(None),
                rain_sound: RefCell::new(None),
                timer_font_provider: RefCell::new(None),
            }
        }

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.install_action("win.toggle-timer", None, move |win, _, _| {
                win.toggle_timer();
            });

            klass.install_action("win.reset", None, move |win, _, _| {
                win.reset();
            });

            klass.install_action("win.reset-today", None, move |win, _, _| {
                win.reset_today();
            });

            klass.install_action("win.next-background", None, move |win, _, _| {
                win.cycle_background(1);
            });

            klass.install_action("win.previous-background", None, move |win, _, _| {
                win.cycle_background(-1);
            });

            klass.install_action("win.toggle-rain-sound", None, move |win, _, _| {
                win.toggle_rain_sound();
            });

            klass.install_action("win.lap-forward", None, move |win, _, _| {
                win.adjust_today(1);
            });

            klass.install_action("win.lap-back", None, move |win, _, _| {
                win.adjust_today(-1);
            });

            klass.install_action("win.skip", None, move |win, _, _| {
                win.next_lap(None);

                if !win.is_active() {
                    win.present();
                }
            });

            // Resizes the window from outside (the content size, as with
            // default-width and default-height), for desktops whose window
            // manager cannot: WSLg windows on Windows ignore resizes from the
            // Windows side. GTK only resizes a mapped window when the default
            // size changes, and resizes by the compositor (such as a snap) do
            // not update it, so go through an unset size first, or asking for
            // the same size twice would do nothing.
            klass.install_action(
                "win.set-size",
                Some(glib::VariantTy::new("(ii)").unwrap()),
                move |win, _, param| {
                    if let Some((width, height)) = param.and_then(|p| p.get::<(i32, i32)>()) {
                        win.set_default_size(-1, -1);
                        win.set_default_size(width, height);
                    }
                },
            );
        }

        fn instance_init(obj: &subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for SolanumWindow {
        fn constructed(&self) {
            self.parent_constructed();

            // The breakpoints only name the size of the text; style.css
            // does the rest.
            for (bp, class) in [
                (&*self.large_text_bp, "large-text"),
                (&*self.huge_text_bp, "huge-text"),
            ] {
                bp.connect_apply(clone!(
                    #[weak(rename_to = win)]
                    self.obj(),
                    move |_| {
                        win.add_css_class(class);
                    }
                ));
                bp.connect_unapply(clone!(
                    #[weak(rename_to = win)]
                    self.obj(),
                    move |_| {
                        win.remove_css_class(class);
                    }
                ));
            }
        }

        fn dispose(&self) {
            // A closed window is a quiet window.
            self.rain_sound.take();
        }
    }

    // We don't need to override any vfuncs here, but since they're superclasses
    // we need to declare the blank impls
    impl WidgetImpl for SolanumWindow {}
    impl WindowImpl for SolanumWindow {}
    impl ApplicationWindowImpl for SolanumWindow {}
    impl AdwApplicationWindowImpl for SolanumWindow {}
}

glib::wrapper! {
    pub struct SolanumWindow(ObjectSubclass<imp::SolanumWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, libadwaita::ApplicationWindow,
        @implements gio::ActionMap, gio::ActionGroup;
}

impl SolanumWindow {
    pub fn new<P: IsA<gtk::Application> + glib::value::ToValue>(app: &P) -> Self {
        let win = glib::Object::builder::<Self>()
            .property("application", app)
            .build();

        win.init();

        // Set icons for shell
        gtk::Window::set_default_icon_name(config::APP_ID);

        win
    }

    fn application(&self) -> SolanumApplication {
        gtk::prelude::GtkWindowExt::application(self)
            .unwrap()
            .downcast::<SolanumApplication>()
            .unwrap()
    }

    fn init(&self) {
        let imp = self.imp();
        let timer_label = &*imp.timer_label;
        let app = self.application();
        let settings = app.gsettings();

        timer_label.set_direction(gtk::TextDirection::Ltr);

        if config::APP_ID.ends_with("Devel") {
            self.add_css_class("devel");
        }

        self.update_lap_label();
        self.update_today_label();

        // The count can also change from outside, e.g. with gsettings.
        for key in ["daily-goal", "laps-today", "laps-date"] {
            settings.connect_changed(
                Some(key),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_, _| {
                        win.update_today_label();
                    }
                ),
            );
        }

        self.setup_rain();
        self.setup_rain_sound();
        self.setup_timer_font();

        let min = settings.uint("lap-length");
        imp.timer.set_duration(min);
        self.set_timer_label_from_secs(min * 60);

        imp.timer.connect_countdown_update(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, minutes, seconds| {
                win.update_countdown(minutes, seconds);
            }
        ));

        imp.timer.connect_lap(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| {
                // Only a work lap that ran out on the clock counts towards the
                // daily goal; skipping a lap does not. The lap that reaches
                // the goal ends with the goal sound.
                let mut cue = Cue::End;
                if win.imp().lap_type.get() == LapType::Pomodoro && win.record_completed_lap() {
                    cue = Cue::Goal;
                }
                win.toggle_timer();
                win.next_lap(Some(cue));
            }
        ));
    }

    fn update_countdown(&self, min: u32, sec: u32) -> glib::ControlFlow {
        let imp = self.imp();
        imp.timer_label.set_label(&Self::clock_text(min, sec));
        glib::ControlFlow::Continue
    }

    fn update_lap(&self, lap_type: LapType) {
        let imp = self.imp();
        let label = &*imp.lap_label;
        let timer = &imp.timer;
        let app = self.application();
        let settings = app.gsettings();

        imp.lap_type.set(lap_type);

        let lap_number = &imp.pomodoro_count;
        println!("Setting lap to {:?}", lap_type);

        match lap_type {
            LapType::Pomodoro => {
                let length = settings.get("lap-length");
                self.update_lap_label();
                timer.set_duration(length);
                self.set_timer_label_from_secs(length * 60);
            }
            LapType::Break => {
                if lap_number.get() >= settings.uint("sessions-until-long-break") {
                    let length = settings.uint("long-break-length");
                    lap_number.set(1);
                    label.set_label(&i18n("Long Break"));
                    timer.set_duration(length);
                    self.set_timer_label_from_secs(length * 60);
                } else {
                    let length = settings.uint("short-break-length");
                    lap_number.set(lap_number.get() + 1);
                    label.set_label(&i18n("Short Break"));
                    timer.set_duration(length);
                    self.set_timer_label_from_secs(length * 60);
                }
            }
        };
    }

    // Callback to run whenever the timer is toggled - by button or action
    fn toggle_timer(&self) {
        let imp = self.imp();
        let app = self.application();
        let settings = app.gsettings();
        let fullscreen = settings.boolean("fullscreen-break");

        let start_timer = !imp.timer.running();
        self.action_set_enabled("win.skip", !start_timer);

        if start_timer {
            let app = self.application();
            app.withdraw_notification("timer-notif");
            imp.timer.start();
            self.play_cue(Cue::Start);
            imp.timer_button
                .set_icon_name("media-playback-pause-symbolic");
            imp.timer_label.remove_css_class("blinking");
            imp.timer_button.remove_css_class("suggested-action");
            if fullscreen {
                if imp.lap_type.get() == LapType::Break {
                    self.fullscreen();
                } else {
                    self.unfullscreen();
                }
            }
        } else {
            imp.timer.stop();
            imp.timer_button
                .set_icon_name("media-playback-start-symbolic");
            imp.timer_label.add_css_class("blinking");
            imp.timer_button.add_css_class("suggested-action");
        }

        // !start_timer = only allow restarting when the timer is paused
        self.action_set_enabled("win.reset", !start_timer)
    }

    // Callback for resetting the application to the initial state.
    fn reset(&self) {
        println!("Resetting to the initial state");
        let imp = self.imp();

        // Reset the user interface to the stopped state.
        imp.timer.stop();
        imp.timer_button
            .set_icon_name("media-playback-start-symbolic");
        imp.timer_label.add_css_class("blinking");
        imp.timer_button.add_css_class("suggested-action");

        imp.pomodoro_count.set(1);
        self.update_lap(LapType::Pomodoro);
    }

    // Util for setting the timer label when given seconds
    fn set_timer_label_from_secs(&self, secs: u32) {
        self.imp()
            .timer_label
            .set_label(&Self::clock_text(secs / 60, secs % 60));
    }

    // MM:SS. A plain colon rather than the ratio sign upstream uses, since
    // not every typeface timer-font can pick has the ratio sign; the
    // typefaces that matter centre their colon between figures themselves.
    fn clock_text(min: u32, sec: u32) -> String {
        format!("{:>02}:{:>02}", min, sec)
    }

    fn play_cue(&self, cue: Cue) {
        let app = self.application();
        if let Some(uri) = sounds::uri(app.gsettings(), cue) {
            self.imp().cues.play_uri(&uri);
        }
    }

    fn send_notifcation(&self, lap_type: LapType, cue: Cue) {
        if !self.is_active() {
            let notif = gio::Notification::new(&i18n("Solanum"));
            // Set notification text based on lap type
            let (title, body, button) = match lap_type {
                LapType::Pomodoro => (
                    i18n("Back to Work"),
                    i18n("Ready to keep working?"),
                    i18n("Start Working"),
                ),
                LapType::Break => (
                    i18n("Break Time"),
                    i18n("Stretch your legs, and drink some water."),
                    i18n("Start Break"),
                ),
            };
            notif.set_title(&title);
            notif.set_body(Some(&body));
            notif.set_priority(gio::NotificationPriority::Urgent);
            notif.add_button(&button, "app.toggle-timer");
            notif.add_button(&i18n("Skip"), "app.skip");
            let app = self.application();
            app.send_notification(Some("timer-notif"), &notif);
        }
        self.play_cue(cue);
    }

    // Today's date as YYYY-MM-DD, the key the daily count is stored under.
    fn today() -> String {
        glib::DateTime::now_local()
            .ok()
            .and_then(|dt| dt.format("%Y-%m-%d").ok())
            .map(|s| s.to_string())
            .unwrap_or_default()
    }

    // Work laps completed today. A count stored under another date is stale
    // and reads as zero, so the counter starts over every day on its own.
    fn laps_today(&self) -> u32 {
        let app = self.application();
        let settings = app.gsettings();
        if settings.string("laps-date").as_str() == Self::today() {
            settings.uint("laps-today")
        } else {
            0
        }
    }

    // Count a completed work lap. Returns true when it is the lap that
    // reaches the daily goal.
    fn record_completed_lap(&self) -> bool {
        let before = self.laps_today();
        let goal = self.application().gsettings().uint("daily-goal");
        self.set_laps_today(before + 1);
        before < goal && before + 1 >= goal
    }

    // Step the day's count by hand, e.g. for laps done while the app was
    // closed. It never goes below zero, and leaves the running timer and
    // the laps until the long break alone.
    fn adjust_today(&self, delta: i32) {
        self.set_laps_today(self.laps_today().saturating_add_signed(delta));
    }

    fn reset_today(&self) {
        self.set_laps_today(0);
    }

    // Store the count under today's date, so it is not read as stale.
    fn set_laps_today(&self, count: u32) {
        let app = self.application();
        let settings = app.gsettings();
        let _ = settings.set_string("laps-date", &Self::today());
        let _ = settings.set_uint("laps-today", count);
        self.update_today_label();
    }

    fn update_today_label(&self) {
        let imp = self.imp();
        let app = self.application();
        let goal = app.gsettings().uint("daily-goal");
        let done = self.laps_today();

        // Translators: {} are the work laps completed today and the daily
        // goal, e.g. "Today 3/10".
        imp.today_label.set_label(&i18n_f(
            "Today {}/{}",
            &[&done.to_string(), &goal.to_string()],
        ));

        if done >= goal {
            imp.today_label.add_css_class("success");
        } else {
            imp.today_label.remove_css_class("success");
        }

        self.action_set_enabled("win.lap-back", done > 0);
    }

    fn setup_rain(&self) {
        let app = self.application();
        let settings = app.gsettings();

        for key in ["rain-background", "rain-image"] {
            settings.connect_changed(
                Some(key),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_, _| {
                        win.update_rain();
                    }
                ),
            );
        }

        self.update_rain();
    }

    // Drop the current rain view, if any, and build a new one when the rain
    // background is enabled. Without it the window looks as it always did.
    fn update_rain(&self) {
        let imp = self.imp();

        if let Some(view) = imp.rain_view.take() {
            imp.rain_box.remove(&view);
        }
        // Over the rain the labels and buttons have a look of their own.
        self.remove_css_class("rainy");

        let app = self.application();
        let settings = app.gsettings();
        if !settings.boolean("rain-background") {
            return;
        }

        let Some(view) = self.new_rain_view(&settings.string("rain-image")) else {
            return;
        };

        imp.rain_box.append(&view);
        imp.rain_view.replace(Some(view));
        self.add_css_class("rainy");
    }

    // The animated rain: raindrop-fx ported to OpenGL (src/rain).
    fn new_rain_view(&self, image_path: &str) -> Option<gtk::Widget> {
        let (image, _) = Self::rain_image(image_path)?;
        rain::new_view(&image)
    }

    fn setup_timer_font(&self) {
        let app = self.application();
        app.gsettings().connect_changed(
            Some("timer-font"),
            clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| {
                    win.update_timer_font();
                }
            ),
        );
        self.update_timer_font();
    }

    // timer-font is either empty (the default look), the name of one of the
    // other built-in looks, which style.css keys on a class of the window,
    // or a font description such as "Noto Sans Light" for the clock alone.
    fn update_timer_font(&self) {
        let imp = self.imp();
        let app = self.application();
        let value = app.gsettings().string("timer-font");
        let value = value.trim();

        for style in TIMER_FONT_STYLES {
            self.remove_css_class(&format!("font-{}", style));
        }
        if let Some(provider) = imp.timer_font_provider.take() {
            gtk::style_context_remove_provider_for_display(&WidgetExt::display(self), &provider);
        }

        if value.is_empty() || value == "display" {
            return;
        }

        if TIMER_FONT_STYLES.contains(&value) {
            self.add_css_class(&format!("font-{}", value));
            return;
        }

        let desc = gtk::pango::FontDescription::from_string(value);
        let Some(family) = desc.family() else {
            return;
        };
        let mut css = format!(
            ".timer-label {{ font-family: \"{}\"; font-variation-settings: normal;",
            family.replace(['"', '\\'], "")
        );
        if desc.set_fields().contains(gtk::pango::FontMask::WEIGHT) {
            css.push_str(&format!(" font-weight: {};", desc.weight().into_glib()));
        }
        if desc.set_fields().contains(gtk::pango::FontMask::STYLE)
            && desc.style() != gtk::pango::Style::Normal
        {
            css.push_str(" font-style: italic;");
        }
        css.push_str(" }");

        let provider = gtk::CssProvider::new();
        provider.load_from_string(&css);
        gtk::style_context_add_provider_for_display(
            &WidgetExt::display(self),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        imp.timer_font_provider.replace(Some(provider));
    }

    // The image seen through the rain: the file in rain-image when it can be
    // read, otherwise the built-in one.
    fn rain_image(path: &str) -> Option<(Vec<u8>, String)> {
        if !path.is_empty() {
            match std::fs::read(path) {
                Ok(data) => {
                    let (content_type, _) = gio::content_type_guess(Some(path), &data);
                    let mime = gio::content_type_get_mime_type(&content_type)
                        .map(|m| m.to_string())
                        .unwrap_or_default();
                    if mime.starts_with("image/") {
                        return Some((data, mime));
                    }
                    glib::g_warning!("solanum", "{} is not an image ({})", path, mime);
                }
                Err(err) => {
                    glib::g_warning!("solanum", "Could not read {}: {}", path, err);
                }
            }
        }

        match gio::resources_lookup_data(RAIN_IMAGE_PATH, gio::ResourceLookupFlags::NONE) {
            Ok(bytes) => Some((bytes.to_vec(), "image/jpeg".to_owned())),
            Err(err) => {
                glib::g_warning!("solanum", "Could not load the rain image: {}", err);
                None
            }
        }
    }

    fn setup_rain_sound(&self) {
        let imp = self.imp();
        let app = self.application();
        let settings = app.gsettings();

        let button = &*imp.sound_button;
        settings.bind("rain-sound", button, "active").build();
        button.connect_toggled(Self::update_sound_button);
        Self::update_sound_button(button);

        settings.connect_changed(
            Some("rain-sound-file"),
            clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| {
                    win.update_rain_sound();
                }
            ),
        );

        settings.connect_changed(
            Some("rain-sound"),
            clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| {
                    win.update_rain_mute();
                }
            ),
        );

        settings.connect_changed(
            Some("rain-volume"),
            clone!(
                #[weak(rename_to = win)]
                self,
                move |settings, _| {
                    if let Some(sound) = win.imp().rain_sound.borrow().as_ref() {
                        sound
                            .playbin
                            .set_property("volume", Self::rain_volume(settings));
                    };
                }
            ),
        );

        self.update_rain_sound();
    }

    fn update_sound_button(button: &gtk::ToggleButton) {
        button.set_icon_name(if button.is_active() {
            "audio-volume-high-symbolic"
        } else {
            "audio-volume-muted-symbolic"
        });
    }

    fn toggle_rain_sound(&self) {
        let app = self.application();
        let settings = app.gsettings();
        let _ = settings.set_boolean("rain-sound", !settings.boolean("rain-sound"));
    }

    fn rain_volume(settings: &gio::Settings) -> f64 {
        f64::from(settings.uint("rain-volume").min(100)) / 100.0
    }

    // The rain sound plays for as long as the window is open whenever a file
    // is chosen, and rain-sound only mutes and unmutes it, so turning it on
    // carries on where the rain is rather than starting the file over.
    // Choosing another file starts that one from the beginning.
    fn update_rain_sound(&self) {
        let imp = self.imp();
        let app = self.application();
        let settings = app.gsettings();

        let path = settings.string("rain-sound-file");

        if let Some(sound) = imp.rain_sound.borrow().as_ref() {
            if sound.path == path.as_str() {
                return;
            }
        }

        imp.rain_sound.take();

        if path.is_empty() {
            self.update_rain_mute();
            return;
        }

        let muted = !settings.boolean("rain-sound");
        match self.start_rain_sound(&path, Self::rain_volume(settings), muted) {
            Ok(sound) => {
                imp.rain_sound.replace(Some(sound));
            }
            Err(err) => {
                eprintln!("Could not play the rain sound \"{}\": {}", path, err);
                self.rain_sound_failed(None);
            }
        }
    }

    fn update_rain_mute(&self) {
        let imp = self.imp();
        let app = self.application();
        let audible = app.gsettings().boolean("rain-sound");

        if let Some(sound) = imp.rain_sound.borrow().as_ref() {
            sound.playbin.set_property("mute", !audible);
            return;
        }

        // Nothing to hear without a file that plays.
        if audible {
            self.rain_sound_failed(None);
        }
    }

    fn start_rain_sound(&self, path: &str, volume: f64, muted: bool) -> Result<RainSound, String> {
        std::fs::File::open(path).map_err(|err| err.to_string())?;
        let uri = glib::filename_to_uri(path, None).map_err(|err| err.to_string())?;

        let playbin = gst::ElementFactory::make("playbin3")
            .build()
            .map_err(|err| err.to_string())?;
        // Audio only, so a file with cover art never opens a video window.
        // soft-volume gives playbin a volume element of its own for sinks
        // that cannot take volume and mute themselves (osxaudiosink on
        // macOS); the pulsesink below can, and then takes both as before.
        playbin.set_property_from_str("flags", "audio+soft-volume");
        playbin.set_property("uri", &uri);

        // Play through PulseAudio (or PipeWire's server for it), whose
        // streams take volume and mute themselves: playbin3 hands both to
        // the sink, so muting and unmuting is heard at once rather than
        // after the audio already buffered. The small buffer keeps the
        // rest of the way short too. Without state.restore-props the
        // session manager would remember the volume and mute of our stream
        // for the application and apply them to the chime as well.
        match gst::ElementFactory::make("pulsesink")
            .property("buffer-time", 50_000i64)
            .property("latency-time", 10_000i64)
            .property(
                "stream-properties",
                gst::Structure::builder("props")
                    .field("state.restore-props", "false")
                    .build(),
            )
            .build()
        {
            Ok(sink) => playbin.set_property("audio-sink", &sink),
            Err(err) => eprintln!(
                "No pulsesink for the rain sound, using the default: {}",
                err
            ),
        }
        playbin.set_property("volume", volume);
        playbin.set_property("mute", muted);

        // Queue the same file again shortly before it ends. playbin3 then
        // moves on to it without a gap, so the rain loops seamlessly.
        let next_uri = uri.to_string();
        playbin.connect("about-to-finish", false, move |args| {
            if let Ok(playbin) = args[0].get::<gst::Element>() {
                playbin.set_property("uri", &next_uri);
            }
            None
        });

        let bus = playbin
            .bus()
            .ok_or_else(|| "playbin3 has no bus".to_owned())?;
        let bus_watch = bus
            .add_watch_local(clone!(
                #[weak(rename_to = win)]
                self,
                #[weak]
                playbin,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move |_, msg| {
                    match msg.view() {
                        gst::MessageView::Error(err) => {
                            eprintln!(
                                "Rain sound error from {}: {} ({})",
                                err.src()
                                    .map(|src| src.path_string().to_string())
                                    .unwrap_or_default(),
                                err.error(),
                                err.debug().unwrap_or_default()
                            );
                            win.rain_sound_failed(Some(&playbin));
                        }
                        gst::MessageView::Eos(_) => {
                            // Not expected with the file queued again above,
                            // but keep the rain going if it happens.
                            let _ = playbin.seek_simple(
                                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                                gst::ClockTime::ZERO,
                            );
                        }
                        _ => (),
                    }
                    glib::ControlFlow::Continue
                }
            ))
            .map_err(|err| err.to_string())?;

        let sound = RainSound {
            playbin,
            path: path.to_owned(),
            _bus_watch: bus_watch,
        };
        sound
            .playbin
            .set_state(gst::State::Playing)
            .map_err(|err| err.to_string())?;

        Ok(sound)
    }

    // The rain sound cannot be played: drop the player that failed, if any,
    // and turn the setting off so the button does not look active for
    // nothing. Done from an idle callback, since this runs inside a handler
    // for the same setting or inside the bus watch of the player.
    fn rain_sound_failed(&self, playbin: Option<&gst::Element>) {
        let settings = self.application().gsettings().clone();
        let failed = playbin.map(|playbin| playbin.downgrade());
        glib::idle_add_local_once(clone!(
            #[weak(rename_to = win)]
            self,
            move || {
                let imp = win.imp();
                if let Some(failed) = failed.and_then(|failed| failed.upgrade()) {
                    let current = imp
                        .rain_sound
                        .borrow()
                        .as_ref()
                        .is_some_and(|sound| sound.playbin == failed);
                    if current {
                        imp.rain_sound.take();
                    }
                }
                let _ = settings.set_boolean("rain-sound", false);
            }
        ));
    }

    // The images in a folder, sorted by name.
    fn backgrounds_in(dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };

        let mut files: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| {
                            BACKGROUND_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
                        })
            })
            .collect();
        files.sort();
        files
    }

    // Step to the next (1) or previous (-1) image in the folder of the current
    // background, wrapping around. With the built-in image the folder is
    // $XDG_DATA_HOME/solanum/backgrounds.
    fn cycle_background(&self, step: isize) {
        let app = self.application();
        let settings = app.gsettings();
        let current = PathBuf::from(settings.string("rain-image").as_str());

        let dir = if current.as_os_str().is_empty() {
            glib::user_data_dir().join("solanum").join("backgrounds")
        } else {
            match current.parent() {
                Some(dir) => dir.to_path_buf(),
                None => return,
            }
        };

        let files = Self::backgrounds_in(&dir);
        if files.is_empty() {
            return;
        }

        let index = match files.iter().position(|file| *file == current) {
            Some(i) => (i as isize + step).rem_euclid(files.len() as isize) as usize,
            None => 0,
        };

        let _ = settings.set_string("rain-image", &files[index].to_string_lossy());
    }

    fn update_lap_label(&self) {
        let imp = self.imp();

        // Translators: Every pomodoro session can range from 1-99 laps,
        // so {} will contain a number between 1 and 99. Lap is always singular.
        imp.lap_label.set_label(&ni18n_f(
            "Lap {}",
            "Lap {}",
            imp.pomodoro_count.get(),
            &[&imp.pomodoro_count.get().to_string()],
        ));
    }

    // Move to the next lap, with a notification and the given sound when the
    // lap ran out on the clock
    fn next_lap(&self, notify: Option<Cue>) {
        let imp = self.imp();
        let lap_type = imp.lap_type.get();

        let next_lap = if lap_type == LapType::Pomodoro {
            LapType::Break
        } else {
            LapType::Pomodoro
        };

        self.update_lap(next_lap);

        if let Some(cue) = notify {
            self.send_notifcation(next_lap, cue);
        }
    }
}
