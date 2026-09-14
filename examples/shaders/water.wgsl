// Rippling water caustics that tint and wobble the terminal.
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/water.glsl

const TAU: f32 = 6.28318530718;
const MAX_ITER: i32 = 6;

// GLSL mod(): the result has the sign of y.
fn glsl_mod(x: vec2<f32>, y: f32) -> vec2<f32> {
    return x - y * floor(x / y);
}

fn shade(uv_in: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let water_color = vec3<f32>(1.0, 1.0, 1.0) * 0.5;
    let time = tron.time * 0.5 + 23.0;
    var uv = uv_in;

    let p = glsl_mod(uv * TAU, TAU) - 250.0;
    var i = p;
    var c = 1.0;
    let inten = 0.005;

    for (var n: i32 = 0; n < MAX_ITER; n++) {
        let t = time * (1.0 - (3.5 / f32(n + 1)));
        i = p + vec2<f32>(cos(t - i.x) + sin(t + i.y), sin(t - i.y) + cos(t + i.x));
        c += 1.0 / length(vec2<f32>(p.x / (sin(i.x + t) / inten), p.y / (cos(i.y + t) / inten)));
    }
    c /= f32(MAX_ITER);
    c = 1.17 - pow(c, 1.4);
    var color = vec3<f32>(pow(abs(c), 15.0));
    color = clamp((color + water_color) * 1.2, vec3<f32>(0.0), vec3<f32>(1.0));

    // perterb uv based on value of c from caustic calc above
    let tc = vec2<f32>(cos(c) - 0.75, sin(c) - 0.75) * 0.04;
    uv = clamp(uv + tc, vec2<f32>(0.0), vec2<f32>(1.0));

    var fragColor = terminal(uv);
    // give transparent pixels a color
    if fragColor.a == 0.0 {
        fragColor = vec4<f32>(1.0, 1.0, 1.0, 1.0);
    }
    return fragColor * vec4<f32>(color, 1.0);
}
