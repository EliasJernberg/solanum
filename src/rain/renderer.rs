// renderer.rs
//
// The rendering of raindrop-fx (https://github.com/SardineFish/raindrop-fx,
// MIT, see data/rain/LICENSE.raindrop-fx): renderer.ts and blur.ts, with the
// parts of zogra-renderer they use (blit, render textures, the blit-copy
// shader), ported pass by pass to OpenGL 3.3 core. The GLSL files in
// shader/ are the upstream ones; only the version line is swapped when the
// context is desktop GL. Everything is drawn in the same order, with the
// same blend functions, texture formats and filtering, at the size of the
// view in logical pixels like the canvas was. Only the passes that end in
// the window are drawn at the size of its framebuffer, so the rain stays
// sharp on HiDPI screens.
//
// SPDX-License-Identifier: GPL-3.0-or-later

use glow::HasContext;

use super::simulator::{RainDrop, Time};
use super::Options;

const SHADER_2D_VERT: &str = include_str!("shader/2d-vert.glsl");
const SHADER_2D_FRAG: &str = include_str!("shader/2d-frag.glsl");
const SHADER_DEFAULT_VERT: &str = include_str!("shader/default-vert.glsl");
const SHADER_BLIT_COPY: &str = include_str!("shader/blit-copy.glsl");
const SHADER_BLUR: &str = include_str!("shader/blur.glsl");
const SHADER_BG_MIST: &str = include_str!("shader/bg-mist.glsl");
const SHADER_COMPOSE: &str = include_str!("shader/compose.glsl");
const SHADER_DROPLET_VERT: &str = include_str!("shader/droplet-vert.glsl");
const SHADER_DROPLET_FRAG: &str = include_str!("shader/droplet.glsl");
const SHADER_ERASE: &str = include_str!("shader/erase.glsl");
const SHADER_RAINDROP_VERT: &str = include_str!("shader/raindrop-vert.glsl");
const SHADER_RAINDROP_FRAG: &str = include_str!("shader/raindrop-frag.glsl");
const SHADER_SHADE: &str = include_str!("shader/shade.glsl");

// Attribute locations, bound before linking. aModelMatrix takes four.
const ATTRIBUTES: &[(&str, u32)] = &[
    ("aPos", 0),
    ("aColor", 1),
    ("aUV", 2),
    ("aNormal", 3),
    ("aSize", 4),
    ("aModelMatrix", 5),
];

// A vertex of zogra's meshes: position, colour, uv and normal.
const VERTEX_FLOATS: usize = 12;
// A raindrop instance: size, then the model matrix.
const INSTANCE_FLOATS: usize = 17;

const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

/// An 8-bit RGBA image, top row first, as decoded from a file.
pub struct Image {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u8>,
}

impl Image {
    /// The rows in GL order. zogra draws every image it loads upside down
    /// into its texture (the FlipTexture shader), so that the top of the
    /// picture ends up at v = 1.
    fn gl_rows(&self) -> Vec<u8> {
        let stride = self.width as usize * 4;
        let mut rows = Vec::with_capacity(self.pixels.len());
        for row in self.pixels.chunks_exact(stride).rev() {
            rows.extend_from_slice(row);
        }
        rows
    }
}

struct Texture {
    texture: glow::Texture,
    framebuffer: Option<glow::Framebuffer>,
    width: i32,
    height: i32,
}

impl Texture {
    unsafe fn new(
        gl: &glow::Context,
        width: i32,
        height: i32,
        format: TextureFormat,
        wrap: u32,
        pixels: Option<&[u8]>,
        render_target: bool,
    ) -> Result<Self, String> {
        let (internal, pixel_format, ty) = match format {
            TextureFormat::Rgba8 => (glow::RGBA8, glow::RGBA, glow::UNSIGNED_BYTE),
            TextureFormat::R16f => (glow::R16F, glow::RED, glow::HALF_FLOAT),
        };
        let texture = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, wrap as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, wrap as i32);
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            internal as i32,
            width,
            height,
            0,
            pixel_format,
            ty,
            glow::PixelUnpackData::Slice(pixels),
        );
        gl.bind_texture(glow::TEXTURE_2D, None);

        let mut tex = Self {
            texture,
            framebuffer: None,
            width,
            height,
        };
        if render_target {
            let framebuffer = gl.create_framebuffer()?;
            tex.framebuffer = Some(framebuffer);
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            if status != glow::FRAMEBUFFER_COMPLETE {
                tex.delete(gl);
                return Err(format!("incomplete framebuffer (0x{:x})", status));
            }
            // WebGL starts textures out cleared; GL leaves them undefined.
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        Ok(tex)
    }

    unsafe fn delete(&self, gl: &glow::Context) {
        if let Some(framebuffer) = self.framebuffer {
            gl.delete_framebuffer(framebuffer);
        }
        gl.delete_texture(self.texture);
    }
}

