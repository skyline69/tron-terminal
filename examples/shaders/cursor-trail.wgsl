// A smear from the previous cursor position to the current one that fades
// over 150 ms. Redraws only for a moment after the cursor moves.

// How long tron redraws after the cursor moves: the effect must not change after it.
const TRON_CURSOR_DURATION: f32 = 0.15;
const DURATION: f32 = TRON_CURSOR_DURATION;
const TINT: vec3<f32> = vec3<f32>(0.31, 0.84, 1.0);

// Distance from `p` to the segment `a`-`b`.
fn segment_distance(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let ab = b - a;
    let t = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-4), 0.0, 1.0);
    return length(p - a - ab * t);
}

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let color = terminal(uv);
    let progress = (tron.time - tron.cursor_change_time) / DURATION;
    if progress >= 1.0 || tron.cursor.z <= 0.0 || tron.previous_cursor.z <= 0.0 {
        return color;
    }
    let start = tron.previous_cursor.xy + tron.previous_cursor.zw * 0.5;
    let to = tron.cursor.xy + tron.cursor.zw * 0.5;
    // The tail catches up with the head as the trail fades.
    let ease = 1.0 - pow(1.0 - progress, 3.0);
    let tail = mix(start, to, ease);
    let radius = min(tron.cursor.z, tron.cursor.w) * 0.5;
    let distance = segment_distance(frag_coord, tail, to);
    let coverage = clamp(radius - distance + 0.5, 0.0, 1.0);
    let strength = coverage * (1.0 - progress) * 0.6 * tron.focused;
    return vec4<f32>(mix(color.rgb, TINT * max(color.a, strength), strength), max(color.a, strength));
}
