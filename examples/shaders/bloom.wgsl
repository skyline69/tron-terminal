// Neon glow around bright text.

// Only what is brighter than this glows.
fn blur_source(color: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(max(color.rgb - vec3<f32>(0.35), vec3<f32>(0.0)), 1.0);
}

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let base = terminal(uv);
    let glow = terminal_blur(uv, 4.0).rgb * 1.9;
    return vec4<f32>(base.rgb + glow, base.a);
}