#[derive(Clone, Copy)]
enum TextureFormat {
    Rgba8,
    R16f,
}

struct Program {
    program: glow::Program,
}

impl Program {
    unsafe fn new(
        gl: &glow::Context,
        es: bool,
        vertex: &str,
        fragment: &str,
    ) -> Result<Self, String> {
        let program = gl.create_program()?;
        let mut shaders = Vec::new();
        for (kind, source) in [
            (glow::VERTEX_SHADER, vertex),
            (glow::FRAGMENT_SHADER, fragment),
        ] {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, &shader_source(source, es));
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                let log = gl.get_shader_info_log(shader);
                gl.delete_shader(shader);
                for shader in shaders {
                    gl.delete_shader(shader);
                }
                gl.delete_program(program);
                return Err(format!("shader compile error: {}", log));
            }
            gl.attach_shader(program, shader);
            shaders.push(shader);
        }
        for (name, location) in ATTRIBUTES {
            gl.bind_attrib_location(program, *location, name);
        }
        gl.link_program(program);
        let linked = gl.get_program_link_status(program);
        let log = gl.get_program_info_log(program);
        for shader in shaders {
            gl.detach_shader(program, shader);
            gl.delete_shader(shader);
        }
        if !linked {
            gl.delete_program(program);
            return Err(format!("shader link error: {}", log));
        }
        Ok(Self { program })
    }

    unsafe fn use_program(&self, gl: &glow::Context) {
        gl.use_program(Some(self.program));
    }

    unsafe fn location(&self, gl: &glow::Context, name: &str) -> Option<glow::UniformLocation> {
        gl.get_uniform_location(self.program, name)
    }

    unsafe fn set_f32(&self, gl: &glow::Context, name: &str, x: f32) {
        gl.uniform_1_f32(self.location(gl, name).as_ref(), x);
    }

    unsafe fn set_vec2(&self, gl: &glow::Context, name: &str, v: [f32; 2]) {
        gl.uniform_2_f32(self.location(gl, name).as_ref(), v[0], v[1]);
    }

    unsafe fn set_vec4(&self, gl: &glow::Context, name: &str, v: [f32; 4]) {
        gl.uniform_4_f32(self.location(gl, name).as_ref(), v[0], v[1], v[2], v[3]);
    }

    unsafe fn set_mat4(&self, gl: &glow::Context, name: &str, m: &[f32; 16]) {
        gl.uniform_matrix_4_f32_slice(self.location(gl, name).as_ref(), false, m);
    }

    unsafe fn set_texture(&self, gl: &glow::Context, name: &str, unit: u32, texture: &Texture) {
        gl.active_texture(glow::TEXTURE0 + unit);
        gl.bind_texture(glow::TEXTURE_2D, Some(texture.texture));
        gl.uniform_1_i32(self.location(gl, name).as_ref(), unit as i32);
    }

    /// The transforms zogra sets for every draw. A blit draws with an
    /// identity view-projection and model.
    unsafe fn set_transforms(&self, gl: &glow::Context, view_projection: &[f32; 16]) {
        self.set_mat4(gl, "uTransformM", &IDENTITY);
        self.set_mat4(gl, "uTransformVP", view_projection);
        self.set_mat4(gl, "uTransformMVP", view_projection);
        self.set_mat4(gl, "uTransformM_IT", &IDENTITY);
    }

    unsafe fn delete(&self, gl: &glow::Context) {
        gl.delete_program(self.program);
    }
}

/// The upstream GLSL is ES 3.00. Desktop GL 3.3 core takes the same code
/// under its own version line (precision qualifiers are allowed and have no
/// effect). Comments may hold characters outside ASCII, which not every
/// driver accepts.
fn shader_source(source: &str, es: bool) -> String {
    let source: String = source
        .chars()
        .map(|c| if c.is_ascii() { c } else { ' ' })
        .collect();
    if es {
        source
    } else {
        source.replacen("#version 300 es", "#version 330 core", 1)
    }
}

