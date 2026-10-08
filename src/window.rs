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

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use webkit6::prelude::*;

use glib::{clone, Enum};
use gtk::CompositeTemplate;

use glib::subclass;
use glib::subclass::prelude::*;
use gtk::prelude::IsA;
use gtk::subclass::prelude::*;
use libadwaita::subclass::prelude::*;

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};

use crate::app::SolanumApplication;
use crate::config;
use crate::i18n::*;
use crate::timer::Timer;

static CHIME_URI: &str = "resource:///org/gnome/Solanum/chime.ogg";
static BEEP_URI: &str = "resource:///org/gnome/Solanum/beep.ogg";

static RAIN_SCRIPT_PATH: &str = "/org/gnome/Solanum/rain/raindrop-fx.js";
static RAIN_IMAGE_PATH: &str = "/org/gnome/Solanum/rain/background.jpg";

// File types picked up when cycling through the background images in a folder.
static BACKGROUND_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];

// The rain page: raindrop-fx (https://github.com/SardineFish/raindrop-fx, MIT)
// renders drops running down a pane of glass in WebGL, with the background
// image blurred behind it. @SCRIPT@ and @BACKGROUND@ are filled in at runtime;
// the image is passed as a data: URI so WebGL can use it as a texture without
// any file access from the page.
static RAIN_HTML: &str = r##"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<style>
  html, body { margin: 0; background: #000; overflow: hidden; }
  #canvas { position: fixed; inset: 0; width: 100vw; height: 100vh; display: block; }
  /* Darken the scene so the timer stays readable, a little more in the
     middle where the labels are. This is done here rather than with a
     text-shadow in GTK, which would be re-rendered on every frame of the
     rain and triple the CPU time of the window. */
  #shade {
    position: fixed; inset: 0;
    background: radial-gradient(ellipse 50% 40% at 50% 50%, rgba(0, 0, 0, 0.6), rgba(0, 0, 0, 0.35));
  }
</style>
</head>
<body>
<canvas id="canvas"></canvas>
<div id="shade"></div>
<script>@SCRIPT@</script>
<script>
  (() => {
    const canvas = document.querySelector("#canvas");
    let raindropFx = null;
    let ready = false;

    const resize = () => {
      if (!ready)
        return;
      const rect = canvas.getBoundingClientRect();
      if (rect.width >= 1 && rect.height >= 1)
        raindropFx.resize(rect.width, rect.height);
    };

    // raindrop-fx advances the rain by a fixed 0.03 s per animation frame,
    // which ties its speed to the refresh rate of the monitor. Drive it here
    // instead: at most about 60 updates per second, each scaled by the time
    // since the last one, so it falls as it does at 60 Hz on any monitor
    // and does not burn GPU and CPU time on frames nobody needs.
    const frameMs = 1000 / 60;
    let last = 0;
    const tick = (now) => {
      requestAnimationFrame(tick);
      const elapsed = now - last;
      if (elapsed < frameMs - 3)
        return;
      last = now;
      const dt = Math.min(0.03 * elapsed / frameMs, 0.06);
      raindropFx.update({ dt: dt, total: now / 1000 });
    };

    // The view may not have been allocated yet when the page loads, and the
    // simulation is set up for the canvas size it starts with, so wait for
    // a real size first.
    const boot = () => {
      const rect = canvas.getBoundingClientRect();
      if (rect.width < 1 || rect.height < 1) {
        setTimeout(boot, 100);
        return;
      }
      canvas.width = rect.width;
      canvas.height = rect.height;
      raindropFx = new RaindropFX({ canvas: canvas, background: "@BACKGROUND@" });
      // start() loads the textures and then begins its own frame loop,
      // which is swapped for the one above.
      raindropFx.start().then(() => {
        raindropFx.stop();
        ready = true;
        resize();
        requestAnimationFrame(tick);
      });
    };

    window.onresize = resize;
    boot();
  })();
