// Draws every terminal element as an instanced quad in a single draw call:
// solid rectangles (backgrounds, cursor, decorations), coverage masks (text)
// and color bitmaps (emoji).

struct Uniforms {
    // xy: surface size in pixels.
    viewport: vec4<f32>,
    // Block cursor rectangle in pixels (min xy, max xy). Text inside uses cursor_text.
    cursor_rect: vec4<f32>,
    cursor_text: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var mask_atlas: texture_2d<f32>;
@group(0) @binding(2) var color_atlas: texture_2d<f32>;

const KIND_SOLID: u32 = 0u;
const KIND_MASK: u32 = 1u;
const KIND_COLOR: u32 = 2u;
const KIND_CURLY: u32 = 3u;
const KIND_ROUNDED: u32 = 4u;

struct Instance {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv: vec4<f32>,
    @location(3) color: vec4<f32>,
    @location(4) kind: u32,
};

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) kind: u32,
    // Position inside the quad in pixels.
    @location(3) local: vec2<f32>,
    // Raw instance uv, used as parameters by curly underlines.
    @location(4) @interpolate(flat) params: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32, inst: Instance) -> VertexOut {
    // Triangle strip corners: (0,0) (1,0) (0,1) (1,1).
    let corner = vec2<f32>(f32(index & 1u), f32((index >> 1u) & 1u));
    let pixel = inst.pos + corner * inst.size;
    let ndc = pixel / u.viewport.xy * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);

    var out: VertexOut;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = mix(inst.uv.xy, inst.uv.zw, corner);
    out.color = inst.color;
    out.kind = inst.kind;
    out.local = corner * inst.size;
    out.params = inst.uv;
    return out;
}

@fragment
fn fs_main(v: VertexOut) -> @location(0) vec4<f32> {
    switch v.kind {
        case KIND_MASK: {
            let coverage = textureLoad(mask_atlas, vec2<i32>(v.uv), 0).r;
            var color = v.color;
            let p = v.position.xy;
            if all(p >= u.cursor_rect.xy) && all(p < u.cursor_rect.zw) {
                color = u.cursor_text;
            }
            let a = coverage * color.a;
            return vec4<f32>(color.rgb * a, a);
        }
        case KIND_CURLY: {
            // params: wave period, amplitude, stroke thickness, band height.
            let period = max(v.params.x, 1.0);
            let angle = v.position.x * 6.2831853 / period;
            let wave = v.params.w * 0.5 + v.params.y * sin(angle);
            let slope = v.params.y * 6.2831853 / period * cos(angle);
            let distance = abs(v.local.y - wave) / sqrt(1.0 + slope * slope);
            let a = clamp(v.params.z * 0.5 + 0.5 - distance, 0.0, 1.0) * v.color.a;
            return vec4<f32>(v.color.rgb * a, a);
        }
        case KIND_ROUNDED: {
            // params: corner radius, unused, box width and height. Signed distance to a
            // rounded box, anti-aliased over one pixel.
            let half = v.params.zw * 0.5;
            let q = abs(v.local - half) - half + vec2<f32>(v.params.x);
            let distance = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - v.params.x;
            let a = clamp(0.5 - distance, 0.0, 1.0) * v.color.a;
            return vec4<f32>(v.color.rgb * a, a);
        }
        case KIND_COLOR: {
            let texel = textureLoad(color_atlas, vec2<i32>(v.uv), 0);
            return vec4<f32>(texel.rgb * texel.a, texel.a);
        }
        default: {
            return vec4<f32>(v.color.rgb * v.color.a, v.color.a);
        }
    }
}
