// Colorful interference rings from two wandering sine sources.
// License: CC BY-NC-SA 3.0 (https://creativecommons.org/licenses/by-nc-sa/3.0/), Shadertoy's default, as the original creator stated no license.
// Based on https://www.shadertoy.com/view/ms3cWn
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/sin-interference.glsl

fn map_range(value: f32, min1: f32, max1: f32, min2: f32, max2: f32) -> f32 {
    return min2 + (value - min1) * (max2 - min2) / (max1 - min1);
}

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let res = tron.resolution;
    let d = length(uv - 0.5) * 2.0;
    let t = d * d * 25.0 - tron.time * 2.0;
    let col = 0.5 + 0.5 * cos(t / 20.0 + uv.xyx + vec3<f32>(0.0, 2.0, 4.0));

    let center = res * 0.5;
    let distCentre = distance(frag_coord, center);
    let dCSin = sin(distCentre * 0.05);

    let anim = vec2<f32>(map_range(sin(tron.time), -1.0, 1.0, 0.0, res.x), map_range(sin(tron.time * 1.25), -1.0, 1.0, 0.0, res.y));
    let distMouse = distance(frag_coord, anim);
    let dMSin = sin(distMouse * 0.05);

    var greycol = (((dMSin * dCSin) + 1.0) * 0.5);
    greycol = greycol * map_range(d, 0.0, 1.4142135623730951, 0.5, 0.0);

    let terminalColor = terminal(uv);
    let blendedColor = mix(terminalColor.rgb, vec3<f32>(greycol * col.x, greycol * col.y, greycol * col.z), 0.25);

    return vec4<f32>(blendedColor, terminalColor.a);
}
