// Static blue-to-red diagonal gradient behind dark terminal areas.
// credits: https://github.com/unkn0wncode
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/gradient-background.glsl
//
// Coordinates: Ghostty on OpenGL hands the shader a bottom-left fragCoord origin
// (Shadertoy convention). This port flips tron's top-left coordinates to match.

// Define gradient colors (adjust to your preference)
const GRADIENT_START_COLOR: vec3<f32> = vec3<f32>(0.1, 0.1, 0.5); // Start color (e.g., dark blue)
const GRADIENT_END_COLOR: vec3<f32> = vec3<f32>(0.5, 0.1, 0.1); //      End color (e.g., dark red)

// iChannel0 with a bottom-left origin.
fn channel0(uv: vec2<f32>) -> vec4<f32> {
    return terminal(vec2<f32>(uv.x, 1.0 - uv.y));
}

fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = vec2<f32>(frag_coord.x, tron.resolution.y - frag_coord.y);

    // Normalize pixel coordinates (range from 0 to 1)
    let uv = fragCoord / tron.resolution;

    // Create a gradient from bottom right to top left as a function (x + y)/2
    let gradientFactor = (uv.x + uv.y) / 2.0;

    let gradientColor = mix(GRADIENT_START_COLOR, GRADIENT_END_COLOR, gradientFactor);

    // Sample the terminal screen texture including alpha channel
    let terminalColor = channel0(uv);

    // Make a mask that is 1.0 where the terminal content is not black
    let mask = 1.0 - step(0.5, dot(terminalColor.rgb, vec3<f32>(1.0)));
    let blendedColor = mix(terminalColor.rgb, gradientColor, mask);

    // Apply terminal's alpha to control overall opacity
    return vec4<f32>(blendedColor, terminalColor.a);
}
