// Slowly shifting three-color gradient behind dark terminal areas.
// credits: https://github.com/unkn0wncode
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/animated-gradient-shader.glsl
//
// Coordinates: Ghostty on OpenGL hands the shader a bottom-left fragCoord origin
// (Shadertoy convention). This port flips tron's top-left coordinates to match.

// Animation speed of the color cycle.
const SPEED: f32 = 0.2;

const COLOR1: vec3<f32> = vec3<f32>(0.1, 0.1, 0.5);
const COLOR2: vec3<f32> = vec3<f32>(0.5, 0.1, 0.1);
const COLOR3: vec3<f32> = vec3<f32>(0.1, 0.5, 0.1);

// iChannel0 with a bottom-left origin.
fn channel0(uv: vec2<f32>) -> vec4<f32> {
    return terminal(vec2<f32>(uv.x, 1.0 - uv.y));
}

fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = vec2<f32>(frag_coord.x, tron.resolution.y - frag_coord.y);
    let uv = fragCoord / tron.resolution;

    // Create seamless gradient animation
    var gradientFactor = (uv.x + uv.y) / 2.0;

    // Use smoothstep and multiple sin waves for smoother transition
    gradientFactor = smoothstep(0.0, 1.0, gradientFactor);

    // Create smooth circular animation
    let angle = tron.time * SPEED;

    // Smooth interpolation between colors using multiple mix operations
    let gradientStartColor = mix(
        mix(COLOR1, COLOR2, smoothstep(0.0, 1.0, sin(angle) * 0.5 + 0.5)),
        COLOR3,
        smoothstep(0.0, 1.0, sin(angle + 2.0) * 0.5 + 0.5)
    );

    let gradientEndColor = mix(
        mix(COLOR2, COLOR3, smoothstep(0.0, 1.0, sin(angle + 1.0) * 0.5 + 0.5)),
        COLOR1,
        smoothstep(0.0, 1.0, sin(angle + 3.0) * 0.5 + 0.5)
    );

    let gradientColor = mix(gradientStartColor, gradientEndColor, gradientFactor);

    let terminalColor = channel0(uv);
    let mask = 1.0 - step(0.5, dot(terminalColor.rgb, vec3<f32>(1.0)));
    let blendedColor = mix(terminalColor.rgb, gradientColor, mask);

    return vec4<f32>(blendedColor, terminalColor.a);
}
