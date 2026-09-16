// Stylized video-game CRT: curve, fringing, grille, flicker, bloom.
// In-game CRT shader
// Author: sarphiv
// License: CC BY-NC-SA 4.0
// Description:
//   Shader for Ghostty with a focus on being usable while looking like a stylized CRT terminal from a modern video game.

// Based on:
//   1. https://gist.github.com/mitchellh/39d62186910dcc27cad097fed16eb882 (forces the choice of license)
//   2. https://gist.github.com/qwerasd205/c3da6c610c8ffe17d6d2d3cc7068f17f
//   3. https://gist.github.com/seanwcom/0fbe6b270aaa5f28823e053d3dbb14ca
//   4. https://www.shadertoy.com/view/ltB3zD

// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/in-game-crt.glsl
//
// Coordinates: Ghostty on OpenGL hands the shader a bottom-left fragCoord origin
// (Shadertoy convention). This port flips tron's top-left coordinates to match.
// WGSL has no preprocessor, so settings cannot be left undefined. Set a strength
// to 0.0 (or VIGNETTE_BRIGHTNESS to 1.0 with VIGNETTE_SPREAD 0.0) to disable an
// effect.
// The output alpha is BACKGROUND_OPACITY, as in the original.



// Settings:
// How straight the terminal is in each axis
// (x, y) \in R^2 : x, y > 0
const CURVE: vec2<f32> = vec2<f32>(13.0, 11.0);

// How far apart the different colors are from each other
// x \in R
const COLOR_FRINGING_SPREAD: f32 = 0.1;

// How much the ghost images are spread out
// x \in R : x >= 0
const GHOSTING_SPREAD: f32 = 0.75;
// How visible ghost images are
// x \in R : x >= 0
const GHOSTING_STRENGTH: f32 = 0.1;

// How much of the non-linearly darkened colors are mixed in
// [0, 1]
const DARKEN_MIX: f32 = 0.4;

// How far in the vignette spreads
// x \in R : x >= 0
const VIGNETTE_SPREAD: f32 = 0.4;
// How bright the vignette is
// x \in R : x >= 0
const VIGNETTE_BRIGHTNESS: f32 = 20.0;

// Tint all colors
// [0, 1]^3
const TINT: vec3<f32> = vec3<f32>(0.93, 1.00, 0.96);

// How visible the scan line effect is
// NOTE: Technically these are not scan lines, but rather the lack of them
// [0, 1]
const SCAN_LINES_STRENGTH: f32 = 0.20;
// How bright the spaces between the lines are
// [0, 1]
const SCAN_LINES_VARIANCE: f32 = 0.35;
// Pixels per scan line effect
// x \in R : x > 0
const SCAN_LINES_PERIOD: f32 = 4.0;

// How visible the aperture grille is
// x \in R : x >= 0
const APERTURE_GRILLE_STRENGTH: f32 = 0.3;
// Pixels per aperture grille
// x \in R : x > 0
const APERTURE_GRILLE_PERIOD: f32 = 2.0;

// How much the screen flickers
// x \in R : x >= 0
const FLICKER_STRENGTH: f32 = 0.04;
// How fast the screen flickers
// x \in R : x > 0
const FLICKER_FREQUENCY: f32 = 15.0;

// How much noise is added to filled areas
// [0, 1]
const NOISE_CONTENT_STRENGTH: f32 = 0.25;
// How much noise is added everywhere
// [0, 1]
const NOISE_UNIFORM_STRENGTH: f32 = 0.25;

// How big the bloom is, in pixels
const BLOOM_RADIUS: f32 = 32.0;
// How visible the bloom is
const BLOOM_STRENGTH: f32 = 0.0338;

// Backgrond opacity
// [0, 1]
const BACKGROUND_OPACITY: f32 = 0.8;



// Constants:
const PI: f32 = 3.1415926535897932384626433832795;
const PHI: f32 = 1.61803398874989484820459;

// GLSL mod: x - y * floor(x / y).
fn glsl_mod(x: f32, y: f32) -> f32 {
    return x - y * floor(x / y);
}

// iChannel0 with a bottom-left origin.
fn channel0(uv: vec2<f32>) -> vec4<f32> {
    return terminal(vec2<f32>(uv.x, 1.0 - uv.y));
}

// Functions:

// What blooms: every pixel, weighted by its brightness.
fn blur_source(color: vec4<f32>) -> vec4<f32> {
    return color * (0.299 * color.r + 0.587 * color.g + 0.114 * color.b);
}
fn gold_v2_noise(xy: vec2<f32>, seed: f32) -> f32 {
    return fract(sin(distance(xy * PHI, xy) * seed) * xy.x * xy.y);
}


fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = vec2<f32>(frag_coord.x, tron.resolution.y - frag_coord.y);
    let iResolution = tron.resolution;
    let iTime = tron.time;

    // Get texture coordinates
    var uv = fragCoord.xy / iResolution.xy;

    // Curve texture coordinates to mimic non-flat CRT monior
    uv = (uv - 0.5) * 2.0;
    uv *= 1.0 + pow((abs(vec2<f32>(uv.y, uv.x)) / CURVE), vec2<f32>(2.0));
    uv = (uv / 2.0) + 0.5;


    var fragColor: vec4<f32>;

    // Retrieve colors from appropriate locations
    fragColor.r = channel0(vec2<f32>(uv.x + 0.0003 * COLOR_FRINGING_SPREAD, uv.y + 0.0003 * COLOR_FRINGING_SPREAD)).x;
    fragColor.g = channel0(vec2<f32>(uv.x + 0.0000 * COLOR_FRINGING_SPREAD, uv.y - 0.0006 * COLOR_FRINGING_SPREAD)).y;
    fragColor.b = channel0(vec2<f32>(uv.x - 0.0006 * COLOR_FRINGING_SPREAD, uv.y + 0.0000 * COLOR_FRINGING_SPREAD)).z;
    fragColor.a = channel0(uv).a;


    // Add faint ghost images
    fragColor.r += 0.04 * GHOSTING_STRENGTH * channel0(GHOSTING_SPREAD * vec2<f32>(0.025, -0.027) + uv.xy).x;
    fragColor.g += 0.02 * GHOSTING_STRENGTH * channel0(GHOSTING_SPREAD * vec2<f32>(-0.022, -0.020) + uv.xy).y;
    fragColor.b += 0.04 * GHOSTING_STRENGTH * channel0(GHOSTING_SPREAD * vec2<f32>(-0.020, -0.018) + uv.xy).z;


    // Quadratically darken everything
    var rgb = fragColor.rgb;
    rgb = mix(rgb, rgb * rgb, DARKEN_MIX);


    // Vignette effect
    // NOTE: Clamp necessary because of curve effect
    rgb *= VIGNETTE_BRIGHTNESS * pow(clamp(uv.x * uv.y * (1.0 - uv.x) * (1.0 - uv.y), 0.0, 1.0), VIGNETTE_SPREAD);


    // Tint all colors
    rgb *= TINT;


    // NOTE: At this point, RGB values may be above 1


    // Add scan lines effect
    rgb *= mix(
        1.0,
        SCAN_LINES_VARIANCE / 2.0 * (1.0 + sin(2.0 * PI * uv.y * iResolution.y / SCAN_LINES_PERIOD)),
        SCAN_LINES_STRENGTH
    );


    // Add aperture grille
    let apertureGrilleStep = i32(8.0 * glsl_mod(fragCoord.x, APERTURE_GRILLE_PERIOD) / APERTURE_GRILLE_PERIOD);
    var apertureGrilleMask = 0.0;

    if apertureGrilleStep < 3 {
        apertureGrilleMask = 0.0;
    } else if apertureGrilleStep < 4 {
        apertureGrilleMask = glsl_mod(8.0 * fragCoord.x, APERTURE_GRILLE_PERIOD) / APERTURE_GRILLE_PERIOD;
    } else if apertureGrilleStep < 7 {
        apertureGrilleMask = 1.0;
    } else if apertureGrilleStep < 8 {
        apertureGrilleMask = glsl_mod(-8.0 * fragCoord.x, APERTURE_GRILLE_PERIOD) / APERTURE_GRILLE_PERIOD;
    }

    rgb *= 1.0 - APERTURE_GRILLE_STRENGTH * apertureGrilleMask;
    fragColor = vec4<f32>(rgb, fragColor.a);


    // Add flicker
    fragColor *= 1.0 - FLICKER_STRENGTH / 2.0 * (1.0 + sin(2.0 * PI * FLICKER_FREQUENCY * iTime));


    // Add noise
    // NOTE: Hard-coded noise distributions
    let noise = smoothstep(0.4, 0.6, gold_v2_noise(fragCoord.xy, fract(iTime * 0.001)));
    rgb = fragColor.rgb * clamp(noise + 1.0 - NOISE_CONTENT_STRENGTH, 0.0, 1.0);
    rgb = clamp(rgb + noise * NOISE_UNIFORM_STRENGTH / 100.0, vec3<f32>(0.0), vec3<f32>(1.0));
    fragColor = vec4<f32>(rgb, fragColor.a);


    // NOTE: At this point, RGB values are again within [0, 1]


    // Remove output outside of screen bounds
    // if (uv.x < 0.0 || uv.x > 1.0)
    //     fragColor.rgb *= 0.0;
    // if (uv.y < 0.0 || uv.y > 1.0)
    //     fragColor.rgb *= 0.0;


    // Add bloom: the neighborhood weighted by brightness, see `blur_source`
    fragColor += terminal_blur(vec2<f32>(uv.x, 1.0 - uv.y), BLOOM_RADIUS) * BLOOM_STRENGTH;

    fragColor = clamp(fragColor, vec4<f32>(0.0), vec4<f32>(1.0));


    // Set background opacity
    return vec4<f32>(fragColor.rgb * fragColor.a, BACKGROUND_OPACITY);
}
