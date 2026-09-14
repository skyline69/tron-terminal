// Ordered 4x4 Bayer dithering down to two levels per channel.
// Simple "dithering" effect
// (c) moni-dz (https://github.com/moni-dz)
// CC BY-NC-SA 4.0 (https://creativecommons.org/licenses/by-nc-sa/4.0/)
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/dither.glsl
//
// Coordinates: Ghostty on OpenGL hands the shader a bottom-left fragCoord origin
// (Shadertoy convention). This port flips tron's top-left coordinates to match.

// Packed bayer pattern using bit manipulation
const bayerPattern = array<i32, 4>(
    0x0514, // Encoding 0,8,2,10
    0xC4E6, // Encoding 12,4,14,6
    0x3B19, // Encoding 3,11,1,9
    0xF7D5  // Encoding 15,7,13,5
);

fn getBayerFromPacked(x: i32, y: i32) -> f32 {
    return f32((bayerPattern[y & 3] >> u32((x & 3) << 2u)) & 0xF) * (1.0 / 16.0);
}

const LEVELS: f32 = 2.0; // Available color steps per channel
const INV_LEVELS: f32 = 1.0 / LEVELS;

// iChannel0 with a bottom-left origin.
fn channel0(uv: vec2<f32>) -> vec4<f32> {
    return terminal(vec2<f32>(uv.x, 1.0 - uv.y));
}

fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = vec2<f32>(frag_coord.x, tron.resolution.y - frag_coord.y);
    let uv = fragCoord * (1.0 / tron.resolution);
    let color = channel0(uv).rgb;

    let threshold = getBayerFromPacked(i32(fragCoord.x), i32(fragCoord.y));
    let dithered = floor(color * LEVELS + threshold) * INV_LEVELS;

    return vec4<f32>(dithered, 1.0);
}
