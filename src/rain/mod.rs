// rain/mod.rs
//
// The rain background: raindrop-fx (https://github.com/SardineFish/raindrop-fx,
// MIT, see data/rain/LICENSE.raindrop-fx) ported from WebGL to OpenGL in a
// GtkGLArea, so that the same code runs on Linux and macOS. The simulation
// is in simulator.rs, the rendering in renderer.rs and the GLSL, taken over
// from upstream unchanged, in shader/.
//
// The options are upstream's defaults, the ones the fork's rain page used,
// and the view drives the rain the way that page did: at most about 60
// updates a second, each advancing the simulation by the real time since
// the last one, capped at 0.1 s.
//
// For checking the port without a screen grab, SOLANUM_RAIN_DUMP=file.png
// saves one frame of the view SOLANUM_RAIN_DUMP_AFTER seconds (default 3)
// into the rain. SOLANUM_RAIN_SEED=n seeds the random numbers, and
// SOLANUM_RAIN_STEPS=n then runs n updates of 1/60 s at once and stops,
// which gives the same drops as raindrop-fx with the same random sequence.
//
// SPDX-License-Identifier: GPL-3.0-or-later

mod renderer;
mod simulator;

use std::cell::RefCell;
use std::ffi::{c_void, CStr, CString};
use std::path::PathBuf;
use std::rc::Rc;

use glow::HasContext;
use gtk::prelude::*;
use gtk::{gdk, glib};

use renderer::{Image, Renderer};
use simulator::{Random, Simulator, Time};

static RAINDROP_PATH: &str = "/org/gnome/Solanum/rain/raindrop.png";

// The rain page's frame loop: at most about 60 updates a second.
const FRAME_MS: f64 = 1000.0 / 60.0;
const MAX_STEP: f64 = 0.1;

/// The options of raindrop-fx that the simulation and rendering use, at
/// upstream's defaults. Of the choices upstream offers, the fork used the
/// default ones: mist on, the "smoother" raindrop compose and a clamped
/// background, which are the paths ported.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    // Simulator options
    pub spawn_interval: [f64; 2],
    pub spawn_size: [f64; 2],
    pub spawn_limit: f64,
    pub gravity: f64,
    pub slip_rate: f64,
    pub motion_interval: [f64; 2],
    pub collider_size: f64,
    pub trail_drop_density: f64,
    pub trail_distance: [f64; 2],
    pub trail_drop_size: [f64; 2],
    pub trail_spread: f64,
    pub initial_spread: f64,
    pub shrink_rate: f64,
    pub velocity_spread: f64,
    pub evaporate: f64,
    pub x_shifting: [f64; 2],

    // Rendering options
    pub background_blur_steps: usize,
    pub mist_color: [f64; 4],
    pub mist_blur_step: usize,
    pub mist_time: f64,
    pub droplets_per_seconds: f64,
    pub droplet_size: [f64; 2],
    pub smooth_raindrop: [f64; 2],
    pub refract_base: f64,
    pub refract_scale: f64,
    pub raindrop_light_pos: [f64; 4],
    pub raindrop_diffuse_light: [f64; 3],
    pub raindrop_shadow_offset: f64,
    pub raindrop_eraser_size: [f64; 2],
    pub raindrop_specular_light: [f64; 3],
    pub raindrop_specular_shininess: f64,
    pub raindrop_light_bump: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            spawn_interval: [0.1, 0.1],
            spawn_size: [60.0, 100.0],
            spawn_limit: 2000.0,
            gravity: 2400.0,
            slip_rate: 0.0,
            motion_interval: [0.1, 0.4],
            collider_size: 1.0,
            trail_drop_density: 0.2,
            trail_distance: [20.0, 30.0],
            trail_drop_size: [0.3, 0.5],
            trail_spread: 0.6,
            initial_spread: 0.5,
            shrink_rate: 0.01,
            velocity_spread: 0.3,
            evaporate: 10.0,
            x_shifting: [0.0, 0.1],

            background_blur_steps: 3,
            mist_color: [0.01, 0.01, 0.01, 1.0],
            mist_blur_step: 4,
            mist_time: 10.0,
            droplets_per_seconds: 500.0,
            droplet_size: [10.0, 30.0],
            smooth_raindrop: [0.96, 0.99],
            refract_base: 0.4,
            refract_scale: 0.6,
            raindrop_light_pos: [-1.0, 1.0, 2.0, 0.0],
            raindrop_diffuse_light: [0.2, 0.2, 0.2],
            raindrop_shadow_offset: 0.8,
            raindrop_eraser_size: [0.93, 1.0],
            raindrop_specular_light: [0.0, 0.0, 0.0],
            raindrop_specular_shininess: 256.0,
            raindrop_light_bump: 1.0,
        }
    }
}

