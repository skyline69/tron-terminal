// Rainbow firework bursts behind dark terminal areas.
// This Ghostty shader is a port of https://www.shadertoy.com/view/lscGRl

// "Fireworks" by Martijn Steinrucken aka BigWings - 2015
// License Creative Commons Attribution-NonCommercial-ShareAlike 3.0 Unported License.
// Email:countfrolic@gmail.com Twitter:@The_ArtOfCode

// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/fireworks.glsl
//
// Coordinates: the Ghostty port flips y for a top-left fragCoord origin (Ghostty
// on Metal), which is tron's own convention, so frag_coord is used as is and
// sparks fall down.
// pow(t - 1., 2.) is written as a square: pow of a negative base is undefined
// in GLSL and WGSL, and the square is what the original means.

const BLACK_BLEND_THRESHOLD: f32 = 0.4;
const PI: f32 = 3.141592653589793238;
const TWOPI: f32 = 6.283185307179586;

const NUM_EXPLOSIONS: f32 = 3.0;
const NUM_PARTICLES: f32 = 42.0;

// smoothstep that also accepts edge0 > edge1, like the GLSL original relies on.
fn S(x: f32, y: f32, z: f32) -> f32 {
    let t = clamp((z - x) / (y - x), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn B(x: f32, y: f32, z: f32, w: f32) -> f32 {
    return S(x - z, x + z, w) * S(y + z, y - z, w);
}

fn saturate(x: f32) -> f32 {
    return clamp(x, 0.0, 1.0);
}

// Noise functions by Dave Hoskins
const MOD3: vec3<f32> = vec3<f32>(0.1031, 0.11369, 0.13787);
fn hash31(p: f32) -> vec3<f32> {
    var p3 = fract(vec3<f32>(p) * MOD3);
    p3 += dot(p3, p3.yzx + 19.19);
    return fract(vec3<f32>((p3.x + p3.y) * p3.z, (p3.x + p3.z) * p3.y, (p3.y + p3.z) * p3.x));
}
fn hash12(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.xyx) * MOD3);
    p3 += dot(p3, p3.yzx + 19.19);
    return fract((p3.x + p3.y) * p3.z);
}

fn circ(uv_in: vec2<f32>, pos: vec2<f32>, size_in: f32) -> f32 {
    let uv = uv_in - pos;

    let size = size_in * size_in;
    return S(size * 1.1, size, dot(uv, uv));
}

fn light(uv_in: vec2<f32>, pos: vec2<f32>, size_in: f32) -> f32 {
    let uv = uv_in - pos;

    let size = size_in * size_in;
    return size / dot(uv, uv);
}

fn explosion(uv: vec2<f32>, p: vec2<f32>, seed: f32, t: f32) -> vec3<f32> {
    var col = vec3<f32>(0.0);

    let en = hash31(seed);
    let baseCol = en;
    for (var i = 0.0; i < NUM_PARTICLES; i += 1.0) {
        let n = hash31(i) - 0.5;

        let startP = p - vec2<f32>(0.0, t * t * 0.1);
        let endP = startP + normalize(n.xy) * n.z - vec2<f32>(0.0, t * 0.2);

        let pt = 1.0 - (t - 1.0) * (t - 1.0);
        let pos = mix(p, endP, pt);
        var size = mix(0.01, 0.005, S(0.0, 0.1, pt));
        size *= S(1.0, 0.1, pt);

        var sparkle = (sin((pt + n.z) * 21.0) * 0.5 + 0.5);
        sparkle = pow(sparkle, pow(en.x, 3.0) * 50.0) * mix(0.01, 0.01, en.y * n.y);

        //size += sparkle*B(.6, 1., .1, t);
        size += sparkle * B(en.x, en.y, en.z, t);

        col += baseCol * light(uv, pos, size);
    }

    return col;
}

fn Rainbow(c_in: vec3<f32>) -> vec3<f32> {
    let t = tron.time;

    let avg = (c_in.r + c_in.g + c_in.b) / 3.0;
    var c = avg + (c_in - avg) * sin(vec3<f32>(0.0, 0.333, 0.666) + t);

    c += sin(vec3<f32>(0.4, 0.3, 0.3) * t + vec3<f32>(1.1244, 3.43215, 6.435)) * vec3<f32>(0.4, 0.1, 0.5);

    return c;
}

fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = frag_coord;
    let iResolution = tron.resolution;
    var uv = fragCoord / iResolution;
    uv.x -= 0.5;
    uv.x *= iResolution.x / iResolution.y;

    // Flip the y-axis so that the gravity is downwards
    uv.y = -uv.y + 1.0;

    let n = hash12(uv + 10.0);
    let t = tron.time * 0.5;

    var c = vec3<f32>(0.0);

    for (var i = 0.0; i < NUM_EXPLOSIONS; i += 1.0) {
        var et = t + i * 1234.45235;
        let id = floor(et);
        et -= id;

        var p = hash31(id).xy;
        p.x -= 0.5;
        p.x *= 1.6;
        c += explosion(uv, p, id, et);
    }
    c = Rainbow(c);

    let termUV = fragCoord / iResolution;
    let terminalColor = terminal(termUV);

    let alpha = step(length(terminalColor.rgb), BLACK_BLEND_THRESHOLD);
    let blendedColor = mix(terminalColor.rgb * 1.0, c.rgb * 0.3, alpha);

    return vec4<f32>(blendedColor, terminalColor.a);
}
