// Wobbly drunken distortion with fBm noise and color fringing.
// Drunken stupor effect using fractal Brownian motion and Perlin noise
// (c) moni-dz (https://github.com/moni-dz)
// CC BY-NC-SA 4.0 (https://creativecommons.org/licenses/by-nc-sa/4.0/)
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/drunkard.glsl
//
// Coordinates: Ghostty on OpenGL hands the shader a bottom-left fragCoord origin
// (Shadertoy convention). This port flips tron's top-left coordinates to match.

fn hash2(p: vec2<f32>) -> vec2<f32> {
    var q = vec2<u32>(bitcast<u32>(p.x), bitcast<u32>(p.y));
    q = (q * vec2<u32>(1597334673u, 3812015801u)) ^ (q.yx * vec2<u32>(2798796415u, 1979697793u));
    return vec2<f32>(q) * (1.0 / f32(0xffffffffu)) * 2.0 - 1.0;
}

fn perlin2d(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);

    return mix(mix(dot(hash2(i + vec2<f32>(0.0, 0.0)), f - vec2<f32>(0.0, 0.0)),
                   dot(hash2(i + vec2<f32>(1.0, 0.0)), f - vec2<f32>(1.0, 0.0)), u.x),
               mix(dot(hash2(i + vec2<f32>(0.0, 1.0)), f - vec2<f32>(0.0, 1.0)),
                   dot(hash2(i + vec2<f32>(1.0, 1.0)), f - vec2<f32>(1.0, 1.0)), u.x), u.y);
}

const OCTAVES: i32 = 10;     // How many passes of fractal Brownian motion to perform
const GAIN: f32 = 0.5;       // How much should each pixel move
const LACUNARITY: f32 = 2.0; // How fast should each ripple be per pass

fn fbm(p: vec2<f32>) -> f32 {
    var sum = 0.0;
    var amp = 0.5;
    var freq = 1.0;

    for (var i = 0; i < OCTAVES; i++) {
        sum += amp * perlin2d(p * freq);
        freq *= LACUNARITY;
        amp *= GAIN;
    }

    return sum;
}

const NOISE_SCALE: f32 = 1.0;      // How distorted the image you want to be
const NOISE_INTENSITY: f32 = 0.05; // How strong the noise effect is
const ABERRATION: bool = true;     // Chromatic aberration
const ABERRATION_DELTA: f32 = 0.1; // How strong the chromatic aberration effect is
const ANIMATE: bool = true;
const SPEED: f32 = 0.4;            // Animation speed

// iChannel0 with a bottom-left origin.
fn channel0(uv: vec2<f32>) -> vec4<f32> {
    return terminal(vec2<f32>(uv.x, 1.0 - uv.y));
}

fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = vec2<f32>(frag_coord.x, tron.resolution.y - frag_coord.y);
    let uv = fragCoord / tron.resolution;
    let time = select(0.0, tron.time * SPEED, ANIMATE);

    let noisePos = uv * NOISE_SCALE + vec2<f32>(time);
    let noise = fbm(noisePos) * NOISE_INTENSITY;

    var col: vec3<f32>;

    if ABERRATION {
        col.r = channel0(uv + vec2<f32>(noise * (1.0 + ABERRATION_DELTA))).r;
        col.g = channel0(uv + vec2<f32>(noise)).g;
        col.b = channel0(uv + vec2<f32>(noise * (1.0 - ABERRATION_DELTA))).b;
    } else {
        let distortedUV = uv + vec2<f32>(noise);
        col = channel0(distortedUV).rgb;
    }

    return vec4<f32>(col, 1.0);
}