/// A mesh in a vertex array, drawn with 32-bit indices.
struct Mesh {
    vertex_array: glow::VertexArray,
    vertex_buffer: glow::Buffer,
    index_buffer: glow::Buffer,
    count: i32,
}

impl Mesh {
    unsafe fn new(gl: &glow::Context, vertices: &[f32], indices: &[u32]) -> Result<Self, String> {
        let vertex_array = gl.create_vertex_array()?;
        gl.bind_vertex_array(Some(vertex_array));

        let vertex_buffer = gl.create_buffer()?;
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vertex_buffer));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, as_bytes(vertices), glow::STATIC_DRAW);
        let stride = (VERTEX_FLOATS * 4) as i32;
        for (location, size, offset) in [(0, 3, 0), (1, 4, 3), (2, 2, 7), (3, 3, 9)] {
            gl.enable_vertex_attrib_array(location);
            gl.vertex_attrib_pointer_f32(location, size, glow::FLOAT, false, stride, offset * 4);
        }

        let index_buffer = gl.create_buffer()?;
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(index_buffer));
        gl.buffer_data_u8_slice(
            glow::ELEMENT_ARRAY_BUFFER,
            as_bytes(indices),
            glow::STATIC_DRAW,
        );

        gl.bind_vertex_array(None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);

        Ok(Self {
            vertex_array,
            vertex_buffer,
            index_buffer,
            count: indices.len() as i32,
        })
    }

    /// `MeshBuilder.quad()`: a square around the centre with uvs 0..1.
    fn quad_vertices(center: [f32; 2], size: [f32; 2], uvs: [[f32; 2]; 4]) -> Vec<f32> {
        let half = [size[0] * 0.5, size[1] * 0.5];
        let corners = [
            [center[0] - half[0], center[1] - half[1]],
            [center[0] + half[0], center[1] - half[1]],
            [center[0] + half[0], center[1] + half[1]],
            [center[0] - half[0], center[1] + half[1]],
        ];
        let mut vertices = Vec::with_capacity(4 * VERTEX_FLOATS);
        for (corner, uv) in corners.iter().zip(uvs) {
            vertices.extend_from_slice(&[
                corner[0], corner[1], 0.0, 1.0, 1.0, 1.0, 1.0, uv[0], uv[1], 0.0, 0.0, 1.0,
            ]);
        }
        vertices
    }

    unsafe fn draw(&self, gl: &glow::Context) {
        gl.bind_vertex_array(Some(self.vertex_array));
        gl.draw_elements(glow::TRIANGLES, self.count, glow::UNSIGNED_INT, 0);
    }

    unsafe fn draw_instanced(&self, gl: &glow::Context, instances: i32) {
        gl.bind_vertex_array(Some(self.vertex_array));
        gl.draw_elements_instanced(
            glow::TRIANGLES,
            self.count,
            glow::UNSIGNED_INT,
            0,
            instances,
        );
    }

    unsafe fn delete(&self, gl: &glow::Context) {
        gl.delete_vertex_array(self.vertex_array);
        gl.delete_buffer(self.vertex_buffer);
        gl.delete_buffer(self.index_buffer);
    }
}

const UNIT_UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

fn as_bytes<T: Copy>(data: &[T]) -> &[u8] {
    // SAFETY: plain f32 and u32 data, read as bytes for upload.
    unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), std::mem::size_of_val(data)) }
}

/// `mat4.ortho(0, width, 0, height, 1, -1)` from gl-matrix.
fn ortho(width: f64, height: f64) -> [f32; 16] {
    let (left, right, bottom, top, near, far) = (0.0, width, 0.0, height, 1.0, -1.0);
    let lr = 1.0 / (left - right);
    let bt = 1.0 / (bottom - top);
    let nf = 1.0 / (near - far);
    let mut out = [0.0f32; 16];
    out[0] = (-2.0 * lr) as f32;
    out[5] = (-2.0 * bt) as f32;
    out[10] = (2.0 * nf) as f32;
    out[12] = ((left + right) * lr) as f32;
    out[13] = ((top + bottom) * bt) as f32;
    out[14] = ((far + near) * nf) as f32;
    out[15] = 1.0;
    out
}

/// The textures that follow the size of the view.
struct Targets {
    background: Texture,
    raindrop_compose: Texture,
    droplet: Texture,
    blurry_background: Texture,
    mist_background: Texture,
    mist: Texture,
    // The blur steps, index 0 unused as upstream (the mist path never uses
    // its first render texture).
    steps: Vec<Texture>,
}

