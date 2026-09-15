// Underwater sun rays shimmering behind dark parts of the terminal.
// License: CC BY-NC-SA 3.0 (https://creativecommons.org/licenses/by-nc-sa/3.0/), Shadertoy's default, as the original creator stated no license.
// adapted by Alex Sherwin for Ghostty from https://www.shadertoy.com/view/lljGDt
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/underwater.glsl

const BLACK_BLEND_THRESHOLD: f32 = 0.4;

fn hash21(p_in: vec2<f32>) -> f32 {
    var p = fract(p_in * vec2<f32>(233.34, 851.73));
    p += dot(p, p + 23.45);
    return fract(p.x * p.y);
}

fn rayStrength(raySource: vec2<f32>, rayRefDirection: vec2<f32>, coord: vec2<f32>, seedA: f32, seedB: f32, speed: f32) -> f32 {
    let sourceToCoord = coord - raySource;
    let cosAngle = dot(normalize(sourceToCoord), rayRefDirection);

    // Add subtle dithering based on screen coordinates
    let dither = hash21(coord) * 0.015 - 0.0075;

    let ray = clamp(
        (0.45 + 0.15 * sin(cosAngle * seedA + tron.time * speed)) +
        (0.3 + 0.2 * cos(-cosAngle * seedB + tron.time * speed)) + dither,
        0.0, 1.0);

    // Smoothstep the distance falloff
    let distFade = smoothstep(0.0, tron.resolution.x, tron.resolution.x - length(sourceToCoord));
    return ray * mix(0.5, 1.0, distFade);
}

fn shade(uv_in: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let res = tron.resolution;
    var uv = uv_in;

    uv.y = 1.0 - uv.y;
    let coord = vec2<f32>(frag_coord.x, res.y - frag_coord.y);

    // Set the parameters of the sun rays
    let rayPos1 = vec2<f32>(res.x * 0.7, res.y * 1.1);
    let rayRefDir1 = normalize(vec2<f32>(1.0, 0.116));
    let raySeedA1 = 36.2214;
    let raySeedB1 = 21.11349;
    let raySpeed1 = 1.1;

    let rayPos2 = vec2<f32>(res.x * 0.8, res.y * 1.2);
    let rayRefDir2 = normalize(vec2<f32>(1.0, -0.241));
    const raySeedA2 = 22.39910;
    const raySeedB2 = 18.0234;
    const raySpeed2 = 0.9;

    // Calculate the colour of the sun rays on the current fragment
    let rays1 =
        vec4<f32>(1.0, 1.0, 1.0, 0.0) *
        rayStrength(rayPos1, rayRefDir1, coord, raySeedA1, raySeedB1, raySpeed1);

    let rays2 =
        vec4<f32>(1.0, 1.0, 1.0, 0.0) *
        rayStrength(rayPos2, rayRefDir2, coord, raySeedA2, raySeedB2, raySpeed2);

    var col = rays1 * 0.5 + rays2 * 0.4;

    // Attenuate brightness towards the bottom, simulating light-loss due to depth.
    // Give the whole thing a blue-green tinge as well.
    let brightness = 1.0 - (coord.y / res.y);
    col.r *= 0.05 + (brightness * 0.8);
    col.g *= 0.15 + (brightness * 0.6);
    col.b *= 0.3 + (brightness * 0.5);

    let termUV = uv_in;
    let terminalColor = terminal(termUV);

    let alpha = step(length(terminalColor.rgb), BLACK_BLEND_THRESHOLD);
    let blendedColor = mix(terminalColor.rgb * 1.0, col.rgb * 0.3, vec3<f32>(alpha));

    return vec4<f32>(blendedColor, terminalColor.a);
}
