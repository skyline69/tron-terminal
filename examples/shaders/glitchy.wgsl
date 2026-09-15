// Analog glitch: noisy jitter, RGB split and scanlines every 10 s.
// License: CC BY-NC-SA 3.0 (https://creativecommons.org/licenses/by-nc-sa/3.0/), Shadertoy's default, as the original creator stated no license.
// modified version of https://www.shadertoy.com/view/wld3WN
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/glitchy.glsl
//
// Coordinates: Ghostty on OpenGL hands the shader a bottom-left fragCoord origin
// (Shadertoy convention). This port flips tron's top-left coordinates to match.
// iFrame is tron.frame.

// amount of seconds for which the glitch loop occurs
const DURATION: f32 = 10.0;
// percentage of the duration for which the glitch is triggered
const AMT: f32 = 0.1;

const UI0: u32 = 1597334673u;
const UI1: u32 = 3812015801u;
const UI2: vec2<u32> = vec2<u32>(UI0, UI1);
const UI3: vec3<u32> = vec3<u32>(UI0, UI1, 2798796415u);
const UIF: f32 = 1.0 / f32(0xffffffffu);

// GLSL mod: x - y * floor(x / y).
fn glsl_mod(x: f32, y: f32) -> f32 {
    return x - y * floor(x / y);
}

// smoothstep that also accepts edge0 > edge1, like the GLSL original relies on.
fn smooth_step(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn SS(a: f32, b: f32, x: f32) -> f32 {
    return smooth_step(a, b, x) * smooth_step(b, a, x);
}

// Hash by David_Hoskins
fn hash33(p: vec3<f32>) -> vec3<f32> {
    var q = vec3<u32>(vec3<i32>(p)) * UI3;
    q = (q.x ^ q.y ^ q.z) * UI3;
    return -1.0 + 2.0 * vec3<f32>(q) * UIF;
}

// Gradient noise by iq
fn gnoise(x: vec3<f32>) -> f32 {
    // grid
    let p = floor(x);
    let w = fract(x);

    // quintic interpolant
    let u = w * w * w * (w * (w * 6.0 - 15.0) + 10.0);

    // gradients
    let ga = hash33(p + vec3<f32>(0.0, 0.0, 0.0));
    let gb = hash33(p + vec3<f32>(1.0, 0.0, 0.0));
    let gc = hash33(p + vec3<f32>(0.0, 1.0, 0.0));
    let gd = hash33(p + vec3<f32>(1.0, 1.0, 0.0));
    let ge = hash33(p + vec3<f32>(0.0, 0.0, 1.0));
    let gf = hash33(p + vec3<f32>(1.0, 0.0, 1.0));
    let gg = hash33(p + vec3<f32>(0.0, 1.0, 1.0));
    let gh = hash33(p + vec3<f32>(1.0, 1.0, 1.0));

    // projections
    let va = dot(ga, w - vec3<f32>(0.0, 0.0, 0.0));
    let vb = dot(gb, w - vec3<f32>(1.0, 0.0, 0.0));
    let vc = dot(gc, w - vec3<f32>(0.0, 1.0, 0.0));
    let vd = dot(gd, w - vec3<f32>(1.0, 1.0, 0.0));
    let ve = dot(ge, w - vec3<f32>(0.0, 0.0, 1.0));
    let vf = dot(gf, w - vec3<f32>(1.0, 0.0, 1.0));
    let vg = dot(gg, w - vec3<f32>(0.0, 1.0, 1.0));
    let vh = dot(gh, w - vec3<f32>(1.0, 1.0, 1.0));

    // interpolation
    let gNoise = va + u.x * (vb - va) +
                 u.y * (vc - va) +
                 u.z * (ve - va) +
                 u.x * u.y * (va - vb - vc + vd) +
                 u.y * u.z * (va - vc - ve + vg) +
                 u.z * u.x * (va - vb - ve + vf) +
                 u.x * u.y * u.z * (-va + vb + vc - vd + ve - vf - vg + vh);

    return 2.0 * gNoise;
}

// gradient noise in range [0, 1]
fn gnoise01(x: vec3<f32>) -> f32 {
    return 0.5 + 0.5 * gnoise(x);
}

// warp uvs for the crt effect
fn crt(uv_in: vec2<f32>) -> vec2<f32> {
    var uv = uv_in;
    let tht = atan2(uv.y, uv.x);
    var r = length(uv);
    // curve without distorting the center
    r /= (1.0 - 0.1 * r * r);
    uv.x = r * cos(tht);
    uv.y = r * sin(tht);
    return 0.5 * (uv + 1.0);
}

// iChannel0 with a bottom-left origin.
fn channel0(uv: vec2<f32>) -> vec4<f32> {
    return terminal(vec2<f32>(uv.x, 1.0 - uv.y));
}

fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = vec2<f32>(frag_coord.x, tron.resolution.y - frag_coord.y);
    let iResolution = tron.resolution;
    let uv = fragCoord / iResolution;
    let t = tron.time;

    // smoothed interval for which the glitch gets triggered
    let glitchAmount = SS(DURATION * 0.001, DURATION * AMT, glsl_mod(t, DURATION));
    var displayNoise = 0.0;
    var col = vec3<f32>(0.0);
    let eps = vec2<f32>(5.0 / iResolution.x, 0.0);
    var st = vec2<f32>(0.0);

    // analog distortion
    let y = uv.y * iResolution.y;
    var distortion = gnoise(vec3<f32>(0.0, y * 0.01, t * 500.0)) * (glitchAmount * 4.0 + 0.1);
    distortion *= gnoise(vec3<f32>(0.0, y * 0.02, t * 250.0)) * (glitchAmount * 2.0 + 0.025);

    displayNoise += 1.0;
    distortion += smoothstep(0.999, 1.0, sin((uv.y + t * 1.6) * 2.0)) * 0.02;
    distortion -= smoothstep(0.999, 1.0, sin((uv.y + t) * 2.0)) * 0.02;
    st = uv + vec2<f32>(distortion, 0.0);
    // chromatic aberration
    col.r += channel0(st + eps + distortion).r;
    col.g += channel0(st).g;
    col.b += channel0(st - eps - distortion).b;

    // white noise + scanlines
    displayNoise = 0.2 * clamp(displayNoise, 0.0, 1.0);
    col += (0.15 + 0.65 * glitchAmount) * (hash33(vec3<f32>(fragCoord, glsl_mod(f32(tron.frame),
        1000.0))).r) * displayNoise;
    col -= (0.25 + 0.75 * glitchAmount) * (sin(4.0 * t + uv.y * iResolution.y * 1.75))
        * displayNoise;
    return vec4<f32>(col, 1.0);
}
