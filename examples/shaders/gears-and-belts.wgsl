// Dim scrolling tiles of turning gears and conveyor belts.
// License: CC BY-NC-SA 3.0 (https://creativecommons.org/licenses/by-nc-sa/3.0/), Shadertoy's default, as the original creator stated no license.
// sligltly modified version of https://www.shadertoy.com/view/DsVSDV
// The only changes are done in the mainImage function
// Ive added comments on what to modify
// works really well with most colorschemes
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/gears-and-belts.glsl
//
// Coordinates: Ghostty on OpenGL hands the shader a bottom-left fragCoord origin
// (Shadertoy convention). This port flips tron's top-left coordinates to match,
// so the pattern moves down as the comment in shade() says.

// GLSL mod: x - y * floor(x / y).
fn glsl_mod(x: f32, y: f32) -> f32 {
    return x - y * floor(x / y);
}

// smoothstep that also accepts edge0 > edge1, like the GLSL original relies on.
fn smooth_step(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn Rot(a: f32) -> mat2x2<f32> {
    return mat2x2<f32>(cos(a), -sin(a), sin(a), cos(a));
}
fn antialiasing(n: f32) -> f32 {
    return n / min(tron.resolution.y, tron.resolution.x);
}
fn S(d: f32, b: f32) -> f32 {
    return smooth_step(antialiasing(3.0), b, d);
}
fn B(p: vec2<f32>, s: vec2<f32>) -> f32 {
    return max(abs(p).x - s.x, abs(p).y - s.y);
}
const deg45: f32 = 0.707;
fn R45(p: vec2<f32>) -> vec2<f32> {
    return (p + vec2<f32>(p.y, -p.x)) * deg45;
}
fn Tri(p: vec2<f32>, s: vec2<f32>) -> f32 {
    return max(R45(p).x, max(R45(p).y, B(p, s)));
}
fn DF(a: vec2<f32>, b: f32) -> vec2<f32> {
    return length(a) * cos(glsl_mod(atan2(a.y, a.x) + 6.28 / (b * 8.0), 6.28 / ((b * 8.0) * 0.5)) + (b - 1.0) * 6.28 / (b * 8.0) + vec2<f32>(0.0, 11.0));
}

fn random(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p.xy, vec2<f32>(12.9898, 78.233))) * 43758.5453123);
}

fn innerGear(p_in: vec2<f32>, dir: f32) -> f32 {
    var p = p_in * Rot(radians(-tron.time * 45.0 + 45.0) * dir);
    let prevP = p;

    //p*=Rot(radians(iTime*45.+20.));
    p = DF(p, 7.0);
    p -= vec2<f32>(0.24);
    p *= Rot(deg45);
    var d = B(p, vec2<f32>(0.01, 0.06));
    p = prevP;
    var d2 = abs(length(p) - 0.42) - 0.02;
    d = min(d, d2);
    d2 = abs(length(p) - 0.578) - 0.02;
    d = min(d, d2);
    d2 = abs(length(p) - 0.499) - 0.005;
    d = min(d, d2);

    p = DF(p, 7.0);
    p -= vec2<f32>(0.43);
    p *= Rot(deg45);
    d2 = B(p, vec2<f32>(0.01, 0.04));
    d = min(d, d2);

    return d;
}

fn pattern1(p_in: vec2<f32>, col_in: vec3<f32>, dir: f32) -> vec3<f32> {
    var p = p_in;
    var col = col_in;
    let prevP = p;
    let size = 0.499;
    let thick = 0.15;

    p += vec2<f32>(size);
    var d = abs(length(p) - size) - thick;
    d = max(d, innerGear(p, dir));
    col = mix(col, vec3<f32>(1.0), S(d, 0.0));

    p = prevP;
    p -= vec2<f32>(size);
    d = abs(length(p) - size) - thick;
    d = max(d, innerGear(p, dir));
    col = mix(col, vec3<f32>(1.0), S(d, 0.0));

    return col;
}

