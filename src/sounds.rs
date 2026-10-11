// sounds.rs
//
// Copyright 2026 Elias Jernberg
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

// The short sounds played when the timer starts, when a lap ends and when a
// work lap reaches the daily goal, and the player for them.

use gstreamer as gst;
use gtk::prelude::*;
use gtk::{gio, glib};

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub static CHIME_URI: &str = "resource:///org/gnome/Solanum/chime.ogg";

// The value of a sound setting that means "no sound".
pub static NO_SOUND: &str = "none";

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Cue {
    // The timer starts.
    Start,
    // A lap ends.
    End,
    // A work lap ends and reaches the daily goal.
    Goal,
}

impl Cue {
    pub fn key(self) -> &'static str {
        match self {
            Cue::Start => "start-sound-file",
            Cue::End => "end-sound-file",
            Cue::Goal => "goal-sound-file",
        }
    }
}

// What a sound setting stands for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    Silent,
    BuiltIn,
    File(PathBuf),
}

// The sound a cue plays: the start sound is silent unless a file is chosen,
// the end sound is the built-in chime unless a file (or 'none') is chosen,
// and the goal sound is the end sound unless one of its own is chosen.
pub fn source(settings: &gio::Settings, cue: Cue) -> Source {
    let value = settings.string(cue.key());
    match (cue, value.as_str()) {
        (_, v) if v == NO_SOUND => Source::Silent,
        (Cue::Start, "") => Source::Silent,
        (Cue::End, "") => Source::BuiltIn,
        (Cue::Goal, "") => source(settings, Cue::End),
        (_, path) => Source::File(PathBuf::from(path)),
    }
}

// The URI to play for a cue, or None for silence. A file that cannot be read
// is logged and replaced by the built-in chime, so a lap never ends unheard
// because a file was moved.
pub fn uri(settings: &gio::Settings, cue: Cue) -> Option<String> {
    match source(settings, cue) {
        Source::Silent => None,
        Source::BuiltIn => Some(CHIME_URI.to_owned()),
        Source::File(path) => match file_uri(&path) {
            Ok(uri) => Some(uri),
            Err(err) => {
                eprintln!(
                    "Cannot play the {} \"{}\": {}; using the built-in chime",
                    cue.key(),
                    path.display(),
                    err
                );
                Some(CHIME_URI.to_owned())
            }
        },
    }
}

fn file_uri(path: &Path) -> Result<String, String> {
    if !path.is_file() {
        return Err("not a file".to_owned());
    }
    std::fs::File::open(path).map_err(|err| err.to_string())?;
    glib::filename_to_uri(path, None)
        .map(|uri| uri.to_string())
        .map_err(|err| err.to_string())
}

// $XDG_DATA_HOME/solanum/chimes, where the file chooser starts.
pub fn chimes_dir() -> PathBuf {
    glib::user_data_dir().join("solanum").join("chimes")
}

// Plays one sound at a time. A file that turns out not to be playable, say
// a text file with an .ogg name, is replaced by the built-in chime. The
// stream is closed again when the sound has played, so nothing is left
// open on the sound server between laps.
pub struct CuePlayer {
    play: gstreamer_play::Play,
    stopped: Rc<RefCell<Option<Box<dyn Fn()>>>>,
    _bus_watch: gst::bus::BusWatchGuard,
}

impl std::fmt::Debug for CuePlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CuePlayer").finish_non_exhaustive()
    }
}

impl Default for CuePlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl CuePlayer {
    pub fn new() -> Self {
        let play = gstreamer_play::Play::new(None::<gstreamer_play::PlayVideoRenderer>);
        // Audio only, so a file with cover art never opens a video window.
        play.set_video_track_enabled(false);
        play.set_subtitle_track_enabled(false);

        let stopped: Rc<RefCell<Option<Box<dyn Fn()>>>> = Rc::new(RefCell::new(None));

        let weak_play = play.downgrade();
        let stopped_cb = stopped.clone();
        let bus_watch = play
            .message_bus()
            .add_watch_local(move |_, msg| {
                let Some(play) = weak_play.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let notify_stopped = || {
                    if let Some(f) = stopped_cb.borrow().as_ref() {
                        f();
                    }
                };
                match gstreamer_play::PlayMessage::parse(msg) {
                    Ok(gstreamer_play::PlayMessage::Error { error, .. }) => {
                        let current = play.uri().map(|u| u.to_string()).unwrap_or_default();
                        eprintln!("Cannot play the sound \"{}\": {}", current, error);
                        play.stop();
                        if current != CHIME_URI {
                            eprintln!("Playing the built-in chime instead");
                            play.set_uri(Some(CHIME_URI));
                            play.play();
                        } else {
                            notify_stopped();
                        }
                    }
                    Ok(gstreamer_play::PlayMessage::EndOfStream) => {
                        play.stop();
                        notify_stopped();
                    }
                    _ => (),
                }
                glib::ControlFlow::Continue
            })
            .expect("The sound player's bus has a watch already");

        Self {
            play,
            stopped,
            _bus_watch: bus_watch,
        }
    }

    pub fn play_uri(&self, uri: &str) {
        self.play.stop();
        self.play.set_uri(Some(uri));
        self.play.play();
    }

    pub fn stop(&self) {
        self.play.stop();
    }

    // Called when a sound has played to the end or could not be played.
    pub fn connect_stopped<F: Fn() + 'static>(&self, f: F) {
        self.stopped.replace(Some(Box::new(f)));
    }
}

impl Drop for CuePlayer {
    fn drop(&mut self) {
        self.play.stop();
    }
}
