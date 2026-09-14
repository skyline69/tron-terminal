// A dim, sparse starfield drifting slowly behind the terminal background.
// starfield.glsl - from https://github.com/0xhckr/ghostty-shaders
// Tuned down: dimmer, sparser, slower than upstream. Optimized for fill rate.
// Ported to WGSL for tron from a tuned copy of
// https://github.com/hackr-sh/ghostty-shaders/blob/main/starfield.glsl

// transparent background
const TRANSPARENT: bool = false;

// terminal contents luminance threshold to be considered background (0.0 to 1.0)
const THRESHOLD: f32 = 0.15;

// divisions of grid (lower = fewer stars; upstream 30.)
const REPEATS: f32 = 12.0;

// number of layers - the single biggest cost knob, work scales linearly
const LAYERS: f32 = 12.0;
const INV_LAYERS: f32 = 1.0 / LAYERS;

// overall star brightness (upstream is effectively 1.0)
const BRIGHTNESS: f32 = 0.08;

// time multiplier (lower = slower drift; upstream 1.0)
const SPEED: f32 = 0.25;

// cut the 1/r^2 glow tails - only star cores survive, no ambient haze
const GLOW_CUT: f32 = 0.02;

// hard ceiling on star core brightness (additive blend would blow out otherwise)
const MAX_STAR: f32 = 0.55;

// star colors
const WHITE: vec3<f32> = vec3<f32>(1.0); // Set star color to pure white

fn luminance(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn n21(input: vec2<f32>) -> f32 {
    var p = fract(input * vec2<f32>(233.34, 851.73));
    p += dot(p, p + 23.45);
    return fract(p.x * p.y);
}

fn n22(p: vec2<f32>) -> vec2<f32> {
    let n = n21(p);
    return vec2<f32>(n, n21(p + n));
}

// Stars are pure white, so one layer's contribution is a single intensity.
fn stars(input: vec2<f32>, offset: f32, aspect: f32, t_base: f32) -> f32 {
    let time_scale = t_base - offset * INV_LAYERS;
    let trans = fract(time_scale);
    let new_rnd = floor(time_scale);

    // Translate uv then scale for center.
    var uv = (input - 0.5) * trans + 0.5;

    // Create square aspect ratio (aspect is hoisted out of the layer loop)
    uv.x *= aspect;

    // Create boxes
    uv *= REPEATS;

    // Get position
    let ipos = floor(uv);

    // Return uv as 0 to 1
    uv = fract(uv);

    // Calculate random xy and size
    let rnd_xy = n22(vec2<f32>(new_rnd) + ipos * (offset + 1.0)) * 0.9 + 0.05;
    let rnd_size = n21(ipos) * 100.0 + 200.0;

    let j = (rnd_xy - uv) * rnd_size;
    var sparkle = 1.0 / dot(j, j);

    // Drop the wide faint halo, keep only the core
    sparkle = max(sparkle - GLOW_CUT, 0.0);

    return sparkle * smoothstep(1.0, 0.8, trans);
}

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    // Sample the terminal screen texture including alpha channel
    let terminal_color = terminal(uv);

    // Make a mask that is 1.0 where the terminal content is not black
    let mask = 1.0 - step(THRESHOLD, luminance(terminal_color.rgb));

    // Where terminal content covers the background the stars would be thrown away.
    if mask < 0.5 && !TRANSPARENT {
        return terminal_color;
    }

    // Loop-invariant terms, computed once instead of once per layer
    let aspect = tron.resolution.x / tron.resolution.y;
    let t_base = -tron.time * SPEED * INV_LAYERS;

    var acc = 0.0;
    for (var i = 0.0; i < LAYERS; i += 1.0) {
        acc += stars(uv, i, aspect, t_base);
    }

    var col = WHITE * min(acc * BRIGHTNESS, MAX_STAR);

    if TRANSPARENT {
        col += terminal_color.rgb;
    }

    // Additive: keeps the theme background color.
    let blended = terminal_color.rgb + col * mask;

    // Apply terminal's alpha to control overall opacity
    return vec4<f32>(blended, terminal_color.a);
}