fn pattern2(p_in: vec2<f32>, col_in: vec3<f32>, dir: f32) -> vec3<f32> {
    var p = p_in;
    var col = col_in;
    let iTime = tron.time;

    let prevP = p;
    let size = 0.33;
    let thick = 0.15;
    let thift = 0.0;
    let speed = 0.3;

    p -= vec2<f32>(size, 0.0);
    var d = B(p, vec2<f32>(size, thick));

    p.x += thift;
    p.x -= iTime * speed * dir;
    p.x = glsl_mod(p.x, 0.08) - 0.04;
    d = max(d, B(p, vec2<f32>(0.011, thick)));
    p = prevP;
    d = max(-(abs(p.y) - 0.1), d);
    //d = min(B(p,vec2(1.,0.1)),d);
    p.y = abs(p.y) - 0.079;
    d = min(B(p, vec2<f32>(1.0, 0.02)), d);

    p = prevP;
    p -= vec2<f32>(0.0, size);
    var d2 = B(p, vec2<f32>(thick, size));

    p.y += thift;
    p.y += iTime * speed * dir;
    p.y = glsl_mod(p.y, 0.08) - 0.04;
    d2 = max(d2, B(p, vec2<f32>(thick, 0.011)));

    p = prevP;
    d2 = max(-(abs(p.x) - 0.1), d2);
    d2 = min(B(p, vec2<f32>(0.005, 1.0)), d2);
    p.x = abs(p.x) - 0.079;
    d2 = min(B(p, vec2<f32>(0.02, 1.0)), d2);

    d = min(d, d2);

    p = prevP;
    p += vec2<f32>(0.0, size);
    d2 = B(p, vec2<f32>(thick, size));

    p.y += thift;
    p.y -= iTime * speed * dir;
    p.y = glsl_mod(p.y, 0.08) - 0.04;
    d2 = max(d2, B(p, vec2<f32>(thick, 0.011)));

    p = prevP;
    d2 = max(-(abs(p.x) - 0.1), d2);
    d2 = min(B(p, vec2<f32>(0.005, 1.0)), d2);
    p.x = abs(p.x) - 0.079;
    d2 = min(B(p, vec2<f32>(0.02, 1.0)), d2);

    d = min(d, d2);

    p = prevP;
    p += vec2<f32>(size, 0.0);
    d2 = B(p, vec2<f32>(size, thick));

    p.x += thift;
    p.x += iTime * speed * dir;
    p.x = glsl_mod(p.x, 0.08) - 0.04;
    d2 = max(d2, B(p, vec2<f32>(0.011, thick)));
    d = min(d, d2);
    p = prevP;
    d = max(-(abs(p.y) - 0.1), d);
    d = min(B(p, vec2<f32>(1.0, 0.005)), d);
    p.y = abs(p.y) - 0.079;
    d = min(B(p, vec2<f32>(1.0, 0.02)), d);

    p = prevP;
    d2 = abs(B(p, vec2<f32>(size * 0.3))) - 0.05;
    d = min(d, d2);

    col = mix(col, vec3<f32>(1.0), S(d, 0.0));

    d = B(p, vec2<f32>(0.08));
    col = mix(col, vec3<f32>(0.0), S(d, 0.0));

    p *= Rot(radians(60.0 * iTime * dir));
    d = B(p, vec2<f32>(0.03));
    col = mix(col, vec3<f32>(1.0), S(d, 0.0));

    return col;
}

fn drawBelt(p_in: vec2<f32>, col_in: vec3<f32>, size: f32) -> vec3<f32> {
    var col = col_in;
    let p = p_in * size;
    let id = floor(p);
    var gr = fract(p) - 0.5;
    let dir = glsl_mod(id.x + id.y, 2.0) * 2.0 - 1.0;
    let n = random(id);

    if n < 0.5 {
        if n < 0.25 {
            gr.x *= -1.0;
        }
        col = pattern1(gr, col, dir);
    } else {
        if n > 0.75 {
            gr.x *= -1.0;
        }
        col = pattern2(gr, col, dir);
    }

    return col;
}

