// TFT LCD look: a dark pixel grid over the terminal.
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/tft.glsl

/** Size of TFT "pixels" */
const resolution: f32 = 4.0;

/** Strength of effect */
const strength: f32 = 0.5;

// GLSL mod(): the result has the sign of y.
fn glsl_mod(x: f32, y: f32) -> f32 {
    return x - y * floor(x / y);
}

fn scanline(color: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    let scanline = step(1.2, glsl_mod(uv.y * tron.resolution.y, resolution));
    let grille = step(1.2, glsl_mod(uv.x * tron.resolution.x, resolution));
    return color * max(1.0 - strength, scanline * grille);
}

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let color = scanline(terminal(uv).rgb, uv);
    return vec4<f32>(color, 1.0);
}
