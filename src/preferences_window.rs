// preferences_window.rs
//
// Copyright 2021 Christopher Davis <christopherdavis@gnome.org>
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

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;
use gtk::{gio, glib};
use libadwaita::prelude::*;
use libadwaita::subclass::prelude::*;

use std::cell::{Cell, OnceCell, RefCell};
use std::path::Path;

use crate::i18n::*;
use crate::sounds::{self, Cue, CuePlayer};

static CUES: [Cue; 3] = [Cue::Start, Cue::End, Cue::Goal];

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/org/gnome/Solanum/preferences-window.ui")]
    pub struct SolanumPreferencesWindow {
        #[template_child]
        pub lap_spin: TemplateChild<libadwaita::SpinRow>,
        #[template_child]
        pub short_break_spin: TemplateChild<libadwaita::SpinRow>,
        #[template_child]
        pub long_break_spin: TemplateChild<libadwaita::SpinRow>,
        #[template_child]
        pub session_count_spin: TemplateChild<libadwaita::SpinRow>,
        #[template_child]
        pub daily_goal_spin: TemplateChild<libadwaita::SpinRow>,
        #[template_child]
        pub fullscreen_switch: TemplateChild<libadwaita::SwitchRow>,
        #[template_child]
        pub rain_switch: TemplateChild<libadwaita::SwitchRow>,
        #[template_child]
        pub rain_image_row: TemplateChild<libadwaita::ActionRow>,
        #[template_child]
        pub rain_image_choose_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub rain_image_reset_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub rain_sound_switch: TemplateChild<libadwaita::SwitchRow>,
        #[template_child]
        pub rain_sound_file_row: TemplateChild<libadwaita::ActionRow>,
        #[template_child]
        pub rain_sound_choose_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub rain_sound_reset_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub rain_volume_spin: TemplateChild<libadwaita::SpinRow>,
        #[template_child]
        pub start_sound_row: TemplateChild<libadwaita::ActionRow>,
        #[template_child]
        pub start_sound_play_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub start_sound_choose_button: TemplateChild<libadwaita::SplitButton>,
        #[template_child]
        pub start_sound_reset_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub end_sound_row: TemplateChild<libadwaita::ActionRow>,
        #[template_child]
        pub end_sound_play_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub end_sound_choose_button: TemplateChild<libadwaita::SplitButton>,
        #[template_child]
        pub end_sound_reset_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub goal_sound_row: TemplateChild<libadwaita::ActionRow>,
        #[template_child]
        pub goal_sound_play_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub goal_sound_choose_button: TemplateChild<libadwaita::SplitButton>,
        #[template_child]
        pub goal_sound_reset_button: TemplateChild<gtk::Button>,
        pub settings: OnceCell<gio::Settings>,
        // Plays a sound from the Sounds group to hear what it is like.
        pub preview: CuePlayer,
        pub previewing: Cell<Option<Cue>>,
        pub settings_handlers: RefCell<Vec<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SolanumPreferencesWindow {
        const NAME: &'static str = "SolanumPreferencesWindow";
        type Type = super::SolanumPreferencesWindow;
        type ParentType = libadwaita::PreferencesWindow;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for SolanumPreferencesWindow {
        fn dispose(&self) {
            self.preview.stop();
            if let Some(settings) = self.settings.get() {
                for handler in self.settings_handlers.take() {
                    settings.disconnect(handler);
                }
            }
        }
    }
    impl WidgetImpl for SolanumPreferencesWindow {}
    impl WindowImpl for SolanumPreferencesWindow {}
    impl AdwWindowImpl for SolanumPreferencesWindow {}
    impl PreferencesWindowImpl for SolanumPreferencesWindow {}
}

glib::wrapper! {
    pub struct SolanumPreferencesWindow(ObjectSubclass<imp::SolanumPreferencesWindow>)
        @extends gtk::Widget, gtk::Window, libadwaita::Window, libadwaita::PreferencesWindow,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Native,
        gtk::Root, gtk::ShortcutManager;
}

impl SolanumPreferencesWindow {
    pub fn new<W: IsA<gtk::Window>>(parent: &W, settings: &gio::Settings) -> Self {
        let obj = glib::Object::builder::<Self>()
            .property("transient-for", Some(parent))
            .build();

        let imp = obj.imp();

        settings.bind("lap-length", &*imp.lap_spin, "value").build();
        settings
            .bind("short-break-length", &*imp.short_break_spin, "value")
            .build();
        settings
            .bind("long-break-length", &*imp.long_break_spin, "value")
            .build();
        settings
            .bind(
                "sessions-until-long-break",
                &*imp.session_count_spin,
                "value",
            )
            .build();
        settings
            .bind("daily-goal", &*imp.daily_goal_spin, "value")
            .build();
        settings
            .bind("fullscreen-break", &*imp.fullscreen_switch, "active")
            .build();
        settings
            .bind("rain-background", &*imp.rain_switch, "active")
            .build();

        // Show which image is in use: its file name, or "Built-in".
        settings
            .bind("rain-image", &*imp.rain_image_row, "subtitle")
            .get_only()
            .mapping(|variant, _| {
                let path = variant.str().unwrap_or_default();
                let subtitle = Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| i18n("Built-in"));
                Some(subtitle.to_value())
            })
            .build();

        settings
            .bind("rain-sound", &*imp.rain_sound_switch, "active")
            .build();
        settings
            .bind("rain-volume", &*imp.rain_volume_spin, "value")
            .build();

        // Show which sound file is in use: its file name, or "None".
        settings
            .bind("rain-sound-file", &*imp.rain_sound_file_row, "subtitle")
            .get_only()
            .mapping(|variant, _| {
                let path = variant.str().unwrap_or_default();
                let subtitle = Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| i18n("None"));
                Some(subtitle.to_value())
            })
            .build();

        let _ = imp.settings.set(settings.clone());

        imp.rain_image_choose_button.connect_clicked(glib::clone!(
            #[weak]
            obj,
            move |_| {
                obj.choose_rain_image();
            }
        ));

        imp.rain_image_reset_button.connect_clicked(glib::clone!(
            #[weak]
            obj,
            move |_| {
                if let Some(settings) = obj.imp().settings.get() {
                    let _ = settings.set_string("rain-image", "");
                }
            }
        ));

        imp.rain_sound_choose_button.connect_clicked(glib::clone!(
            #[weak]
            obj,
            move |_| {
                obj.choose_rain_sound();
            }
        ));

        imp.rain_sound_reset_button.connect_clicked(glib::clone!(
            #[weak]
            obj,
            move |_| {
                if let Some(settings) = obj.imp().settings.get() {
                    let _ = settings.set_string("rain-sound-file", "");
                }
            }
        ));

        obj.setup_sounds(settings);

        obj
    }

    fn sound_widgets(
        &self,
        cue: Cue,
    ) -> (
        libadwaita::ActionRow,
        gtk::Button,
        libadwaita::SplitButton,
        gtk::Button,
    ) {
        let imp = self.imp();
        match cue {
            Cue::Start => (
                imp.start_sound_row.get(),
                imp.start_sound_play_button.get(),
                imp.start_sound_choose_button.get(),
                imp.start_sound_reset_button.get(),
            ),
            Cue::End => (
                imp.end_sound_row.get(),
                imp.end_sound_play_button.get(),
                imp.end_sound_choose_button.get(),
                imp.end_sound_reset_button.get(),
            ),
            Cue::Goal => (
                imp.goal_sound_row.get(),
                imp.goal_sound_play_button.get(),
                imp.goal_sound_choose_button.get(),
                imp.goal_sound_reset_button.get(),
            ),
        }
    }

    // The Sounds group: one row per cue with the file in use, a button to
    // hear it, one to choose a file (with "No Sound" in its menu) and one to
    // go back to the default.
    fn setup_sounds(&self, settings: &gio::Settings) {
        let imp = self.imp();

        for cue in CUES {
            let (_, play_button, choose_button, reset_button) = self.sound_widgets(cue);

            play_button.connect_clicked(glib::clone!(
                #[weak(rename_to = obj)]
                self,
                move |_| {
                    obj.toggle_preview(cue);
                }
            ));

            choose_button.connect_clicked(glib::clone!(
                #[weak(rename_to = obj)]
                self,
                move |_| {
                    obj.choose_sound(cue);
                }
            ));

            reset_button.connect_clicked(glib::clone!(
                #[weak]
                settings,
                move |_| {
                    let _ = settings.set_string(cue.key(), "");
                }
            ));

            let handler = settings.connect_changed(
                Some(cue.key()),
                glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    move |_, _| {
                        // Whatever was playing is not what the row says now.
                        if obj.imp().previewing.get().is_some() {
                            obj.imp().preview.stop();
                            obj.imp().previewing.set(None);
                        }
                        obj.update_sound_rows();
                    }
                ),
            );
            imp.settings_handlers.borrow_mut().push(handler);
        }

        // "No Sound" in the menus of the choose buttons.
        let actions = gio::SimpleActionGroup::new();
        for (name, cue) in [
            ("set-start", Cue::Start),
            ("set-end", Cue::End),
            ("set-goal", Cue::Goal),
        ] {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            action.connect_activate(glib::clone!(
                #[weak]
                settings,
                move |_, value| {
                    if let Some(value) = value.and_then(|v| v.str()) {
                        let _ = settings.set_string(cue.key(), value);
                    }
                }
            ));
            actions.add_action(&action);
        }
        self.insert_action_group("sounds", Some(&actions));

        imp.preview.connect_stopped(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            move || {
                obj.imp().previewing.set(None);
                obj.update_sound_rows();
            }
        ));

        self.update_sound_rows();
    }

    fn update_sound_rows(&self) {
        let imp = self.imp();
        let Some(settings) = imp.settings.get() else {
            return;
        };

        for cue in CUES {
            let (row, play_button, _, reset_button) = self.sound_widgets(cue);
            let value = settings.string(cue.key());

            let subtitle = match value.as_str() {
                v if v == sounds::NO_SOUND => i18n("None"),
                "" => match cue {
                    Cue::Start => i18n("None"),
                    Cue::End => i18n("Built-in Chime"),
                    Cue::Goal => i18n("Same as Lap End"),
                },
                path => {
                    let name = Path::new(path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.to_owned());
                    if Path::new(path).is_file() {
                        name
                    } else {
                        // Translators: {} is the name of a sound file that
                        // is gone; the built-in chime plays instead.
                        i18n_f("{} (not found)", &[&name])
                    }
                }
            };
            row.set_subtitle(&subtitle);

            let playing = imp.previewing.get() == Some(cue);
            play_button.set_sensitive(sounds::source(settings, cue) != sounds::Source::Silent);
            play_button.set_icon_name(if playing {
                "media-playback-stop-symbolic"
            } else {
                "media-playback-start-symbolic"
            });
            play_button.set_tooltip_text(Some(&if playing { i18n("Stop") } else { i18n("Play") }));

            reset_button.set_sensitive(!value.is_empty());
        }
    }

    // Play the sound of a row, or stop it when it is the one playing.
    fn toggle_preview(&self, cue: Cue) {
        let imp = self.imp();
        let Some(settings) = imp.settings.get() else {
            return;
        };

        if imp.previewing.get() == Some(cue) {
            imp.preview.stop();
            imp.previewing.set(None);
        } else if let Some(uri) = sounds::uri(settings, cue) {
            imp.preview.play_uri(&uri);
            imp.previewing.set(Some(cue));
        }

        self.update_sound_rows();
    }

    fn choose_sound(&self, cue: Cue) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&i18n("Audio")));
        filter.add_mime_type("audio/*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);

        let dialog = gtk::FileDialog::builder()
            .title(i18n("Sound File"))
            .modal(true)
            .filters(&filters)
            .default_filter(&filter)
            .build();

        // Start in the folder of the current file, or else in
        // $XDG_DATA_HOME/solanum/chimes, whichever exists first.
        let current = self
            .imp()
            .settings
            .get()
            .map(|settings| settings.string(cue.key()).to_string())
            .unwrap_or_default();
        let folders = [
            Path::new(&current).parent().map(Path::to_path_buf),
            Some(sounds::chimes_dir()),
        ];
        if let Some(folder) = folders
            .into_iter()
            .flatten()
            .find(|dir| !dir.as_os_str().is_empty() && dir.is_dir())
        {
            dialog.set_initial_folder(Some(&gio::File::for_path(&folder)));
        }

        dialog.open(
            Some(self),
            None::<&gio::Cancellable>,
            glib::clone!(
                #[weak(rename_to = obj)]
                self,
                move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        return;
                    };
                    if let Some(settings) = obj.imp().settings.get() {
                        let _ = settings.set_string(cue.key(), &path.to_string_lossy());
                    }
                }
            ),
        );
    }

    fn choose_rain_image(&self) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&i18n("Images")));
        for mime in ["image/jpeg", "image/png", "image/webp"] {
            filter.add_mime_type(mime);
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);

        let dialog = gtk::FileDialog::builder()
            .title(i18n("Background Image"))
            .modal(true)
            .filters(&filters)
            .default_filter(&filter)
            .build();

        // Start in the folder of the current image, if there is one.
        if let Some(settings) = self.imp().settings.get() {
            let current = settings.string("rain-image");
            if let Some(dir) = Path::new(current.as_str()).parent() {
                if dir.is_dir() {
                    dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
                }
            }
        }

        dialog.open(
            Some(self),
            None::<&gio::Cancellable>,
            glib::clone!(
                #[weak(rename_to = obj)]
                self,
                move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        return;
                    };
                    if let Some(settings) = obj.imp().settings.get() {
                        let _ = settings.set_string("rain-image", &path.to_string_lossy());
                    }
                }
            ),
        );
    }

    fn choose_rain_sound(&self) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&i18n("Audio")));
        filter.add_mime_type("audio/*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);

        let dialog = gtk::FileDialog::builder()
            .title(i18n("Sound File"))
            .modal(true)
            .filters(&filters)
            .default_filter(&filter)
            .build();

        // Start in the folder of the current file, or else in
        // $XDG_DATA_HOME/solanum/sounds, whichever exists first.
        let current = self
            .imp()
            .settings
            .get()
            .map(|settings| settings.string("rain-sound-file").to_string())
            .unwrap_or_default();
        let folders = [
            Path::new(&current).parent().map(Path::to_path_buf),
            Some(glib::user_data_dir().join("solanum").join("sounds")),
        ];
        if let Some(folder) = folders.into_iter().flatten().find(|dir| dir.is_dir()) {
            dialog.set_initial_folder(Some(&gio::File::for_path(&folder)));
        }

        dialog.open(
            Some(self),
            None::<&gio::Cancellable>,
            glib::clone!(
                #[weak(rename_to = obj)]
                self,
                move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        return;
                    };
                    if let Some(settings) = obj.imp().settings.get() {
                        let _ = settings.set_string("rain-sound-file", &path.to_string_lossy());
                    }
                }
            ),
        );
    }
}
