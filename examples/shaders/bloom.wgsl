// Neon glow around bright text.

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let base = terminal(uv);
    let texel = 1.0 / tron.resolution;
    var glow = vec3<f32>(0.0);
    var total = 0.0;
    for (var x = -3; x <= 3; x++) {
        for (var y = -3; y <= 3; y++) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel * 2.0;
            let weight = exp(-f32(x * x + y * y) / 6.0);
            let bright = max(terminal(uv + offset).rgb - vec3<f32>(0.35), vec3<f32>(0.0));
            glow += bright * weight;
            total += weight;
        }
    }
    return vec4<f32>(base.rgb + glow / total * 1.6, base.a);
}