fn gear(p_in: vec2<f32>, col_in: vec3<f32>, dir: f32) -> vec3<f32> {
    var p = p_in;
    var col = col_in;
    let iTime = tron.time;
    let prevP = p;

    p *= Rot(radians(iTime * 45.0 + 13.0) * -dir);
    p = DF(p, 7.0);
    p -= vec2<f32>(0.23);
    p *= Rot(deg45);
    var d = B(p, vec2<f32>(0.01, 0.04));
    p = prevP;
    var d2 = abs(length(p) - 0.29) - 0.02;
    d = min(d, d2);
    col = mix(col, vec3<f32>(1.0), S(d, 0.0));

    p *= Rot(radians(iTime * 30.0 - 30.0) * dir);
    p = DF(p, 6.0);
    p -= vec2<f32>(0.14);
    p *= Rot(radians(45.0));
    d = B(p, vec2<f32>(0.01, 0.03));
    p = prevP;
    d2 = abs(length(p) - 0.1) - 0.02;
    p *= Rot(radians(iTime * 25.0 + 30.0) * -dir);
    d2 = max(-(abs(p.x) - 0.05), d2);
    d = min(d, d2);
    col = mix(col, vec3<f32>(1.0), S(d, 0.0));

    return col;
}

fn item0(p_in: vec2<f32>, col_in: vec3<f32>, dir: f32) -> vec3<f32> {
    var p = p_in;
    var col = col_in;
    p.x *= dir;
    p *= Rot(radians(tron.time * 30.0 + 30.0));
    var d = abs(length(p) - 0.2) - 0.05;
    col = mix(col, vec3<f32>(0.3), S(d, 0.0));

    d = abs(length(p) - 0.2) - 0.05;
    d = max(-p.x, d);
    let a = clamp(atan2(p.x, p.y) * 0.5, 0.3, 1.0);

    col = mix(col, vec3<f32>(a), S(d, 0.0));

    return col;
}

fn item1(p_in: vec2<f32>, col_in: vec3<f32>, dir: f32) -> vec3<f32> {
    var p = p_in;
    var col = col_in;
    let iTime = tron.time;
    p.x *= dir;
    let prevP = p;
    p *= Rot(radians(iTime * 30.0 + 30.0));
    var d = abs(length(p) - 0.25) - 0.04;
    d = abs(max((abs(p.y) - 0.15), d)) - 0.005;
    var d2 = abs(length(p) - 0.25) - 0.01;
    d2 = max((abs(p.y) - 0.12), d2);
    d = min(d, d2);

    d2 = abs(length(p) - 0.27) - 0.01;
    d2 = max(-(abs(p.y) - 0.22), d2);
    d = min(d, d2);
    d2 = B(p, vec2<f32>(0.01, 0.32));
    d2 = max(-(abs(p.y) - 0.22), d2);
    d = min(d, d2);

    p = prevP;
    p *= Rot(radians(iTime * -20.0 + 30.0));
    p = DF(p, 2.0);
    p -= vec2<f32>(0.105);
    p *= Rot(radians(45.0));
    d2 = B(p, vec2<f32>(0.03, 0.01));
    d = min(d, d2);

    p = prevP;
    d2 = abs(length(p) - 0.09) - 0.005;
    d2 = max(-(abs(p.x) - 0.03), d2);
    d2 = max(-(abs(p.y) - 0.03), d2);
    d = min(d, d2);

    col = mix(col, vec3<f32>(0.6), S(d, 0.0));

    return col;
}

fn item2(p_in: vec2<f32>, col_in: vec3<f32>, dir: f32) -> vec3<f32> {
    var p = p_in;
    var col = col_in;
    p.x *= dir;
    p *= Rot(radians(tron.time * 50.0 - 10.0));
    let prevP = p;
    var d = abs(length(p) - 0.15) - 0.005;
    var d2 = abs(length(p) - 0.2) - 0.01;
    d2 = max((abs(p.y) - 0.15), d2);
    d = min(d, d2);

    p = DF(p, 1.0);
    p -= vec2<f32>(0.13);
    p *= Rot(radians(45.0));
    d2 = B(p, vec2<f32>(0.008, 0.1));
    d = min(d, d2);

    p = prevP;
    p = DF(p, 4.0);
    p -= vec2<f32>(0.18);
    p *= Rot(radians(45.0));
    d2 = B(p, vec2<f32>(0.005, 0.02));
    d = min(d, d2);

    col = mix(col, vec3<f32>(0.6), S(d, 0.0));

    return col;
}

