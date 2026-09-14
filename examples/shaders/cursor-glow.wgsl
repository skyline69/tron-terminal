// Pulsing glow around the cursor. Reads tron.time, so tron redraws continuously.

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let color = terminal(uv);
    if tron.cursor.z <= 0.0 {
        return color;
    }
    let center = tron.cursor.xy + tron.cursor.zw * 0.5;
    let distance = length((frag_coord - center) / tron.cell_size);
    let pulse = 0.6 + 0.4 * sin(tron.time * 3.0);
    let glow = exp(-distance * 0.9) * 0.35 * pulse * tron.focused;
    let tint = vec3<f32>(0.31, 0.84, 1.0);
    return vec4<f32>(color.rgb + tint * glow, max(color.a, glow));
}
