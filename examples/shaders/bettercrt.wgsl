// Curved CRT screen with dark scanlines.
// Original shader collected from: https://www.shadertoy.com/view/WsVSzV
// Licensed under Shadertoy's default since the original creator didn't provide any license. (CC BY NC SA 3.0)
// Slight modifications were made to give a green-ish effect.

// This shader was modified by April Hall (arithefirst)
// Sourced from https://github.com/m-ahdal/ghostty-shaders/blob/main/retro-terminal.glsl
// Changes made:
// - Removed tint
// - Made the boundaries match ghostty's background color

// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/bettercrt.glsl
//
// Coordinates: Ghostty on OpenGL hands the shader a bottom-left fragCoord origin
// (Shadertoy convention). This port flips tron's top-left coordinates to match.
// Outside the warped screen the sampler clamps to the edge, which shows the
// terminal background like Ghostty does.

const warp: f32 = 0.25; // simulate curvature of CRT monitor
const scan: f32 = 0.50; // simulate darkness between scanlines

// iChannel0 with a bottom-left origin.
fn channel0(uv: vec2<f32>) -> vec4<f32> {
    return terminal(vec2<f32>(uv.x, 1.0 - uv.y));
}

fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = vec2<f32>(frag_coord.x, tron.resolution.y - frag_coord.y);

    // squared distance from center
    var uv = fragCoord / tron.resolution;
    var dc = abs(0.5 - uv);
    dc *= dc;

    // warp the fragment coordinates
    uv.x -= 0.5; uv.x *= 1.0 + (dc.y * (0.3 * warp)); uv.x += 0.5;
    uv.y -= 0.5; uv.y *= 1.0 + (dc.x * (0.4 * warp)); uv.y += 0.5;

    // determine if we are drawing in a scanline
    let apply = abs(sin(fragCoord.y) * 0.25 * scan);

    // sample the texture
    let color = channel0(uv).rgb;

    // mix the sampled color with the scanline intensity
    return vec4<f32>(mix(color, vec3<f32>(0.0), apply), 1.0);
}
