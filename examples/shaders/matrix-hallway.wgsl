// Green matrix rain hallway behind dark parts of the terminal.
// based on the following Shader Toy entry
//
// [SH17A] Matrix rain. Created by Reinder Nijhoff 2017
// Creative Commons Attribution-NonCommercial-ShareAlike 4.0 International License.
// @reindernijhoff
//
// https://www.shadertoy.com/view/ldjBW1
//
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/matrix-hallway.glsl

const SPEED_MULTIPLIER: f32 = 1.0;
const GREEN_ALPHA: f32 = 0.33;

const BLACK_BLEND_THRESHOLD: f32 = 0.4;

fn R(p: vec3<f32>) -> f32 {
    return fract(1e2 * sin(p.x * 8.0 + p.y));
}

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let v = vec3<f32>(frag_coord, 1.0) / vec3<f32>(tron.resolution, 1.0) - 0.5;
    // vec3 s = .5 / abs(v);
    // scale?
    var s = 0.9 / abs(v);
    s.z = min(s.y, s.x);
    var i = ceil(8e2 * s.z * select(v.zyz, v.xzz, s.y < s.x)) * 0.1;
    let j = fract(i);
    i -= j;
    var p = vec3<f32>(9.0, f32(i32(tron.time * SPEED_MULTIPLIER * (9.0 + 8.0 * sin(i).x))), 0.0) + i;
    // The GLSL version reads the uninitialized output color here; start from black.
    var col = vec3<f32>(0.0);
    col.g = R(p) / s.z;
    p *= j;
    col *= select(0.0, GREEN_ALPHA, R(p) > 0.5 && j.x < 0.6 && j.y < 0.8);

    // Sample the terminal screen texture including alpha channel
    let terminalColor = terminal(uv);

    let alpha = step(length(terminalColor.rgb), BLACK_BLEND_THRESHOLD);
    let blendedColor = mix(terminalColor.rgb * 1.2, col, alpha);

    return vec4<f32>(blendedColor, terminalColor.a);
}