/// A frame to save once, for checking the rendering.
struct Dump {
    path: PathBuf,
    after: f64,
    done: bool,
}

struct Context {
    gl: glow::Context,
    renderer: Renderer,
}

struct State {
    options: Options,
    raindrop: Image,
    background: Image,
    context: Option<Context>,
    simulator: Option<Simulator>,
    random: Random,
    // The size of the view in logical pixels, which the simulation and
    // the textures use like the canvas did, and of the framebuffer.
    size: (i32, i32),
    framebuffer_size: (i32, i32),
    // Frame clock times in microseconds.
    origin: Option<i64>,
    last: Option<i64>,
    first_update: Option<i64>,
    pending: Option<Time>,
    fixed_steps: Option<u32>,
    last_resize: i64,
    dump: Option<Dump>,
}

impl State {
    fn step(&mut self, time: Time) {
        let (Some(context), Some(simulator)) = (self.context.as_mut(), self.simulator.as_mut())
        else {
            return;
        };
        // RaindropFX.update(): the simulation, then the renderer, which
        // draws the next random number for the droplets first.
        simulator.update(&mut self.random, time);
        let seed = self.random.range([0.0, 133.0]);
        unsafe {
            context
                .renderer
                .update(&context.gl, simulator.raindrops(), time, seed);
        }
    }
}

/// A new rain view over the image in `image` (the bytes of a picture file),
/// or None when it cannot be read. Without OpenGL the view falls back to
/// the picture as a still under the same shade.
pub fn new_view(image: &[u8]) -> Option<gtk::Widget> {
    let texture = match gdk::Texture::from_bytes(&glib::Bytes::from(image)) {
        Ok(texture) => texture,
        Err(err) => {
            glib::g_warning!("solanum", "Could not show the background: {}", err);
            return None;
        }
    };
    let background = download(&texture);
    let raindrop = match load_resource_image(RAINDROP_PATH) {
        Ok(image) => image,
        Err(err) => {
            glib::g_warning!("solanum", "Could not load the raindrop texture: {}", err);
            return Some(still_view(&texture));
        }
    };

    let env_u32 = |name| {
        std::env::var(name)
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
    };
    let seed = env_u32("SOLANUM_RAIN_SEED").unwrap_or_else(glib::random_int);
    let fixed_steps = env_u32("SOLANUM_RAIN_STEPS");
    let dump = std::env::var_os("SOLANUM_RAIN_DUMP").map(|path| Dump {
        path: PathBuf::from(path),
        after: std::env::var("SOLANUM_RAIN_DUMP_AFTER")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(3.0),
        done: false,
    });

    let state = Rc::new(RefCell::new(State {
        options: Options::default(),
        raindrop,
        background,
        context: None,
        simulator: None,
        random: Random::new(seed),
        size: (0, 0),
        framebuffer_size: (0, 0),
        origin: None,
        last: None,
        first_update: None,
        pending: None,
        fixed_steps,
        last_resize: 0,
        dump,
    }));

    let area = gtk::GLArea::builder()
        .auto_render(false)
        .has_depth_buffer(false)
        .has_stencil_buffer(false)
        .hexpand(true)
        .vexpand(true)
        .build();
    // The same API on every system: desktop GL 3.3 core, which macOS
    // offers as 4.1 core and Linux as it is.
    area.set_allowed_apis(gdk::GLAPI::GL);
    area.set_required_version(3, 3);

    let view = gtk::Box::builder().hexpand(true).vexpand(true).build();
    view.append(&area);
    // The view is only decoration: clicks and keys belong to the timer.
    view.set_can_focus(false);
    view.set_can_target(false);

    area.connect_realize(glib::clone!(
        #[strong]
        state,
        #[weak]
        view,
        #[strong]
        texture,
        move |area| {
            if let Err(err) = realize(area, &state) {
                glib::g_warning!("solanum", "The rain needs OpenGL 3.3: {}", err);
                // Not from inside the realize handler.
                glib::idle_add_local_once(glib::clone!(
                    #[weak]
                    view,
                    #[weak]
                    area,
                    #[strong]
                    texture,
                    move || {
                        view.remove(&area);
                        view.append(&still_view(&texture));
                    }
                ));
            }
        }
    ));

    area.connect_unrealize(glib::clone!(
        #[strong]
        state,
        move |area| {
            let mut state = state.borrow_mut();
            if let Some(mut context) = state.context.take() {
                area.make_current();
                unsafe { context.renderer.delete(&context.gl) };
            }
            state.size = (0, 0);
        }
    ));

    area.connect_resize(glib::clone!(
        #[strong]
        state,
        move |area, width, height| {
            let mut state = state.borrow_mut();
            state.framebuffer_size = (width, height);
            let size = (area.width(), area.height());
            if size.0 < 1 || size.1 < 1 || size == state.size {
                return;
            }
            let Some(context) = state.context.as_mut() else {
                return;
            };
            // RaindropFX.resize(): the textures are made anew and the
            // background drawn again; the drops stay where they are.
            if let Err(err) = unsafe { context.renderer.resize(&context.gl, size.0, size.1) } {
                glib::g_warning!("solanum", "Could not size the rain: {}", err);
                return;
            }
            state.size = size;
            state.last_resize = glib::monotonic_time();
            let (width, height) = (f64::from(size.0), f64::from(size.1));
            let options = state.options;
            match state.simulator.as_mut() {
                Some(simulator) => simulator.resize(width, height),
                None => state.simulator = Some(Simulator::new(options, width, height)),
            }
        }
    ));

    area.connect_render(glib::clone!(
        #[strong]
        state,
        move |area, _| {
            render(area, &mut state.borrow_mut());
            glib::Propagation::Stop
        }
    ));

    area.add_tick_callback(glib::clone!(
        #[strong]
        state,
        move |area, clock| {
            tick(area, &mut state.borrow_mut(), clock.frame_time());
            glib::ControlFlow::Continue
        }
    ));

    Some(view.upcast())
}

