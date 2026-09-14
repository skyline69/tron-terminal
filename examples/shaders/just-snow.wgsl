// Parallax snowfall with depth of field over the terminal.
// Copyright (c) 2013 Andrew Baldwin (twitter: baldand, www: http://thndl.com)
// License = Attribution-NonCommercial-ShareAlike (http://creativecommons.org/licenses/by-nc-sa/3.0/deed.en_US)

// "Just snow"
// Simple (but not cheap) snow made from multiple parallax layers with randomly positioned
// flakes and directions. Also includes a DoF effect. Pan around with mouse.

// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/just-snow.glsl
// Port note: the Ghostty version has no mouse panning, neither does this one.

const LIGHT_SNOW: bool = true; // Set this to false for a blizzard

// LIGHT_SNOW
const LIGHT_LAYERS: i32 = 50;
const LIGHT_DEPTH: f32 = 0.5;
const LIGHT_WIDTH: f32 = 0.3;
const LIGHT_SPEED: f32 = 0.6;
// BLIZZARD
const BLIZZARD_LAYERS: i32 = 200;
const BLIZZARD_DEPTH: f32 = 0.1;
const BLIZZARD_WIDTH: f32 = 0.8;
const BLIZZARD_SPEED: f32 = 1.5;

const LAYERS: i32 = select(BLIZZARD_LAYERS, LIGHT_LAYERS, LIGHT_SNOW);
const DEPTH: f32 = select(BLIZZARD_DEPTH, LIGHT_DEPTH, LIGHT_SNOW);
const WIDTH: f32 = select(BLIZZARD_WIDTH, LIGHT_WIDTH, LIGHT_SNOW);
const SPEED: f32 = select(BLIZZARD_SPEED, LIGHT_SPEED, LIGHT_SNOW);

// GLSL mod(): the result has the sign of y.
fn glsl_mod(x: f32, y: f32) -> f32 {
    return x - y * floor(x / y);
}

fn glsl_mod2(x: vec2<f32>, y: f32) -> vec2<f32> {
    return x - y * floor(x / y);
}

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let p = mat3x3<f32>(13.323122, 23.5112, 21.71123, 21.1212, 28.7312, 11.9312, 21.8112, 14.7212, 61.3934);

    var acc = vec3<f32>(0.0);
    let dof = 5.0 * sin(tron.time * 0.1);
    for (var i: i32 = 0; i < LAYERS; i++) {
        let fi = f32(i);
        var q = -uv * (1.0 + fi * DEPTH);
        q += vec2<f32>(q.y * (WIDTH * glsl_mod(fi * 7.238917, 1.0) - WIDTH * 0.5), SPEED * tron.time / (1.0 + fi * DEPTH * 0.03));
        let n = vec3<f32>(floor(q), 31.189 + fi);
        let m = floor(n) * 0.00001 + fract(n);
        let mp = (31415.9 + m) / fract(p * m);
        let r = fract(mp);
        var s = abs(glsl_mod2(q, 1.0) - 0.5 + 0.9 * r.xy - 0.45);
        s += 0.01 * abs(2.0 * fract(10.0 * q.yx) - 1.0);
        let d = 0.6 * max(s.x - s.y, s.x + s.y) + max(s.x, s.y) - 0.01;
        let edge = 0.005 + 0.05 * min(0.5 * abs(fi - 5.0 - dof), 1.0);
        acc += vec3<f32>(smoothstep(edge, -edge, d) * (r.x / (1.0 + 0.02 * fi * DEPTH)));
    }

    // Sample the terminal screen texture including alpha channel
    let terminalColor = terminal(uv);

    // Combine the snow effect with the terminal color
    let blendedColor = terminalColor.rgb + acc;

    // Use the terminal's original alpha
    return vec4<f32>(blendedColor, terminalColor.a);
}
