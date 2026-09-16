// Twitchy RGB split plus perceptual (Oklab) glow around text.
// First it does a "chromatic aberration" by splitting the rgb signals by a product of sin functions
// over time, then it does a glow effect in a perceptual color space
// Based on kalgynirae's Ghostty passable glow shader and NickWest's Chromatic Aberration shader demo
// Passable glow:  https://github.com/kalgynirae/dotfiles/blob/main/ghostty/glow.glsl
// "Chromatic Aberration": https://www.shadertoy.com/view/Mds3zn
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/glow-rgbsplit-twitchy.glsl
//
// Coordinates: Ghostty on OpenGL hands the shader a bottom-left fragCoord origin
// (Shadertoy convention). This port flips tron's top-left coordinates to match.

// sRGB linear -> nonlinear transform from https://bottosson.github.io/posts/colorwrong/
fn f(x: f32) -> f32 {
    if x >= 0.0031308 {
        return 1.055 * pow(x, 1.0 / 2.4) - 0.055;
    } else {
        return 12.92 * x;
    }
}

fn f_inv(x: f32) -> f32 {
    if x >= 0.04045 {
        return pow((x + 0.055) / 1.055, 2.4);
    } else {
        return x / 12.92;
    }
}

// Oklab <-> linear sRGB conversions from https://bottosson.github.io/posts/oklab/
fn toOklab(rgb: vec4<f32>) -> vec4<f32> {
    let c = vec3<f32>(f_inv(rgb.r), f_inv(rgb.g), f_inv(rgb.b));
    let l = 0.4122214708 * c.r + 0.5363325363 * c.g + 0.0514459929 * c.b;
    let m = 0.2119034982 * c.r + 0.6806995451 * c.g + 0.1073969566 * c.b;
    let s = 0.0883024619 * c.r + 0.2817188376 * c.g + 0.6299787005 * c.b;
    let l_ = pow(l, 1.0 / 3.0);
    let m_ = pow(m, 1.0 / 3.0);
    let s_ = pow(s, 1.0 / 3.0);
    return vec4<f32>(
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
        rgb.a
    );
}

fn toRgb(oklab: vec4<f32>) -> vec4<f32> {
    let c = oklab.rgb;
    let l_ = c.r + 0.3963377774 * c.g + 0.2158037573 * c.b;
    let m_ = c.r - 0.1055613458 * c.g - 0.0638541728 * c.b;
    let s_ = c.r - 0.0894841775 * c.g - 1.2914855480 * c.b;
    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;
    let linear_srgb = vec3<f32>(
         4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s
    );
    return vec4<f32>(
        clamp(f(linear_srgb.r), 0.0, 1.0),
        clamp(f(linear_srgb.g), 0.0, 1.0),
        clamp(f(linear_srgb.b), 0.0, 1.0),
        oklab.a
    );
}

const periods = array<f32, 4>(6.0, 16.0, 19.0, 27.0);

fn offsetFunction(iTime: f32) -> f32 {
    var amount = 1.0;
    for (var i = 0; i < 4; i++) {
        amount *= 1.0 + 0.5 * sin(iTime * periods[i]);
    }
    //return amount;
    return amount * periods[3];
}

const DIM_CUTOFF: f32 = 0.35;
const BRIGHT_CUTOFF: f32 = 0.65;
const ABBERATION_FACTOR: f32 = 0.05;
const GLOW_RADIUS: f32 = 4.5;
const GLOW_STRENGTH: f32 = 8.44;

// What glows, in Oklab: the lightness of text brighter than DIM_CUTOFF, doubled
// above BRIGHT_CUTOFF, and its color. The blurred texture holds no negative
// values, so the color's a and b are stored around AB_ZERO, which 8 bits hold exactly.
const AB_ZERO: f32 = 128.0 / 255.0;

fn blur_source(color: vec4<f32>) -> vec4<f32> {
    let lab = toOklab(color);
    if lab.x <= DIM_CUTOFF {
        return vec4<f32>(0.0, AB_ZERO, AB_ZERO, 0.0);
    }
    let lightness = select(0.5, 1.0, lab.x > BRIGHT_CUTOFF) * lab.x;
    return vec4<f32>(lightness, lab.y + AB_ZERO, lab.z + AB_ZERO, 0.0);
}

// iChannel0 with a bottom-left origin.
fn channel0(uv: vec2<f32>) -> vec4<f32> {
    return terminal(vec2<f32>(uv.x, 1.0 - uv.y));
}

fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = vec2<f32>(frag_coord.x, tron.resolution.y - frag_coord.y);
    let iResolution = tron.resolution;
    let uv = fragCoord / iResolution;

    let amount = offsetFunction(tron.time);

    var col: vec3<f32>;
    col.r = channel0(vec2<f32>(uv.x - ABBERATION_FACTOR * amount / iResolution.x, uv.y)).r;
    col.g = channel0(uv).g;
    col.b = channel0(vec2<f32>(uv.x + ABBERATION_FACTOR * amount / iResolution.x, uv.y)).b;

    let splittedColor = vec4<f32>(col, 1.0);
    let source = toOklab(splittedColor);
    var dest = source;

    if source.x > DIM_CUTOFF {
        dest.x *= 1.2;
        // dest.x = 1.2;
    } else {
        let blurred = terminal_blur(screen_uv, GLOW_RADIUS);
        let glow = vec3<f32>(blurred.x * 0.1, (blurred.y - AB_ZERO) * 0.3, (blurred.z - AB_ZERO) * 0.3) * GLOW_STRENGTH;
        // float lightness_diff = clamp(glow.x - dest.x, 0.0, 1.0);
        // dest.x = lightness_diff;
        // dest.yz = dest.yz * (1.0 - lightness_diff) + glow.yz * lightness_diff;
        dest = vec4<f32>(dest.xyz + glow.xyz, dest.w);
    }

    return toRgb(dest);
}