</script>
</body>
</html>
"##;

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
        pub player: gstreamer_play::Play,
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
        pub menu_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub large_text_bp: TemplateChild<libadwaita::Breakpoint>,
        #[template_child]
        pub rain_box: TemplateChild<gtk::Box>,
        pub rain_view: RefCell<Option<webkit6::WebView>>,
        pub rain_session: OnceCell<webkit6::NetworkSession>,
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
                player: gstreamer_play::Play::new(None::<gstreamer_play::PlayVideoRenderer>),
                lap_type: Default::default(),
                lap_label: TemplateChild::default(),
                timer_label: TemplateChild::default(),
                today_label: TemplateChild::default(),
                timer_button: TemplateChild::default(),
                menu_button: TemplateChild::default(),
                large_text_bp: TemplateChild::default(),
                rain_box: TemplateChild::default(),
                rain_view: RefCell::new(None),
                rain_session: OnceCell::new(),
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

            klass.install_action("win.skip", None, move |win, _, _| {
                win.next_lap(false);

                if !win.is_active() {
                    win.present();
                }
            });
        }

        fn instance_init(obj: &subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for SolanumWindow {
        fn constructed(&self) {
            self.parent_constructed();

            let timer_label = &*self.timer_label;
            let lap_label = &*self.lap_label;
            let today_label = &*self.today_label;
            self.large_text_bp.connect_apply(clone!(
                #[weak]
                timer_label,
                #[weak]
                lap_label,
                #[weak]
                today_label,
                move |_| {
                    timer_label.add_css_class("large-timer");
                    lap_label.remove_css_class("heading");
                    lap_label.add_css_class("title-4");
                    today_label.remove_css_class("heading");
                    today_label.add_css_class("title-4");
                }
            ));

            self.large_text_bp.connect_unapply(clone!(
                #[weak]
                timer_label,
                #[weak]
                lap_label,
                #[weak]
                today_label,
                move |_| {
                    timer_label.remove_css_class("large-timer");
                    lap_label.remove_css_class("title-4");
                    lap_label.add_css_class("heading");
                    today_label.remove_css_class("title-4");
                    today_label.add_css_class("heading");
                }
            ));
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

        settings.connect_changed(
            Some("daily-goal"),
            clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| {
                    win.update_today_label();
                }
            ),
        );

        self.setup_rain();

        let min = settings.uint("lap-length");
        imp.timer.set_duration(min);
        timer_label.set_label(&format!("{:>02}∶00", min));

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
                // daily goal; skipping a lap does not.
                if win.imp().lap_type.get() == LapType::Pomodoro {
                    win.record_completed_lap();
                }
                win.toggle_timer();
                win.next_lap(true);
            }
        ));
    }

    fn update_countdown(&self, min: u32, sec: u32) -> glib::ControlFlow {
        let imp = self.imp();
        let label = &*imp.timer_label;
        label.set_label(&format!("{:>02}∶{:>02}", min, sec));
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
            self.play_sound(BEEP_URI);
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
        let imp = self.imp();
        let label = &*imp.timer_label;
        let min = secs / 60;
        let secs = secs % 60;
        label.set_label(&format!("{:>02}∶{:>02}", min, secs));
    }

    fn play_sound(&self, uri: &str) {
        let player = &self.imp().player;
        player.set_uri(Some(uri));
        player.play();
    }

    fn send_notifcation(&self, lap_type: LapType) {
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
        self.play_sound(CHIME_URI);
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

    fn record_completed_lap(&self) {
        let count = self.laps_today() + 1;
        let app = self.application();
        let settings = app.gsettings();
        let _ = settings.set_string("laps-date", &Self::today());
        let _ = settings.set_uint("laps-today", count);
        self.update_today_label();
    }

    fn reset_today(&self) {
        let app = self.application();
        let settings = app.gsettings();
        let _ = settings.set_string("laps-date", &Self::today());
        let _ = settings.set_uint("laps-today", 0);
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

        let app = self.application();
        let settings = app.gsettings();
        if !settings.boolean("rain-background") {
            return;
        }

        let Some(html) = Self::rain_html(&settings.string("rain-image")) else {
            return;
        };

        let web_settings = webkit6::Settings::new();
        web_settings.set_enable_webgl(true);
        web_settings
            .set_hardware_acceleration_policy(webkit6::HardwareAccelerationPolicy::Always);

        // An ephemeral session keeps WebKit from writing caches and storage
        // next to our own data in $XDG_DATA_HOME/solanum.
        let session = imp
            .rain_session
            .get_or_init(webkit6::NetworkSession::new_ephemeral);

        let view = webkit6::WebView::builder()
            .network_session(session)
            .settings(&web_settings)
            .hexpand(true)
            .vexpand(true)
            .build();
        // The view is only decoration: clicks and keys belong to the timer.
        view.set_can_focus(false);
        view.set_can_target(false);
        // Black instead of a white flash until the page has painted.
        view.set_background_color(&gdk::RGBA::BLACK);
        view.load_html(&html, None);

        imp.rain_box.append(&view);
        imp.rain_view.replace(Some(view));
    }

    fn rain_html(image_path: &str) -> Option<String> {
        let script =
            match gio::resources_lookup_data(RAIN_SCRIPT_PATH, gio::ResourceLookupFlags::NONE) {
                Ok(bytes) => bytes,
                Err(err) => {
                    glib::g_warning!("solanum", "Could not load the rain script: {}", err);
                    return None;
                }
            };
        let script = String::from_utf8_lossy(&script);

        let (image, mime) = Self::rain_image(image_path)?;
        let background = format!("data:{};base64,{}", mime, glib::base64_encode(&image));

        Some(
            RAIN_HTML
                .replace("@BACKGROUND@", &background)
                .replace("@SCRIPT@", &script),
        )
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

    // Move to the next lap
    fn next_lap(&self, notify: bool) {
        let imp = self.imp();
        let lap_type = imp.lap_type.get();

        let next_lap = if lap_type == LapType::Pomodoro {
            LapType::Break
        } else {
            LapType::Pomodoro
        };

        self.update_lap(next_lap);

        if notify {
            self.send_notifcation(next_lap);
        }
    }
}
