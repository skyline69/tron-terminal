// Effects for tron's startup screen. Loaded only while the startup screen runs,
// after the user's own shaders.
//
// tron.scene: 1 splash, 2 setup, 3 tour. 0 means no scene.
// tron.params.x: power, from 0 (off) to 1 (on): a CRT style turn on and off.
// tron.params.y: strength of the neon grid behind the text.
// tron.params.z: glitch strength, for transitions.
// tron.params.w: bloom strength around bright text.

const CYAN: vec3<f32> = vec3<f32>(0.31, 0.84, 1.0);
const MAGENTA: vec3<f32> = vec3<f32>(0.76, 0.55, 1.0);

fn hash(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(12.9898, 78.233))) * 43758.5453);
}

// Neon perspective grid rolling toward the viewer below a glowing horizon.
fn grid(uv: vec2<f32>, time: f32) -> vec3<f32> {
    let horizon = 0.62;
    var color = MAGENTA * exp(-abs(uv.y - horizon) * 70.0) * 0.45;
    if uv.y > horizon {
        let depth = 1.0 / max(uv.y - horizon, 0.002);
        let x = (uv.x - 0.5) * depth * 1.8;
        let z = depth * 0.3 + time * 0.9;
        let across = 1.0 - smoothstep(0.0, fwidth(x) * 1.5, abs(fract(x + 0.5) - 0.5));
        let along = 1.0 - smoothstep(0.0, fwidth(z) * 1.5, abs(fract(z + 0.5) - 0.5));
        let fade = smoothstep(horizon, 1.0, uv.y);
        color += CYAN * max(across, along) * fade * 0.55;
    } else {
        // A few faint, twinkling stars above the horizon, each a small round point.
        let scaled = uv * tron.resolution / 14.0;
        let cell = floor(scaled);
        let offset = vec2<f32>(hash(cell + 7.0), hash(cell + 13.0)) * 0.6 + 0.2;
        let point = 1.0 - smoothstep(0.02, 0.09, length(fract(scaled) - offset));
        let twinkle = 0.55 + 0.45 * sin(time * 2.0 + hash(cell + 1.0) * 6.28);
        color += vec3<f32>(step(0.985, hash(cell)) * point * twinkle * 0.5);
    }
    return color;
}

// Glow from bright pixels around `uv`.
fn bloom(uv: vec2<f32>) -> vec3<f32> {
    let texel = 1.0 / tron.resolution;
    var sum = vec3<f32>(0.0);
    for (var i = 0; i < 12; i++) {
        let direction = vec2<f32>(cos(f32(i) * 0.5236), sin(f32(i) * 0.5236));
        for (var ring = 1; ring <= 3; ring++) {
            let sample = terminal(uv + direction * f32(ring) * 5.0 * texel).rgb;
            let brightness = max(max(sample.r, sample.g), sample.b);
            sum += sample * smoothstep(0.4, 0.95, brightness) / f32(ring);
        }
    }
    return sum / 22.0;
}

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let params = tron.params;
    let power = clamp(params.x, 0.0, 1.0);

    // Turning on: a bright line grows across the screen, then opens vertically.
    let opened_x = max(smoothstep(0.0, 0.35, power), 0.001);
    let opened_y = max(smoothstep(0.3, 1.0, power), 0.004);
    let screen = vec2<f32>((uv.x - 0.5) / opened_x + 0.5, (uv.y - 0.5) / opened_y + 0.5);
    let line = (1.0 - smoothstep(0.35, 0.7, power)) * exp(-abs(uv.y - 0.5) * 600.0) * step(abs(uv.x - 0.5), opened_x * 0.5);
    // Around the opening screen the window shows its background color, not black.
    if screen.x < 0.0 || screen.x > 1.0 || screen.y < 0.0 || screen.y > 1.0 {
        return vec4<f32>(tron.background.rgb + vec3<f32>(line), max(tron.background.a, line));
    }

    // Glitch: rows jump sideways and the color channels split.
    let glitch = params.z;
    let row = floor(screen.y * 48.0);
    let jump = (hash(vec2<f32>(row, floor(tron.time * 24.0))) - 0.5) * glitch * 0.06 * step(0.7, hash(vec2<f32>(row, 3.0)));
    let at = screen + vec2<f32>(jump, 0.0);
    let split = vec2<f32>(glitch * 0.005, 0.0);
    let base = terminal(at);
    var color = vec3<f32>(terminal(at + split).r, base.g, terminal(at - split).b);

    // The grid shows only where the terminal shows its background.
    let background = 1.0 - smoothstep(0.02, 0.1, distance(base.rgb, tron.background.rgb));
    color += grid(at, tron.time) * params.y * background;
    color += bloom(at) * params.w;

    let scanlines = 0.94 + 0.06 * sin(frag_coord.y * 3.14159);
    let vignette = smoothstep(1.25, 0.3, length(uv - 0.5));
    // The vignette fades toward the background color, so the edges match the window around it.
    color = mix(tron.background.rgb, color * scanlines, vignette) + vec3<f32>(line);
    let alpha = max(base.a, max(max(color.r, color.g), color.b));
    return vec4<f32>(color, alpha);
}
