// Soft bloom around bright text.
// source: https://gist.github.com/qwerasd205/c3da6c610c8ffe17d6d2d3cc7068f17f
// credits: https://github.com/qwerasd205
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/bloom.glsl
//
// Coordinates: Ghostty on OpenGL hands the shader a bottom-left fragCoord origin
// (Shadertoy convention). This port flips tron's top-left coordinates to match.

// iChannel0 with a bottom-left origin.
fn channel0(uv: vec2<f32>) -> vec4<f32> {
    return terminal(vec2<f32>(uv.x, 1.0 - uv.y));
}

fn lum(c: vec4<f32>) -> f32 {
    return 0.299 * c.r + 0.587 * c.g + 0.114 * c.b;
}

// The original summed 24 golden-spiral samples around each pixel, weighted by
// their inverse distance. A blur of the same size gives the same glow for a few
// samples: what glows is each pixel brighter than 0.2, weighted by its brightness.
const RADIUS: f32 = 4.5;
const STRENGTH: f32 = 1.69;

fn blur_source(color: vec4<f32>) -> vec4<f32> {
    let l = lum(color);
    return select(vec4<f32>(0.0), color * l, l > 0.2);
}

fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = vec2<f32>(frag_coord.x, tron.resolution.y - frag_coord.y);
    let uv = fragCoord / tron.resolution;
    return channel0(uv) + terminal_blur(screen_uv, RADIUS) * STRENGTH;
}
