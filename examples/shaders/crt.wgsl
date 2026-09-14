// CRT look: barrel distortion, scanlines and vignette.

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let centered = uv * 2.0 - 1.0;
    let warped = centered * (1.0 + 0.04 * dot(centered.yx, centered.yx));
    let tuv = warped * 0.5 + 0.5;
    if any(tuv < vec2<f32>(0.0)) || any(tuv > vec2<f32>(1.0)) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let color = terminal(tuv);
    let scanline = 0.85 + 0.15 * sin(frag_coord.y * 3.14159);
    let vignette = smoothstep(1.45, 0.35, length(centered));
    return vec4<f32>(color.rgb * scanline * vignette, color.a);
}
