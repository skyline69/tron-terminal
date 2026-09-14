// A soft spotlight wandering over a dimmed terminal.
// Created by Paul Robello
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/spotlight.glsl

// Smooth oscillating function that varies over time
fn smoothOscillation(t: f32, frequency: f32, phase: f32) -> f32 {
    return sin(t * frequency + phase);
}

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    // Used to fix distortion when calculating distance to circle center
    let ratio = vec2<f32>(tron.resolution.x / tron.resolution.y, 1.0);

    // Get the texture from iChannel0
    let texColor = terminal(uv);

    // Spotlight center moving based on a smooth random pattern
    let time = tron.time * 1.0; // Control speed of motion
    let spotlightCenter = vec2<f32>(
        0.5 + 0.4 * smoothOscillation(time, 1.0, 0.0),  // Smooth X motion
        0.5 + 0.4 * smoothOscillation(time, 1.3, 3.14159) // Smooth Y motion with different frequency and phase
    );

    // Distance from the spotlight center
    let distanceToCenter = distance(uv * ratio, spotlightCenter);

    // Spotlight intensity based on distance
    let spotlightRadius = 0.25; // Spotlight radius
    let softness = 20.0;       // Spotlight edge softness. Higher values have sharper edge
    let spotlightIntensity = smoothstep(spotlightRadius, spotlightRadius - (1.0 / softness), distanceToCenter);

    // Ambient light level
    let ambientLight = 0.5; // Controls the minimum brightness across the texture

    // Combine the spotlight effect with the texture
    let spotlightEffect = texColor.rgb * mix(vec3<f32>(ambientLight), vec3<f32>(1.0), spotlightIntensity);

    // Final color output
    return vec4<f32>(spotlightEffect, texColor.a);
}