fn realize(area: &gtk::GLArea, state: &RefCell<State>) -> Result<(), String> {
    area.make_current();
    if let Some(err) = area.error() {
        return Err(err.to_string());
    }
    let es = area.api() == gdk::GLAPI::GLES;
    let gl = unsafe { glow::Context::from_loader_function_cstr(gl_proc_address) };
    let version = gl.version();
    if (version.major, version.minor) < (3, if es { 0 } else { 3 }) {
        return Err(format!("got {}.{}", version.major, version.minor));
    }
    let mut state = state.borrow_mut();
    let renderer =
        unsafe { Renderer::new(&gl, es, state.options, &state.raindrop, &state.background)? };
    state.context = Some(Context { gl, renderer });
    state.size = (0, 0);
    Ok(())
}

fn tick(area: &gtk::GLArea, state: &mut State, now: i64) {
    if state.context.is_none() || state.simulator.is_none() {
        return;
    }
    let origin = *state.origin.get_or_insert(now);

    // The fixed run happens all at once in a render once the size has
    // settled (a resize afterwards would clear the droplets and the mist),
    // and after it only a frame to dump is still asked for.
    if state.fixed_steps.is_some() {
        if (state.first_update.is_none() && fixed_run_due(state)) || dump_due(state, now) {
            area.queue_render();
        }
        return;
    }

    let elapsed = match state.last {
        Some(last) => (now - last) as f64 / 1000.0,
        None => f64::INFINITY,
    };
    if elapsed < FRAME_MS - 3.0 {
        if dump_due(state, now) {
            area.queue_render();
        }
        return;
    }
    state.last = Some(now);
    let dt = (elapsed / 1000.0).min(MAX_STEP);
    let total = (now - origin) as f64 / 1_000_000.0;
    // A step not drawn yet is not lost.
    let dt = match state.pending {
        Some(pending) => (pending.dt + dt).min(MAX_STEP),
        None => dt,
    };
    state.pending = Some(Time { dt, total });
    area.queue_render();
}

fn fixed_run_due(state: &State) -> bool {
    glib::monotonic_time() - state.last_resize >= 1_000_000
}

fn dump_due(state: &State, now: i64) -> bool {
    match (&state.dump, state.first_update) {
        (Some(dump), Some(first)) => !dump.done && (now - first) as f64 / 1e6 >= dump.after,
        _ => false,
    }
}

