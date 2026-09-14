// tron post-processing prelude. User shaders are appended below and must define:
//
//     fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32>
//
// `uv` runs from (0, 0) at the top left to (1, 1). `frag_coord` is in pixels.
// Read the rendered terminal with `terminal(uv)` and the final output of the
// previous frame with `previous(uv)`. Output premultiplied alpha.
//
// Redraws: shaders that read `tron.time` or `tron.frame` redraw every frame.
// Shaders that read `tron.cursor_change_time` or `tron.previous_cursor` redraw
// for one second after each cursor move, then stop until the next change.

struct TronUniforms {
    // Surface size in pixels.
    resolution: vec2<f32>,
    // Seconds since tron started.
    time: f32,
    // Frames rendered since the shader chain was loaded.
    frame: u32,
    // Cursor rectangle in pixels: x, y, width, height. Zero size when hidden.
    cursor: vec4<f32>,
    // Cell size in pixels.
    cell_size: vec2<f32>,
    // 1.0 when the window has focus.
    focused: f32,
    _padding: f32,
    // Background color, premultiplied.
    background: vec4<f32>,
    // Cursor rectangle before the last cursor change, same format as `cursor`.
    previous_cursor: vec4<f32>,
    // Value of `time` when the cursor rectangle last changed.
    cursor_change_time: f32,
    // Scalars, not a vec3: a vec3 would align to 16 and grow the struct.
    _padding2: f32,
    _padding3: f32,
    _padding4: f32,
};

@group(0) @binding(0) var<uniform> tron: TronUniforms;
@group(0) @binding(1) var terminal_texture: texture_2d<f32>;
@group(0) @binding(2) var terminal_sampler: sampler;
@group(0) @binding(3) var previous_texture: texture_2d<f32>;

fn terminal(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(terminal_texture, terminal_sampler, uv, 0.0);
}

// The final image of the previous frame, after all shaders. Transparent on the
// first frame and after a resize.
fn previous(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(previous_texture, terminal_sampler, uv, 0.0);
}

struct TronVertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn tron_vs(@builtin(vertex_index) index: u32) -> TronVertexOut {
    // One triangle covering the screen.
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: TronVertexOut;
    out.position = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn tron_fs(v: TronVertexOut) -> @location(0) vec4<f32> {
    return shade(v.uv, v.position.xy);
}

// ---- user shader ----
