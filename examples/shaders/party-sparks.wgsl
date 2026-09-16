// Colorful plasma sparks burst from the cursor for a moment after it moves.
// Original shader by yakovgal on Shadertoy:
// https://www.shadertoy.com/user/yakovgal
// Ported to WGSL for tron from a Ghostty custom shader (party_sparks.glsl).
//
// Coordinates follow Ghostty's OpenGL convention (origin bottom left, y up):
// fragment and cursor positions are flipped into that space, so the original
// math runs unchanged. The unused cursor rectangle distance and the second
// color channel (always zero) of the original are left out.

const BLUE_SHIFT: vec3<f32> = vec3<f32>(1.0, 1.0, 1.0);

// === Configuration Constants ===
// How long tron redraws after the cursor moves: the effect must not change after it.
const TRON_CURSOR_DURATION: f32 = 0.2;
const DURATION: f32 = TRON_CURSOR_DURATION;
const FADE_IN_TIME: f32 = 0.06;
const FADE_OUT_TIME: f32 = 0.1;
const TOTAL_PARTICLES: f32 = 15.0; // default 50
const PARTICLE_SEPARATION: f32 = 20.0; // default 20
const RANDOM_SEED_OFFSET: f32 = 50.0; // default 50
const TIME_MULTIPLIER: f32 = 5.0; // Default 5
const TWO_PI: f32 = 6.283185; // 2 * PI
const GAUSSIAN_SCALE: f32 = -2.0; // default -2.0
const COLOR_INTENSITY: f32 = 4.0; // default 4.0
const COLOR_FADE_FACTOR: f32 = 0.3; // default 0.3

fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

fn pcg2d(input: vec2<u32>) -> vec2<u32> {
    var v = input * 1664525u + 1013904223u;
    v.x += v.y * 1664525u;
    v.y += v.x * 1664525u;
    v = v ^ (v >> vec2<u32>(16u));
    v.x += v.y * 1664525u;
    v.y += v.x * 1664525u;
    v = v ^ (v >> vec2<u32>(16u));
    return v;
}

// http://www.jcgt.org/published/0009/03/02/
fn pcg3d(input: vec3<u32>) -> vec3<u32> {
    var v = input * 1664525u + 1013904223u;
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    v = v ^ (v >> vec3<u32>(16u));
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    return v;
}

fn hash11(p: f32) -> f32 {
    return f32(pcg(u32(p))) / 4294967296.0;
}

fn hash21(p: f32) -> vec2<f32> {
    return vec2<f32>(pcg2d(vec2<u32>(u32(p), 0u))) / 4294967296.0;
}

fn hash33(p3: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(pcg3d(vec3<u32>(p3))) / 4294967296.0;
}

fn norm(value: vec2<f32>, is_position: f32) -> vec2<f32> {
    return (value * 2.0 - (tron.resolution * is_position)) / tron.resolution.y;
}

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let color = terminal(uv);
    if tron.cursor.z <= 0.0 {
        return color;
    }
    let elapsed = tron.time - tron.cursor_change_time;
    if elapsed >= DURATION {
        return color;
    }
    let fade_in = smoothstep(0.0, FADE_IN_TIME, elapsed);
    let fade_out = 1.0 - smoothstep(DURATION - FADE_OUT_TIME, DURATION, elapsed);
    let fade = clamp(fade_in * fade_out, 0.0, 1.0);

    // Into Ghostty's space: origin bottom left, cursor xy at its top left corner.
    let height = tron.resolution.y;
    let frag = vec2<f32>(frag_coord.x, height - frag_coord.y);
    let cursor = vec4<f32>(tron.cursor.x, height - tron.cursor.y, tron.cursor.z, tron.cursor.w);

    let center = norm(cursor.xy, 1.0);
    let vu = norm(frag, 1.0);
    let v1v = sin(vu.x * 10.0 + tron.time);
    let v2v = sin(vu.y * 10.0 + tron.time * 4.5);
    let v3v = sin((vu.x + vu.y) * 10.0 + tron.time * 0.5);
    let v4v = sin(length(vu) * 10.0 + tron.time * 2.0);
    let plasma = (v1v + v2v + v3v + v4v) / 4.0;
    let base_color = vec3<f32>(
        0.5 + 0.5 * sin(plasma * 6.28 + 0.0),
        0.5 + 0.5 * sin(plasma * 6.28 + 2.09),
        0.5 + 0.5 * sin(plasma * 6.28 + 4.18),
    );

    var c0 = 0.0;
    for (var i = 0.0; i < TOTAL_PARTICLES; i += 1.0) {
        var t = TIME_MULTIPLIER * tron.time + hash11(i);
        var v = hash21(i + RANDOM_SEED_OFFSET * floor(t));
        t = fract(t);
        v = vec2<f32>(sqrt(GAUSSIAN_SCALE * log(1.0 - v.x)), TWO_PI * v.y);
        v = PARTICLE_SEPARATION * v.x * vec2<f32>(cos(v.y), sin(v.y));

        var p = center + t * v - frag;
        p.x = p.x + cursor.x + cursor.z * 0.5;
        p.y = p.y + cursor.y - cursor.w * 0.5;
        c0 += COLOR_INTENSITY * (1.0 - t) / (1.0 + COLOR_FADE_FACTOR * dot(p, p));
    }

    var rgb = c0 * base_color;
    rgb += hash33(vec3<f32>(frag, tron.time * 256.0)) / 512.0;
    let mask = clamp(c0 * 0.2, 0.0, 1.0) * fade;
    // Additive, clamped. The original wrote alpha 1, which made a translucent window
    // turn opaque while the sparks ran; the sparks add their light to the alpha instead.
    let added = rgb * mask;
    let alpha = min(color.a + max(max(added.r, added.g), added.b), 1.0);
    return vec4<f32>(min(color.rgb + added, vec3<f32>(alpha)), alpha);
}