impl Targets {
    unsafe fn delete(&self, gl: &glow::Context) {
        for texture in [
            &self.background,
            &self.raindrop_compose,
            &self.droplet,
            &self.blurry_background,
            &self.mist_background,
            &self.mist,
        ] {
            texture.delete(gl);
        }
        for step in &self.steps {
            step.delete(gl);
        }
    }
}

pub struct Renderer {
    options: Options,
    es: bool,
    width: f64,
    height: f64,

    raindrop_texture: Texture,
    white: Texture,
    original_background: Texture,
    targets: Option<Targets>,

    blit_copy: Program,
    blur: Program,
    mist_accumulate: Program,
    mist_compose: Program,
    compose: Program,
    erase: Program,
    raindrop: Program,
    droplet: Program,
    shade: Program,

    triangle: Mesh,
    quad: Mesh,
    droplet_quad: Mesh,
    instance_buffer: glow::Buffer,
    instance_capacity: usize,
    instance_data: Vec<f32>,
}

impl Renderer {
    pub unsafe fn new(
        gl: &glow::Context,
        es: bool,
        options: Options,
        raindrop: &Image,
        background: &Image,
    ) -> Result<Self, String> {
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
        gl.disable(glow::SCISSOR_TEST);

        // Built here and deleted on failure further down by the caller
        // dropping the GL context with the area.
        let raindrop_texture = Texture::new(
            gl,
            raindrop.width,
            raindrop.height,
            TextureFormat::Rgba8,
            glow::REPEAT,
            Some(&raindrop.gl_rows()),
            false,
        )?;
        let white = Texture::new(
            gl,
            1,
            1,
            TextureFormat::Rgba8,
            glow::REPEAT,
            Some(&[255, 255, 255, 255]),
            false,
        )?;
        let original_background = Texture::new(
            gl,
            background.width,
            background.height,
            TextureFormat::Rgba8,
            glow::CLAMP_TO_EDGE,
            Some(&background.gl_rows()),
            false,
        )?;

        let program = |vertex, fragment| Program::new(gl, es, vertex, fragment);
        let blit_copy = program(SHADER_DEFAULT_VERT, SHADER_BLIT_COPY)?;
        let blur = program(SHADER_2D_VERT, SHADER_BLUR)?;
        let mist_accumulate = program(SHADER_2D_VERT, SHADER_2D_FRAG)?;
        let mist_compose = program(SHADER_2D_VERT, SHADER_BG_MIST)?;
        let compose = program(SHADER_2D_VERT, SHADER_COMPOSE)?;
        let erase = program(SHADER_2D_VERT, SHADER_ERASE)?;
        let raindrop = program(SHADER_RAINDROP_VERT, SHADER_RAINDROP_FRAG)?;
        let droplet = program(SHADER_DROPLET_VERT, SHADER_DROPLET_FRAG)?;
        let shade = program(SHADER_2D_VERT, SHADER_SHADE)?;

        // MeshBuilder.ndcTriangle(), the mesh of every blit.
        #[rustfmt::skip]
        let triangle = Mesh::new(
            gl,
            &[
                -1.0, -1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
                3.0, -1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 2.0, 0.0, 0.0, 0.0, 1.0,
                -1.0, 3.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 2.0, 0.0, 0.0, 1.0,
            ],
            &[0, 1, 2],
        )?;
        let quad_vertices = Mesh::quad_vertices([0.0, 0.0], [1.0, 1.0], UNIT_UVS);
        let quad_indices = [0, 1, 2, 0, 2, 3];
        let quad = Mesh::new(gl, &quad_vertices, &quad_indices)?;
        let droplet_quad = Mesh::new(gl, &quad_vertices, &quad_indices)?;

        // The raindrop instances: aSize and aModelMatrix, one per instance.
        let instance_buffer = gl.create_buffer()?;
        gl.bind_vertex_array(Some(quad.vertex_array));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(instance_buffer));
        let stride = (INSTANCE_FLOATS * 4) as i32;
        gl.enable_vertex_attrib_array(4);
        gl.vertex_attrib_pointer_f32(4, 1, glow::FLOAT, false, stride, 0);
        gl.vertex_attrib_divisor(4, 1);
        for column in 0..4 {
            let location = 5 + column;
            gl.enable_vertex_attrib_array(location);
            gl.vertex_attrib_pointer_f32(
                location,
                4,
                glow::FLOAT,
                false,
                stride,
                (4 + column as i32 * 16) as i32,
            );
            gl.vertex_attrib_divisor(location, 1);
        }
        gl.bind_vertex_array(None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);

