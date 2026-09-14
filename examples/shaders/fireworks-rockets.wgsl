// Striped rockets rise and burst into sparks behind dark areas.
// This Ghostty shader is a lightly modified port of https://www.shadertoy.com/view/4dBGRw
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/fireworks-rockets.glsl
//
// Coordinates: the Ghostty port flips y for a top-left fragCoord origin (Ghostty
// on Metal), which is tron's own convention, so frag_coord is used as is and
// the rockets rise from the bottom.

const BLACK_BLEND_THRESHOLD: f32 = 0.4;

// GLSL mod: x - y * floor(x / y).
fn glsl_mod(x: f32, y: f32) -> f32 {
    return x - y * floor(x / y);
}

//Creates a diagonal red-and-white striped pattern.
fn barberpole(pos: vec2<f32>, rocketpos: vec2<f32>) -> vec3<f32> {
    var d = (pos.x - rocketpos.x) + (pos.y - rocketpos.y);
    var col = vec3<f32>(1.0);

    d = glsl_mod(d * 20.0, 2.0);
    if d > 1.0 {
        col = vec3<f32>(1.0, 0.0, 0.0);
    }

    return col;
}

fn rocket(pos: vec2<f32>, rocketpos: vec2<f32>) -> vec3<f32> {
    var col = vec3<f32>(0.0);
    var f = 0.0;
    let absx = abs(rocketpos.x - pos.x);
    let absy = abs(rocketpos.y - pos.y);

    // Wooden stick
    if absx < 0.01 && absy < 0.22 {
        col = vec3<f32>(1.0, 0.5, 0.5);
    }

    // Barberpole
    if absx < 0.05 && absy < 0.15 {
        col = barberpole(pos, rocketpos);
    }

    // Rocket Point
    let pointw = (rocketpos.y - pos.y - 0.25) * -0.7;
    if (rocketpos.y - pos.y) > 0.1 {
        f = smoothstep(pointw - 0.001, pointw + 0.001, absx);

        col = mix(vec3<f32>(1.0, 0.0, 0.0), col, f);
    }

    // Shadow
    f = -0.5 + smoothstep(-0.05, 0.05, (rocketpos.x - pos.x));
    col *= 0.7 + f;

    return col;
}

fn rand(val: f32, seed: f32) -> f32 {
    return cos(val * sin(val * seed) * seed);
}

fn distance2(a: vec2<f32>, b: vec2<f32>) -> f32 {
    return dot(a - b, a - b);
}

// mat2(cos(1.0), -sin(1.0), sin(1.0), cos(1.0))
const rr: mat2x2<f32> = mat2x2<f32>(0.5403023058681398, -0.8414709848078965, 0.8414709848078965, 0.5403023058681398);

fn drawParticles(pos: vec2<f32>, particolor: vec3<f32>, time: f32, cpos: vec2<f32>, gravity: f32, seed: f32, timelength: f32) -> vec3<f32> {
    var col = vec3<f32>(0.0);
    var pp = vec2<f32>(1.0, 0.0);
    for (var i = 1.0; i <= 128.0; i += 1.0) {
        let d = rand(i, seed);
        let fade = (i / 128.0) * time;
        let particpos = cpos + time * pp * d;
        pp = rr * pp;
        col = mix(particolor / fade, col, smoothstep(0.0, 0.0001, distance2(particpos, pos)));
    }
    col *= smoothstep(0.0, 1.0, (timelength - time) / timelength);

    return col;
}
fn drawFireworks(time: f32, uv: vec2<f32>, particolor: vec3<f32>, seed: f32) -> vec3<f32> {
    let timeoffset = 2.0;
    var col = vec3<f32>(0.0);
    if time <= 0.0 {
        return col;
    }
    if glsl_mod(time, 6.0) > timeoffset {
        col = drawParticles(uv, particolor, glsl_mod(time, 6.0) - timeoffset, vec2<f32>(rand(ceil(time / 6.0), seed), -0.5), 0.5, ceil(time / 6.0), seed);
    } else {
        col = rocket(uv * 3.0, vec2<f32>(3.0 * rand(ceil(time / 6.0), seed), 3.0 * (-0.5 + (timeoffset - glsl_mod(time, 6.0)))));
    }
    return col;
}

fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = frag_coord;
    let iResolution = tron.resolution;
    var uv = 1.0 - 2.0 * fragCoord / iResolution;
    uv.x *= iResolution.x / iResolution.y;
    var col = vec3<f32>(0.1, 0.1, 0.2);

    // Flip the y-axis so that the rocket is drawn from the bottom of the screen
    uv.y = -uv.y;

    col += 0.1 * uv.y;

    col += drawFireworks(tron.time, uv, vec3<f32>(1.0, 0.1, 0.1), 1.0);
    col += drawFireworks(tron.time - 2.0, uv, vec3<f32>(0.0, 1.0, 0.5), 2.0);
    col += drawFireworks(tron.time - 4.0, uv, vec3<f32>(1.0, 1.0, 0.1), 3.0);

    let termUV = fragCoord / iResolution;
    let terminalColor = terminal(termUV);

    let alpha = step(length(terminalColor.rgb), BLACK_BLEND_THRESHOLD);
    let blendedColor = mix(terminalColor.rgb * 1.0, col.rgb * 0.3, alpha);

    return vec4<f32>(blendedColor, terminalColor.a);
}
