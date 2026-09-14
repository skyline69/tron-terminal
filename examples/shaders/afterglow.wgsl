// Phosphor afterglow: text that disappears fades out over a few frames.
// Uses `previous` and redraws while `tron.time` changes.

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let color = terminal(uv);
    let fading = previous(uv) * 0.82;
    return max(color, fading);
}