        Ok(Self {
            options,
            es,
            width: 0.0,
            height: 0.0,
            raindrop_texture,
            white,
            original_background,
            targets: None,
            blit_copy,
            blur,
            mist_accumulate,
            mist_compose,
            compose,
            erase,
            raindrop,
            droplet,
            shade,
            triangle,
            quad,
            droplet_quad,
            instance_buffer,
            instance_capacity: 0,
            instance_data: Vec::new(),
        })
    }

    /// `resize()`: new, cleared textures at the new size, and the
    /// background drawn and blurred again.
    pub unsafe fn resize(
        &mut self,
        gl: &glow::Context,
        width: i32,
        height: i32,
    ) -> Result<(), String> {
        if let Some(targets) = self.targets.take() {
            targets.delete(gl);
        }
        self.width = f64::from(width);
        self.height = f64::from(height);

        let rgba = |wrap| Texture::new(gl, width, height, TextureFormat::Rgba8, wrap, None, true);
        // Upstream the background textures are set to backgroundWrapMode
        // (clamp) and the others keep zogra's repeat; at the size of the
        // view they are only read at texel centres, where the two are the
        // same, and clamp keeps the edges right when the window has more
        // pixels than the view.
        let targets = Targets {
            background: rgba(glow::CLAMP_TO_EDGE)?,
            raindrop_compose: rgba(glow::CLAMP_TO_EDGE)?,
            droplet: rgba(glow::CLAMP_TO_EDGE)?,
            blurry_background: rgba(glow::CLAMP_TO_EDGE)?,
            mist_background: rgba(glow::CLAMP_TO_EDGE)?,
            mist: Texture::new(
                gl,
                width,
                height,
                TextureFormat::R16f,
                glow::CLAMP_TO_EDGE,
                None,
                true,
            )?,
            steps: Vec::new(),
        };
        self.targets = Some(targets);

        self.draw_background(gl)?;
        Ok(())
    }

    /// The image, cropped to cover the view, into `background`, and the
    /// blurred versions of it.
    unsafe fn draw_background(&mut self, gl: &glow::Context) -> Result<(), String> {
        let source = &self.original_background;
        let (sw, sh) = (f64::from(source.width), f64::from(source.height));
        let (dw, dh) = (self.width, self.height);

        // Utils.imageResize(..., ImageSizing.Cover)
        let (mut min, mut max) = ([0.0, 0.0], [sw, sh]);
        if sw / sh > dw / dh {
            let delta = sw - dw * (sh / dh);
            min[0] += delta / 2.0;
            max[0] -= delta / 2.0;
        } else {
            let delta = sh - dh * (sw / dw);
            min[1] += delta / 2.0;
            max[1] -= delta / 2.0;
        }
        let uv_min = [(min[0] / sw) as f32, (min[1] / sh) as f32];
        let uv_max = [(max[0] / sw) as f32, (max[1] / sh) as f32];
        let clip_quad = Mesh::new(
            gl,
            &Mesh::quad_vertices(
                [0.0, 0.0],
                [2.0, 2.0],
                [
                    [uv_min[0], uv_min[1]],
                    [uv_max[0], uv_min[1]],
                    [uv_max[0], uv_max[1]],
                    [uv_min[0], uv_max[1]],
                ],
            ),
            &[0, 1, 2, 0, 2, 3],
        )?;

        let targets = self.targets.as_ref().unwrap();
        gl.disable(glow::BLEND);
        self.blit_copy.use_program(gl);
        self.blit_copy.set_transforms(gl, &IDENTITY);
        self.blit_copy
            .set_texture(gl, "uMainTex", 0, &self.original_background);
        bind_target(gl, &targets.background);
        clip_quad.draw(gl);
        clip_quad.delete(gl);

        self.blur_background(gl)
    }

    /// `blurBackground()` with mist on: downsample to the larger of the two
    /// step counts, then upsample the background and the mist from it.
    unsafe fn blur_background(&mut self, gl: &glow::Context) -> Result<(), String> {
        let background_steps = self.options.background_blur_steps;
        let mist_steps = self.options.mist_blur_step;
        let steps = background_steps.max(mist_steps);

        // downSample()
        let mut created = Vec::new();
        {
            let targets = self.targets.as_ref().unwrap();
            let (mut width, mut height) = (targets.background.width, targets.background.height);
            for _ in 1..=steps {
                width = (width / 2).max(1);
                height = (height / 2).max(1);
                created.push(Texture::new(
                    gl,
                    width,
                    height,
                    TextureFormat::Rgba8,
                    glow::CLAMP_TO_EDGE,
                    None,
                    true,
                )?);
            }
        }
        let targets = self.targets.as_mut().unwrap();
        targets.steps = created;
        let targets = self.targets.as_ref().unwrap();
        let step = |i: usize| &targets.steps[i - 1];

        let mut input = &targets.background;
        for i in 1..=steps {
            self.blur_pass(gl, input, step(i));
            input = step(i);
        }

        // upSample(): the larger output first, so the smaller does not
        // overwrite steps it still needs.
        let up_sample = |iteration: usize, output: &Texture| {
            let mut input = step(iteration);
            for i in (0..iteration).rev() {
                let target = if i == 0 { output } else { step(i) };
                self.blur_pass(gl, input, target);
                input = target;
            }
        };
        if background_steps == mist_steps {
            up_sample(background_steps, &targets.blurry_background);
            gl.disable(glow::BLEND);
            self.blit_copy.use_program(gl);
            self.blit_copy.set_transforms(gl, &IDENTITY);
            self.blit_copy
                .set_texture(gl, "uMainTex", 0, &targets.blurry_background);
            bind_target(gl, &targets.mist_background);
            self.triangle.draw(gl);
        } else if mist_steps > background_steps {
            up_sample(background_steps, &targets.blurry_background);
            up_sample(mist_steps, &targets.mist_background);
        } else {
            up_sample(mist_steps, &targets.mist_background);
            up_sample(background_steps, &targets.blurry_background);
        }
        Ok(())
    }

    /// One step of `BlurRenderer`: four bilinear taps one texel away.
    unsafe fn blur_pass(&self, gl: &glow::Context, input: &Texture, output: &Texture) {
        gl.disable(glow::BLEND);
        self.blur.use_program(gl);
        self.blur.set_transforms(gl, &IDENTITY);
        self.blur.set_texture(gl, "uMainTex", 0, input);
        let (w, h) = (f64::from(input.width), f64::from(input.height));
        self.blur.set_vec4(
            gl,
            "uTexSize",
            [w as f32, h as f32, (1.0 / w) as f32, (1.0 / h) as f32],
        );
        self.blur.set_f32(gl, "uSampleOffset", 1.0);
        bind_target(gl, output);
        self.triangle.draw(gl);
    }

    /// The first half of `render()`: the droplets, the mist and the
    /// raindrops into their textures.
    pub unsafe fn update<'a>(
        &mut self,
        gl: &glow::Context,
        raindrops: impl ExactSizeIterator<Item = &'a RainDrop>,
        time: Time,
        seed: f64,
    ) {
        let Some(targets) = self.targets.as_ref() else {
            return;
        };
        let o = &self.options;
        let projection = ortho(self.width, self.height);
        let (width, height) = (self.width as f32, self.height as f32);

        gl.disable(glow::DEPTH_TEST);
        gl.disable(glow::CULL_FACE);
        gl.disable(glow::SCISSOR_TEST);
        gl.disable(glow::STENCIL_TEST);
        if !self.es {
            gl.disable(glow::FRAMEBUFFER_SRGB);
        }
        gl.color_mask(true, true, true, true);

        // drawDroplet(): tiny droplets sprinkled at random each update.
        // The count is a double in WebGL and truncated to an integer.
        let count = (o.droplets_per_seconds * time.dt) as i32;
        bind_target(gl, &targets.droplet);
        gl.enable(glow::BLEND);
        gl.blend_func_separate(
            glow::ONE_MINUS_DST_COLOR,
            glow::ONE_MINUS_SRC_COLOR,
            glow::ONE,
            glow::ONE_MINUS_SRC_ALPHA,
        );
        self.droplet.use_program(gl);
        self.droplet.set_transforms(gl, &projection);
        self.droplet
            .set_texture(gl, "uMainTex", 0, &self.raindrop_texture);
        self.droplet
            .set_vec4(gl, "uSpawnRect", [0.0, 0.0, width, height]);
        self.droplet.set_vec2(
            gl,
            "uSizeRange",
            [o.droplet_size[0] as f32, o.droplet_size[1] as f32],
        );
        self.droplet.set_f32(gl, "uSeed", seed as f32);
        if count > 0 {
            self.droplet_quad.draw_instanced(gl, count);
        }

        // drawMist(): the mist grows by dt / mistTime in every update.
        bind_target(gl, &targets.mist);
        gl.blend_func_separate(glow::ONE, glow::ONE, glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
        self.mist_accumulate.use_program(gl);
        self.mist_accumulate.set_transforms(gl, &IDENTITY);
        self.mist_accumulate
            .set_texture(gl, "uMainTex", 0, &self.white);
        self.mist_accumulate.set_vec4(
            gl,
            "uColor",
            [(time.dt / o.mist_time) as f32, 1.0, 1.0, 1.0],
        );
        self.triangle.draw(gl);

        // drawRaindrops(): every drop as an instance of the raindrop
        // texture, scaled to its size, ...
        let drops = raindrops.len();
        self.instance_data.clear();
        for drop in raindrops {
            let (sx, sy) = (drop.size[0] as f32, drop.size[1] as f32);
            let (px, py) = (drop.pos[0] as f32, drop.pos[1] as f32);
            self.instance_data.extend_from_slice(&[
                (drop.size[0] / 100.0) as f32,
                sx,
                0.0,
                0.0,
                0.0,
                0.0,
                sy,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
                px,
                py,
                0.0,
                1.0,
            ]);
        }
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.instance_buffer));
        if drops > self.instance_capacity {
            self.instance_capacity = drops.max(3000).next_power_of_two();
            gl.buffer_data_size(
                glow::ARRAY_BUFFER,
                (self.instance_capacity * INSTANCE_FLOATS * 4) as i32,
                glow::DYNAMIC_DRAW,
            );
        }
        gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, as_bytes(&self.instance_data));
        gl.bind_buffer(glow::ARRAY_BUFFER, None);

        bind_target(gl, &targets.raindrop_compose);
        gl.clear_color(0.0, 0.0, 0.0, 0.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        // raindropCompose "smoother": the normals blend like 'exclusion'.
        gl.blend_func_separate(
            glow::ONE_MINUS_DST_COLOR,
            glow::ONE_MINUS_SRC_COLOR,
            glow::ONE,
            glow::ONE_MINUS_SRC_ALPHA,
        );
        self.raindrop.use_program(gl);
        self.raindrop.set_transforms(gl, &projection);
        self.raindrop
            .set_texture(gl, "uMainTex", 0, &self.raindrop_texture);
        if drops > 0 {
            self.quad.draw_instanced(gl, drops as i32);
        }

        // ... which then wipe the droplets and the mist where they are.
        gl.blend_func_separate(
            glow::ZERO,
            glow::ONE_MINUS_SRC_ALPHA,
            glow::ZERO,
            glow::ONE_MINUS_SRC_ALPHA,
        );
        self.erase.use_program(gl);
        self.erase.set_transforms(gl, &IDENTITY);
        self.erase
            .set_texture(gl, "uMainTex", 0, &targets.raindrop_compose);
        self.erase.set_vec2(
            gl,
            "uEraserSmooth",
            [
                o.raindrop_eraser_size[0] as f32,
                o.raindrop_eraser_size[1] as f32,
            ],
        );
        bind_target(gl, &targets.droplet);
        self.triangle.draw(gl);
        bind_target(gl, &targets.mist);
        self.triangle.draw(gl);

        gl.bind_vertex_array(None);
    }

    /// The second half of `render()`, into the framebuffer that is bound:
    /// the blurred background, the mist over it, the drops refracting the
    /// background, and the shade of the rain page.
    pub unsafe fn draw(&self, gl: &glow::Context, framebuffer_width: i32, framebuffer_height: i32) {
        let Some(targets) = self.targets.as_ref() else {
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            return;
        };
        let o = &self.options;

        gl.disable(glow::DEPTH_TEST);
        gl.disable(glow::CULL_FACE);
        gl.disable(glow::SCISSOR_TEST);
        gl.disable(glow::STENCIL_TEST);
        if !self.es {
            gl.disable(glow::FRAMEBUFFER_SRGB);
        }
        gl.color_mask(true, true, true, true);
        gl.viewport(0, 0, framebuffer_width, framebuffer_height);
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);

        // drawBackground()
        gl.disable(glow::BLEND);
        self.blit_copy.use_program(gl);
        self.blit_copy.set_transforms(gl, &IDENTITY);
        self.blit_copy
            .set_texture(gl, "uMainTex", 0, &targets.blurry_background);
        self.triangle.draw(gl);

        gl.enable(glow::BLEND);
        gl.blend_func_separate(
            glow::SRC_ALPHA,
            glow::ONE_MINUS_SRC_ALPHA,
            glow::ONE,
            glow::ONE_MINUS_SRC_ALPHA,
        );
        self.mist_compose.use_program(gl);
        self.mist_compose.set_transforms(gl, &IDENTITY);
        self.mist_compose
            .set_texture(gl, "uMainTex", 0, &targets.mist_background);
        self.mist_compose
            .set_texture(gl, "uMistTex", 1, &targets.mist);
        self.mist_compose
            .set_vec4(gl, "uMistColor", f32x4(o.mist_color));
        self.triangle.draw(gl);

        // The final compose, with the same blend.
        let (w, h) = (self.width, self.height);
        self.compose.use_program(gl);
        self.compose.set_transforms(gl, &IDENTITY);
        self.compose
            .set_texture(gl, "uMainTex", 0, &targets.blurry_background);
        self.compose
            .set_texture(gl, "uRaindropTex", 1, &targets.raindrop_compose);
        self.compose
            .set_texture(gl, "uDropletTex", 2, &targets.droplet);
        self.compose.set_texture(gl, "uMistTex", 3, &targets.mist);
        self.compose.set_vec4(
            gl,
            "uBackgroundSize",
            [w as f32, h as f32, (1.0 / w) as f32, (1.0 / h) as f32],
        );
        self.compose.set_vec4(gl, "uColor", [1.0, 1.0, 1.0, 1.0]);
        self.compose.set_vec2(
            gl,
            "uSmoothRaindrop",
            [o.smooth_raindrop[0] as f32, o.smooth_raindrop[1] as f32],
        );
        self.compose.set_vec2(
            gl,
            "uRefractParams",
            [o.refract_base as f32, o.refract_scale as f32],
        );
        self.compose
            .set_vec4(gl, "uLightPos", f32x4(o.raindrop_light_pos));
        let diffuse = o.raindrop_diffuse_light;
        self.compose.set_vec4(
            gl,
            "uDiffuseColor",
            f32x4([diffuse[0], diffuse[1], diffuse[2], o.raindrop_shadow_offset]),
        );
        let specular = o.raindrop_specular_light;
        self.compose.set_vec4(
            gl,
            "uSpecularParams",
            f32x4([
                specular[0],
                specular[1],
                specular[2],
                o.raindrop_specular_shininess,
            ]),
        );
        self.compose
            .set_f32(gl, "uBump", o.raindrop_light_bump as f32);
        self.triangle.draw(gl);

        // The shade, black over the rain, alpha left alone.
        gl.blend_func_separate(
            glow::SRC_ALPHA,
            glow::ONE_MINUS_SRC_ALPHA,
            glow::ZERO,
            glow::ONE,
        );
        self.shade.use_program(gl);
        self.shade.set_transforms(gl, &IDENTITY);
        self.shade.set_vec2(
            gl,
            "uSize",
            [framebuffer_width as f32, framebuffer_height as f32],
        );
        self.triangle.draw(gl);

        gl.disable(glow::BLEND);
        gl.bind_vertex_array(None);
        for unit in 0..4 {
            gl.active_texture(glow::TEXTURE0 + unit);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
        gl.active_texture(glow::TEXTURE0);
    }

    pub unsafe fn delete(&mut self, gl: &glow::Context) {
        if let Some(targets) = self.targets.take() {
            targets.delete(gl);
        }
        self.raindrop_texture.delete(gl);
        self.white.delete(gl);
        self.original_background.delete(gl);
        for program in [
            &self.blit_copy,
            &self.blur,
            &self.mist_accumulate,
            &self.mist_compose,
            &self.compose,
            &self.erase,
            &self.raindrop,
            &self.droplet,
            &self.shade,
        ] {
            program.delete(gl);
        }
        self.triangle.delete(gl);
        self.quad.delete(gl);
        self.droplet_quad.delete(gl);
        gl.delete_buffer(self.instance_buffer);
    }
}

fn f32x4(v: [f64; 4]) -> [f32; 4] {
    [v[0] as f32, v[1] as f32, v[2] as f32, v[3] as f32]
}

/// `setFramebuffer()` on a render texture: draw into it, all of it.
unsafe fn bind_target(gl: &glow::Context, texture: &Texture) {
    gl.bind_framebuffer(glow::FRAMEBUFFER, texture.framebuffer);
    gl.viewport(0, 0, texture.width, texture.height);
}
