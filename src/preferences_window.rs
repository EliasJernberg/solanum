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
use libadwaita::subclass::prelude::*;

use std::cell::OnceCell;
use std::path::Path;

use crate::i18n::*;

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
        pub settings: OnceCell<gio::Settings>,
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

    impl ObjectImpl for SolanumPreferencesWindow {}
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

        obj
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
}
