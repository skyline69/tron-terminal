// Raymarched lava-lamp blobs drifting behind dark terminal areas.
// License: CC BY-NC-SA 3.0 (https://creativecommons.org/licenses/by-nc-sa/3.0/), Shadertoy's default, as the original creator stated no license.
// INFO: This shader is a port of https://www.shadertoy.com/view/3sySRK
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/cineShader-Lava.glsl
//
// Coordinates: Ghostty on OpenGL hands the shader a bottom-left fragCoord origin
// (Shadertoy convention). This port flips tron's top-left coordinates to match.
//
// Performance: the original marches every pixel 64 steps through 16 blobs. The
// march here stops at a hit distance float precision can reach, and for
// background rays at MARCH_DEPTH, far enough behind the blobs that the shading
// looks the same. It runs about three times as fast.

// INFO: Change these variables to create some variation in the animation
const BLACK_BLEND_THRESHOLD: f32 = 0.4; // This is controls the dim of the screen
const COLOR_SPEED: f32 = 0.1;           // This controls the speed at which the colors change
const MOVEMENT_SPEED: f32 = 0.1;        // This controls the speed at which the balls move
// Depth the color fades out at.
const MAX_DEPTH: f32 = 6.0;
// Rays this far are background. Stopping them nearer shows rings in their shading.
const MARCH_DEPTH: f32 = 12.0;

fn opSmoothUnion(d1: f32, d2: f32, k: f32) -> f32 {
    let h = clamp(0.5 + 0.5 * (d2 - d1) / k, 0.0, 1.0);
    return mix(d2, d1, h) - k * h * (1.0 - h);
}

fn sdSphere(p: vec3<f32>, s: f32) -> f32 {
    return length(p) - s;
}

fn map(p: vec3<f32>) -> f32 {
    var d = 2.0;
    for (var i = 0; i < 16; i++) {
        let fi = f32(i);
        let time = tron.time * (fract(fi * 412.531 + 0.513) - 0.5) * 2.0;
        d = opSmoothUnion(
            sdSphere(p + sin(time * MOVEMENT_SPEED + fi * vec3<f32>(52.5126, 64.62744, 632.25)) * vec3<f32>(2.0, 2.0, 0.8), mix(0.5, 1.0, fract(fi * 412.531 + 0.5124))),
            d,
            0.4
        );
    }
    return d;
}

fn calcNormal(p: vec3<f32>) -> vec3<f32> {
    const h = 1e-5; // or some other value
    const k = vec2<f32>(1.0, -1.0);
    return normalize(k.xyy * map(p + k.xyy * h) +
                     k.yyx * map(p + k.yyx * h) +
                     k.yxy * map(p + k.yxy * h) +
                     k.xxx * map(p + k.xxx * h));
}

// iChannel0 with a bottom-left origin.
fn channel0(uv: vec2<f32>) -> vec4<f32> {
    return terminal(vec2<f32>(uv.x, 1.0 - uv.y));
}

fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = vec2<f32>(frag_coord.x, tron.resolution.y - frag_coord.y);
    let iResolution = tron.resolution;
    let uv = fragCoord / iResolution;

    let rayOri = vec3<f32>((uv - 0.5) * vec2<f32>(iResolution.x / iResolution.y, 1.0) * 6.0, 3.0);
    let rayDir = vec3<f32>(0.0, 0.0, -1.0);

    var depth = 0.0;
    var p = vec3<f32>(0.0);

    for (var i = 0; i < 64; i++) {
        p = rayOri + rayDir * depth;
        let dist = map(p);
        depth += dist;
        if dist < 1e-4 || depth > MARCH_DEPTH {
            break;
        }
    }

    depth = min(MAX_DEPTH, depth);
    let n = calcNormal(p);
    let b = max(0.0, dot(n, vec3<f32>(0.577)));
    var col = (0.5 + 0.5 * cos((b + tron.time * COLOR_SPEED * 3.0) + uv.xyx * 2.0 + vec3<f32>(0.0, 2.0, 4.0))) * (0.85 + b * 0.35);
    col *= exp(-depth * 0.15);

    let termUV = fragCoord / iResolution;
    let terminalColor = channel0(termUV);

    let alpha = step(length(terminalColor.rgb), BLACK_BLEND_THRESHOLD);
    let blendedColor = mix(terminalColor.rgb * 1.0, col.rgb * 0.3, alpha);

    return vec4<f32>(blendedColor, terminalColor.a);
}
