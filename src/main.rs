// main.rs
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

use gettextrs::*;
use gtk::prelude::*;
use gtk::{gio, glib};

mod app;
mod config;
mod i18n;
mod preferences_window;
mod rain;
mod sounds;
mod timer;
mod window;

use crate::app::SolanumApplication;

// Entry point for the application
fn main() -> glib::ExitCode {
    // The rain background is a GtkGLArea. GTK's GL renderer draws its
    // frames straight from the GL texture, while the default Vulkan
    // renderer has to import them as dmabufs, which not every driver does
    // (WebKit's frames, which the rain used to be, were copied through the
    // CPU on NVIDIA at the cost of more than a full core). Set before
    // anything else runs, and only when the user has not chosen a renderer.
    // Dmabufs are a Linux matter; other systems keep GTK's own choice.
    #[cfg(target_os = "linux")]
    if std::env::var_os("GSK_RENDERER").is_none() {
        std::env::set_var("GSK_RENDERER", "gl");
    }

    // The rain's GLArea asks for desktop OpenGL 3.3, and a GLArea context is
    // shared with the display's. GTK 4.24 picks OpenGL ES for the display on
    // NVIDIA (EGL offers both), and a desktop context cannot be shared with
    // an ES one, so the rain fell back to the still image with "Unable to
    // create a GL context". Keeping GTK off the ES API makes the whole
    // window desktop GL. Only when the user has not set GDK_DISABLE.
    #[cfg(target_os = "linux")]
    if std::env::var_os("GDK_DISABLE").is_none() {
        std::env::set_var("GDK_DISABLE", "gles-api");
    }

    // Initiialize gstreamer
    gstreamer::init().expect("Failed to initialize gstreamer");

    // Set up translations
    setlocale(LocaleCategory::LcAll, "");
    bindtextdomain("solanum", config::LOCALEDIR).unwrap();
    textdomain("solanum").unwrap();

    // Register resources so we can integrate things like UI files, CSS, and icons
    let res = gio::Resource::load(config::PKGDATADIR.to_owned() + "/solanum.gresource")
        .expect("Could not load resources");
    gio::resources_register(&res);

    // Set the name shown in desktop environments
    glib::set_application_name("Solanum");

    let app = SolanumApplication::new();

    app.run()
}