fn needle(p_in: vec2<f32>) -> f32 {
    var p = p_in;
    p.y -= 0.05;
    p *= 1.5;
    let prevP = p;
    p.y -= 0.3;
    p.x *= 6.0;
    var d = Tri(p, vec2<f32>(0.3));
    p = prevP;
    p.y += 0.1;
    p.x *= 2.0;
    p.y *= -1.0;
    let d2 = Tri(p, vec2<f32>(0.1));
    d = min(d, d2);
    return d;
}

fn item3(p_in: vec2<f32>, col_in: vec3<f32>, dir: f32) -> vec3<f32> {
    var col = col_in;
    var p = p_in * Rot(radians(sin(tron.time * dir) * 120.0));
    let prevP = p;

    p.y = abs(p.y) - 0.05;
    var d = needle(p);
    p = prevP;
    var d2 = abs(length(p) - 0.1) - 0.003;
    d2 = max(-(abs(p.x) - 0.05), d2);
    d = min(d, d2);
    d2 = abs(length(p) - 0.2) - 0.005;
    d2 = max(-(abs(p.x) - 0.08), d2);
    d = min(d, d2);

    p = DF(p, 4.0);
    p -= vec2<f32>(0.18);
    d2 = length(p) - 0.01;
    p = prevP;
    d2 = max(-(abs(p.x) - 0.03), d2);
    d = min(d, d2);

    col = mix(col, vec3<f32>(0.6), S(d, 0.0));

    return col;
}

fn drawGearsAndItems(p_in: vec2<f32>, col_in: vec3<f32>, size: f32) -> vec3<f32> {
    var col = col_in;
    var p = p_in * size;
    p += vec2<f32>(0.5);

    let id = floor(p);
    let gr = fract(p) - 0.5;

    let n = random(id);
    let dir = glsl_mod(id.x + id.y, 2.0) * 2.0 - 1.0;
    if n < 0.3 {
        col = gear(gr, col, dir);
    } else if n >= 0.3 && n < 0.5 {
        col = item0(gr, col, dir);
    } else if n >= 0.5 && n < 0.7 {
        col = item1(gr, col, dir);
    } else if n >= 0.7 && n < 0.8 {
        col = item2(gr, col, dir);
    } else if n >= 0.8 {
        col = item3(gr, col, dir);
    }

    return col;
}

// iChannel0 with a bottom-left origin.
fn channel0(uv: vec2<f32>) -> vec4<f32> {
    return terminal(vec2<f32>(uv.x, 1.0 - uv.y));
}

fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = vec2<f32>(frag_coord.x, tron.resolution.y - frag_coord.y);
    let iResolution = tron.resolution;
    var p = (fragCoord - 0.5 * iResolution) / iResolution.y;
    // set speed of downwards motion
    p.y += tron.time * 0.02;

    let size = 4.0;
    var col = vec3<f32>(0.0);

    // Modify the colors to be darker by multiplying with a small factor
    let darkFactor = vec3<f32>(0.5); // This makes everything 50% as bright

    // Get the original colors but make them darker
    col = drawBelt(p, col, size) * darkFactor;
    col = drawGearsAndItems(p, col, size) * darkFactor;

    // Additional option: you can add a color tint to make it less stark white
    let tint = vec3<f32>(0.1, 0.12, 0.15); // Slight blue-ish dark tint
    col = col * tint;

    let uv = fragCoord / iResolution;
    let terminalColor = channel0(uv);

    // Blend with reduced opacity for the shader elements
    let blendedColor = terminalColor.rgb + col.rgb * 0.7; // Reduced blend factor

    return vec4<f32>(blendedColor, terminalColor.a);
}
