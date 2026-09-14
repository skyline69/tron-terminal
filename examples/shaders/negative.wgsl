// Inverts the colors of the terminal.
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/negative.glsl

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let color = terminal(uv);
    return vec4<f32>(1.0 - color.x, 1.0 - color.y, 1.0 - color.z, color.w);
}
