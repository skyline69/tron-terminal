// Faint raymarched field of tumbling cubes added over the terminal.
// credits: https://github.com/rymdlego
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/cubes.glsl
//
// Coordinates: Ghostty on OpenGL hands the shader a bottom-left fragCoord origin
// (Shadertoy convention). This port flips tron's top-left coordinates to match.

const speed: f32 = 0.2;
const cube_size: f32 = 1.0;
const cube_brightness: f32 = 1.0;
const cube_rotation_speed: f32 = 2.8;
const camera_rotation_speed: f32 = 0.1;

// GLSL mod: x - y * floor(x / y).
fn glsl_mod3(x: vec3<f32>, y: f32) -> vec3<f32> {
    return x - y * floor(x / y);
}

fn rotationMatrix(m_in: vec3<f32>, a: f32) -> mat3x3<f32> {
    let m = normalize(m_in);
    let c = cos(a);
    let s = sin(a);
    return mat3x3<f32>(c + (1.0 - c) * m.x * m.x,
        (1.0 - c) * m.x * m.y - s * m.z,
        (1.0 - c) * m.x * m.z + s * m.y,
        (1.0 - c) * m.x * m.y + s * m.z,
        c + (1.0 - c) * m.y * m.y,
        (1.0 - c) * m.y * m.z - s * m.x,
        (1.0 - c) * m.x * m.z - s * m.y,
        (1.0 - c) * m.y * m.z + s * m.x,
        c + (1.0 - c) * m.z * m.z);
}

fn sphere(pos: vec3<f32>, radius: f32) -> f32 {
    return length(pos) - radius;
}

fn box(pos_in: vec3<f32>, size: vec3<f32>) -> f32 {
    let t = tron.time;
    let pos = pos_in * 0.9 * rotationMatrix(vec3<f32>(sin(t / 4.0 * speed) * 10.0, cos(t / 4.0 * speed) * 12.0, 2.7), t * 2.4 / 4.0 * speed * cube_rotation_speed);
    return length(max(abs(pos) - size, vec3<f32>(0.0)));
}

fn distfunc(pos: vec3<f32>) -> f32 {
    let t = tron.time;

    var size = 0.45 + 0.25 * abs(16.0 * sin(t * speed / 4.0));
    // float size = 2.3 + 1.8*tan((t-5.4)*6.549);
    size = cube_size * 0.16 * clamp(size, 2.0, 4.0);

    //pos = pos * rotationMatrix(vec3(0.,-3.,0.7), 3.3 * mod(t/30.0, 4.0));
    let q = glsl_mod3(pos, 5.0) - 2.5;
    let obj1 = box(q, vec3<f32>(size));
    return obj1;
}

// iChannel0 with a bottom-left origin.
fn channel0(uv: vec2<f32>) -> vec4<f32> {
    return terminal(vec2<f32>(uv.x, 1.0 - uv.y));
}

fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = vec2<f32>(frag_coord.x, tron.resolution.y - frag_coord.y);
    let iResolution = tron.resolution;
    let t = tron.time;
    var screenPos = -1.0 + 2.0 * fragCoord / iResolution;
    screenPos.x *= iResolution.x / iResolution.y;
    let cameraOrigin = vec3<f32>(t * 1.0 * speed, 0.0, 0.0);
    // vec3 cameraOrigin = vec3(t*1.8*speed, 3.0+t*0.02*speed, 0.0);
    var cameraTarget = vec3<f32>(t * 100.0, 0.0, 0.0);
    cameraTarget = vec3<f32>(t * 20.0, 0.0, 0.0) * rotationMatrix(vec3<f32>(0.0, 0.0, 1.0), t * speed * camera_rotation_speed);

    let upDirection = vec3<f32>(0.5, 1.0, 0.6);

    let cameraDir = normalize(cameraTarget - cameraOrigin);
    let cameraRight = normalize(cross(upDirection, cameraOrigin));
    let cameraUp = cross(cameraDir, cameraRight);

    let rayDir = normalize(cameraRight * screenPos.x + cameraUp * screenPos.y + cameraDir);

    const MAX_ITER: i32 = 64;
    const MAX_DIST: f32 = 48.0;
    const EPSILON: f32 = 0.001;

    var totalDist = 0.0;
    var pos = cameraOrigin;
    var dist = EPSILON;

    for (var i = 0; i < MAX_ITER; i++) {
        if dist < EPSILON || totalDist > MAX_DIST {
            break;
        }
        dist = distfunc(pos);
        totalDist += dist;
        pos += dist * rayDir;
    }

    var cubes: vec4<f32>;

    if dist < EPSILON {
        // Lighting Code
        let eps = vec2<f32>(0.0, EPSILON);
        let normal = normalize(vec3<f32>(
            distfunc(pos + eps.yxx) - distfunc(pos - eps.yxx),
            distfunc(pos + eps.xyx) - distfunc(pos - eps.xyx),
            distfunc(pos + eps.xxy) - distfunc(pos - eps.xxy)));
        let diffuse = max(0.0, dot(-rayDir, normal));
        let specular = pow(diffuse, 32.0);
        var color = vec3<f32>(diffuse + specular);
        var cubeColor = vec3<f32>(abs(screenPos), 0.5 + 0.5 * sin(t * 2.0)) * 0.8;
        cubeColor = mix(cubeColor.rgb, vec3<f32>(0.0, 0.0, 0.0), 1.0);
        color += cubeColor;
        cubes = vec4<f32>(color, 1.0) * vec4<f32>(1.0 - (totalDist / MAX_DIST));
        cubes = vec4<f32>(cubes.rgb * 0.02 * cube_brightness, 0.1);
    } else {
        cubes = vec4<f32>(0.0);
    }

    let uv = fragCoord / iResolution;
    let terminalColor = channel0(uv);
    let blendedColor = terminalColor.rgb + cubes.rgb;
    return vec4<f32>(blendedColor, terminalColor.a);
}