fn render(area: &gtk::GLArea, state: &mut State) {
    if state.context.is_none() {
        return;
    }
    let now = area
        .frame_clock()
        .map(|clock| clock.frame_time())
        .unwrap_or_else(glib::monotonic_time);

    if let Some(steps) = state.fixed_steps {
        if state.first_update.is_none() && state.simulator.is_some() && fixed_run_due(state) {
            for i in 1..=steps {
                state.step(Time {
                    dt: 1.0 / 60.0,
                    total: f64::from(i) / 60.0,
                });
            }
            state.first_update = Some(now);
            eprintln!(
                "solanum: rain after {} fixed steps: {} drops, {} random numbers",
                steps,
                state.simulator.as_ref().map_or(0, |s| s.len()),
                state.random.calls
            );
        }
    } else if let Some(time) = state.pending.take() {
        state.step(time);
        state.first_update.get_or_insert(now);
    }

    // Back to the area's own framebuffer for the passes that end there.
    area.attach_buffers();
    let (width, height) = state.framebuffer_size;
    let context = state.context.as_ref().unwrap();
    unsafe { context.renderer.draw(&context.gl, width, height) };

    if dump_due(state, now) {
        let dump = state.dump.as_mut().unwrap();
        dump.done = true;
        let context = state.context.as_ref().unwrap();
        match unsafe { save_frame(&context.gl, width, height, &dump.path) } {
            Ok(()) => eprintln!(
                "solanum: saved a frame of the rain to {}",
                dump.path.display()
            ),
            Err(err) => eprintln!(
                "solanum: could not save the rain to {}: {}",
                dump.path.display(),
                err
            ),
        }
    }
}

/// Reads back the framebuffer that is bound and saves it as a PNG.
unsafe fn save_frame(
    gl: &glow::Context,
    width: i32,
    height: i32,
    path: &std::path::Path,
) -> Result<(), String> {
    if width < 1 || height < 1 {
        return Err("nothing drawn yet".to_owned());
    }
    let stride = width as usize * 4;
    let mut pixels = vec![0u8; stride * height as usize];
    gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
    gl.read_pixels(
        0,
        0,
        width,
        height,
        glow::RGBA,
        glow::UNSIGNED_BYTE,
        glow::PixelPackData::Slice(Some(&mut pixels)),
    );
    // GL rows run bottom up; the window shows the rain opaque.
    let mut image = Vec::with_capacity(pixels.len());
    for row in pixels.chunks_exact(stride).rev() {
        image.extend_from_slice(row);
    }
    for alpha in image.iter_mut().skip(3).step_by(4) {
        *alpha = 255;
    }
    let texture = gdk::MemoryTexture::new(
        width,
        height,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from_owned(image),
        stride,
    );
    texture.save_to_png(path).map_err(|err| err.to_string())
}

/// GL entry points through libepoxy, which GTK already uses for its own
/// contexts: each `epoxy_glFoo` is a pointer to the function for the
/// current context.
fn gl_proc_address(name: &CStr) -> *const c_void {
    let Ok(name) = name.to_str() else {
        return std::ptr::null();
    };
    let Ok(symbol) = CString::new(format!("epoxy_{}", name)) else {
        return std::ptr::null();
    };
    unsafe {
        let pointer = libc::dlsym(libc::RTLD_DEFAULT, symbol.as_ptr());
        if pointer.is_null() {
            return std::ptr::null();
        }
        *(pointer as *const *const c_void)
    }
}

/// The pixels of a texture as 8-bit RGBA, top row first.
fn download(texture: &gdk::Texture) -> Image {
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let (width, height) = (texture.width(), texture.height());
    let row = width as usize * 4;
    let mut pixels = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        pixels.extend_from_slice(&bytes[y * stride..y * stride + row]);
    }
    Image {
        width,
        height,
        pixels,
    }
}

/// A PNG from the resources with its alpha as stored, like a WebGL upload
/// without premultiplication.
fn load_resource_image(path: &str) -> Result<Image, String> {
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_resource(path).map_err(|err| err.to_string())?;
    let pixbuf = if pixbuf.has_alpha() {
        pixbuf
    } else {
        pixbuf
            .add_alpha(false, 0, 0, 0)
            .map_err(|err| err.to_string())?
    };
    let (width, height) = (pixbuf.width(), pixbuf.height());
    let stride = pixbuf.rowstride() as usize;
    let bytes = pixbuf.read_pixel_bytes();
    let row = width as usize * 4;
    let mut pixels = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        pixels.extend_from_slice(&bytes[y * stride..y * stride + row]);
    }
    Ok(Image {
        width,
        height,
        pixels,
    })
}

/// Without OpenGL the background is the same image as a still, under the
/// same shade as the rain (.rain-shade in style.css) so the labels read as
/// well.
fn still_view(texture: &gdk::Texture) -> gtk::Widget {
    let picture = gtk::Picture::builder()
        .paintable(texture)
        .content_fit(gtk::ContentFit::Cover)
        .hexpand(true)
        .vexpand(true)
        .build();
    let shade = gtk::Box::builder().css_classes(["rain-shade"]).build();

    let view = gtk::Overlay::builder().child(&picture).build();
    view.add_overlay(&shade);
    view.set_can_focus(false);
    view.set_can_target(false);
    view.upcast()
}
