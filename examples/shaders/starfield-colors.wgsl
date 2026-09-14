// Colorful stars flying toward you behind dark parts of the terminal.
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/starfield-colors.glsl

// transparent background
const transparent: bool = false;

// terminal contents luminance threshold to be considered background (0.0 to 1.0)
const threshold: f32 = 0.15;

// divisions of grid
const repeats: f32 = 30.0;

// number of layers
const layers: f32 = 21.0;

// star colours
const blue: vec3<f32> = vec3<f32>(51.0, 64.0, 195.0) / 255.0;
const cyan: vec3<f32> = vec3<f32>(117.0, 250.0, 254.0) / 255.0;
const white: vec3<f32> = vec3<f32>(255.0, 255.0, 255.0) / 255.0;
const yellow: vec3<f32> = vec3<f32>(251.0, 245.0, 44.0) / 255.0;
const red: vec3<f32> = vec3<f32>(247.0, 2.0, 20.0) / 255.0;

fn luminance(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// spectrum function
fn spectrum(pos_in: vec2<f32>) -> vec3<f32> {
    var pos = pos_in;
    pos.x *= 4.0;
    var outCol = vec3<f32>(0.0);
    if pos.x > 0.0 {
        outCol = mix(blue, cyan, vec3<f32>(fract(pos.x)));
    }
    if pos.x > 1.0 {
        outCol = mix(cyan, white, vec3<f32>(fract(pos.x)));
    }
    if pos.x > 2.0 {
        outCol = mix(white, yellow, vec3<f32>(fract(pos.x)));
    }
    if pos.x > 3.0 {
        outCol = mix(yellow, red, vec3<f32>(fract(pos.x)));
    }

    return 1.0 - (pos.y * (1.0 - outCol));
}

fn N21(p_in: vec2<f32>) -> f32 {
    var p = fract(p_in * vec2<f32>(233.34, 851.73));
    p += dot(p, p + 23.45);
    return fract(p.x * p.y);
}

fn N22(p: vec2<f32>) -> vec2<f32> {
    let n = N21(p);
    return vec2<f32>(n, N21(p + n));
}

fn scale(_scale: vec2<f32>) -> mat2x2<f32> {
    return mat2x2<f32>(_scale.x, 0.0,
        0.0, _scale.y);
}

fn stars(uv_in: vec2<f32>, offset: f32) -> vec3<f32> {
    let timeScale = -(tron.time + offset) / layers;
    let trans = fract(timeScale);
    let newRnd = floor(timeScale);
    var col = vec3<f32>(0.0);
    var uv = uv_in;

    // Translate uv then scale for center
    uv -= vec2<f32>(0.5);
    uv = scale(vec2<f32>(trans)) * uv;
    uv += vec2<f32>(0.5);

    // Create square aspect ratio
    uv.x *= tron.resolution.x / tron.resolution.y;

    // Create boxes
    uv *= repeats;

    // Get position
    let ipos = floor(uv);

    // Return uv as 0 to 1
    uv = fract(uv);

    // Calculate random xy and size
    let rndXY = N22(newRnd + ipos * (offset + 1.0)) * 0.9 + 0.05;
    let rndSize = N21(ipos) * 100.0 + 200.0;

    let j = (rndXY - uv) * rndSize;
    let sparkle = 1.0 / dot(j, j);

    // Set stars to be pure white
    col += spectrum(fract(rndXY * newRnd * ipos)) * vec3<f32>(sparkle);

    col *= smoothstep(1.0, 0.8, trans);
    return col; // Return pure white stars only
}

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    var col = vec3<f32>(0.0);

    for (var i: f32 = 0.0; i < layers; i += 1.0) {
        col += stars(uv, i);
    }

    // Sample the terminal screen texture including alpha channel
    let terminalColor = terminal(uv);

    if transparent {
        col += terminalColor.rgb;
    }

    // Make a mask that is 1.0 where the terminal content is not black
    let mask = 1.0 - step(threshold, luminance(terminalColor.rgb));
    let blendedColor = mix(terminalColor.rgb, col, vec3<f32>(mask));

    // Apply terminal's alpha to control overall opacity
    return vec4<f32>(blendedColor, terminalColor.a);
}
