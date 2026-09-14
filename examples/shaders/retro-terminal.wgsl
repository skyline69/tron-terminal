// Curved teal CRT monitor with dark scanlines.
// Original shader collected from: https://www.shadertoy.com/view/WsVSzV
// Licensed under Shadertoy's default since the original creator didn't provide any license. (CC BY NC SA 3.0)
// Slight modifications were made to give a green-ish effect.
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/retro-terminal.glsl

const warp: f32 = 0.25; // simulate curvature of CRT monitor
const scan: f32 = 0.50; // simulate darkness between scanlines

fn shade(uv_in: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    // squared distance from center
    var uv = uv_in;
    var dc = abs(0.5 - uv);
    dc *= dc;

    // warp the fragment coordinates
    uv.x -= 0.5; uv.x *= 1.0 + (dc.y * (0.3 * warp)); uv.x += 0.5;
    uv.y -= 0.5; uv.y *= 1.0 + (dc.x * (0.4 * warp)); uv.y += 0.5;

    // sample inside boundaries, otherwise set to black
    if uv.y > 1.0 || uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    // determine if we are drawing in a scanline
    let apply = abs(sin(frag_coord.y) * 0.5 * scan);

    // sample the texture and apply a teal tint
    let color = terminal(uv).rgb;
    let tealTint = vec3<f32>(0.0, 0.8, 0.6); // teal color (slightly more green than blue)

    // mix the sampled color with the teal tint based on scanline intensity
    return vec4<f32>(mix(color * tealTint, vec3<f32>(0.0), apply), 1.0);
}
