// Glowing sparks and smoke rising behind dark parts of the terminal.
// adapted by Alex Sherwin for Ghstty from https://www.shadertoy.com/view/wl2Gzc

//Shader License: CC BY 3.0
//Author: Jan Mróz (jaszunio15)

// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/sparks-from-fire.glsl

const SMOKE_INTENSITY_MULTIPLIER: f32 = 0.9;
const PARTICLES_ALPHA_MOD: f32 = 0.9;
const SMOKE_ALPHA_MOD: f32 = 0.5;
const LAYERS_COUNT: i32 = 8;

const BLACK_BLEND_THRESHOLD: f32 = 0.4;

const VEC3_1: vec3<f32> = vec3<f32>(1.0);

const PI: f32 = 3.1415927;
const TWO_PI: f32 = 6.283185;

const ANIMATION_SPEED: f32 = 1.0;
const MOVEMENT_SPEED: f32 = 0.33;
const MOVEMENT_DIRECTION: vec2<f32> = vec2<f32>(0.7, 1.0);

const PARTICLE_SIZE: f32 = 0.0025;

const PARTICLE_SCALE: vec2<f32> = vec2<f32>(0.5, 1.6);
const PARTICLE_SCALE_VAR: vec2<f32> = vec2<f32>(0.25, 0.2);

const PARTICLE_BLOOM_SCALE: vec2<f32> = vec2<f32>(0.5, 0.8);
const PARTICLE_BLOOM_SCALE_VAR: vec2<f32> = vec2<f32>(0.3, 0.1);

const SPARK_COLOR: vec3<f32> = vec3<f32>(1.0, 0.4, 0.05) * 1.5;
const BLOOM_COLOR: vec3<f32> = vec3<f32>(1.0, 0.4, 0.05) * 0.8;
const SMOKE_COLOR: vec3<f32> = vec3<f32>(1.0, 0.43, 0.1) * 0.8;

const SIZE_MOD: f32 = 1.05;


fn hash1_2(x: vec2<f32>) -> f32 {
    return fract(sin(dot(x, vec2<f32>(52.127, 61.2871))) * 521.582);
}

fn hash2_2(x: vec2<f32>) -> vec2<f32> {
    return fract(sin(x * mat2x2<f32>(20.52, 24.1994, 70.291, 80.171)) * 492.194);
}

//Simple interpolated noise
fn noise2_2(uv: vec2<f32>) -> vec2<f32> {
    //vec2 f = fract(uv);
    let f = smoothstep(vec2<f32>(0.0), vec2<f32>(1.0), fract(uv));

    let uv00 = floor(uv);
    let uv01 = uv00 + vec2<f32>(0.0, 1.0);
    let uv10 = uv00 + vec2<f32>(1.0, 0.0);
    let uv11 = uv00 + 1.0;
    let v00 = hash2_2(uv00);
    let v01 = hash2_2(uv01);
    let v10 = hash2_2(uv10);
    let v11 = hash2_2(uv11);

    let v0 = mix(v00, v01, vec2<f32>(f.y));
    let v1 = mix(v10, v11, vec2<f32>(f.y));
    let v = mix(v0, v1, vec2<f32>(f.x));

    return v;
}

//Simple interpolated noise
fn noise1_2(uv: vec2<f32>) -> f32 {
    // vec2 f = fract(uv);
    let f = smoothstep(vec2<f32>(0.0), vec2<f32>(1.0), fract(uv));

    let uv00 = floor(uv);
    let uv01 = uv00 + vec2<f32>(0.0, 1.0);
    let uv10 = uv00 + vec2<f32>(1.0, 0.0);
    let uv11 = uv00 + 1.0;

    let v00 = hash1_2(uv00);
    let v01 = hash1_2(uv01);
    let v10 = hash1_2(uv10);
    let v11 = hash1_2(uv11);

    let v0 = mix(v00, v01, f.y);
    let v1 = mix(v10, v11, f.y);
    let v = mix(v0, v1, f.x);

    return v;
}


fn layeredNoise1_2(uv: vec2<f32>, sizeMod: f32, alphaMod: f32, layers: i32, animation: f32) -> f32 {
    var noise = 0.0;
    var alpha = 1.0;
    var size = 1.0;
    var offset = vec2<f32>(0.0);
    for (var i: i32 = 0; i < layers; i++) {
        offset += hash2_2(vec2<f32>(alpha, size)) * 10.0;

        //Adding noise with movement
        noise += noise1_2(uv * size + tron.time * animation * 8.0 * MOVEMENT_DIRECTION * MOVEMENT_SPEED + offset) * alpha;
        alpha *= alphaMod;
        size *= sizeMod;
    }

    noise *= (1.0 - alphaMod) / (1.0 - pow(alphaMod, f32(layers)));
    return noise;
}

//Rotates point around 0,0
fn rotate(point: vec2<f32>, deg: f32) -> vec2<f32> {
    let s = sin(deg);
    let c = cos(deg);
    return mat2x2<f32>(s, c, -c, s) * point;
}

//Cell center from point on the grid
fn voronoiPointFromRoot(root: vec2<f32>, deg: f32) -> vec2<f32> {
    var point = hash2_2(root) - 0.5;
    let s = sin(deg);
    let c = cos(deg);
    point = mat2x2<f32>(s, c, -c, s) * point * 0.66;
    point += root + 0.5;
    return point;
}

//Voronoi cell point rotation degrees
fn degFromRootUV(uv: vec2<f32>) -> f32 {
    return tron.time * ANIMATION_SPEED * (hash1_2(uv) - 0.5) * 2.0;
}

fn randomAround2_2(point: vec2<f32>, range: vec2<f32>, uv: vec2<f32>) -> vec2<f32> {
    return point + (hash2_2(uv) - 0.5) * range;
}


fn fireParticles(uv: vec2<f32>, originalUV: vec2<f32>) -> vec3<f32> {
    var particles = vec3<f32>(0.0);
    let rootUV = floor(uv);
    let deg = degFromRootUV(rootUV);
    let pointUV = voronoiPointFromRoot(rootUV, deg);
    var dist = 2.0;
    var distBloom = 0.0;

    //UV manipulation for the faster particle movement
    var tempUV = uv + (noise2_2(uv * 2.0) - 0.5) * 0.1;
    tempUV += -(noise2_2(uv * 3.0 + tron.time) - 0.5) * 0.07;

    //Sparks sdf
    dist = length(rotate(tempUV - pointUV, 0.7) * randomAround2_2(PARTICLE_SCALE, PARTICLE_SCALE_VAR, rootUV));

    //Bloom sdf
    distBloom = length(rotate(tempUV - pointUV, 0.7) * randomAround2_2(PARTICLE_BLOOM_SCALE, PARTICLE_BLOOM_SCALE_VAR, rootUV));

    //Add sparks
    particles += (1.0 - smoothstep(PARTICLE_SIZE * 0.6, PARTICLE_SIZE * 3.0, dist)) * SPARK_COLOR;

    //Add bloom
    particles += pow((1.0 - smoothstep(0.0, PARTICLE_SIZE * 6.0, distBloom)) * 1.0, 3.0) * BLOOM_COLOR;

    //Upper disappear curve randomization
    var border = (hash1_2(rootUV) - 0.5) * 2.0;
    let disappear = 1.0 - smoothstep(border, border + 0.5, originalUV.y);

    //Lower appear curve randomization
    border = (hash1_2(rootUV + 0.214) - 1.8) * 0.7;
    let appear = smoothstep(border, border + 0.4, originalUV.y);

    return particles * disappear * appear;
}


//Layering particles to imitate 3D view
fn layeredParticles(uv: vec2<f32>, sizeMod: f32, alphaMod: f32, layers: i32, smoke: f32) -> vec3<f32> {
    var particles = vec3<f32>(0.0);
    var size = 1.0;
    // float alpha = 1.0;
    var alpha = 1.0;
    var offset = vec2<f32>(0.0);
    var noiseOffset: vec2<f32>;
    var bokehUV: vec2<f32>;

    for (var i: i32 = 0; i < layers; i++) {
        //Particle noise movement
        noiseOffset = (noise2_2(uv * size * 2.0 + 0.5) - 0.5) * 0.15;

        //UV with applied movement
        bokehUV = (uv * size + tron.time * MOVEMENT_DIRECTION * MOVEMENT_SPEED) + offset + noiseOffset;

        //Adding particles								if there is more smoke, remove smaller particles
        particles += fireParticles(bokehUV, uv) * alpha * (1.0 - smoothstep(0.0, 1.0, smoke) * (f32(i) / f32(layers)));

        //Moving uv origin to avoid generating the same particles
        offset += hash2_2(vec2<f32>(alpha, alpha)) * 10.0;

        alpha *= alphaMod;
        size *= sizeMod;
    }

    return particles;
}

fn shade(term_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let res = tron.resolution;
    var uv = (2.0 * frag_coord - res) / res.x;

    // float vignette = 1.1 - smoothstep(0.4, 1.4, length(uv + vec2(0.0, 0.3)));
    let vignette = 1.3 - smoothstep(0.4, 1.4, length(uv + vec2<f32>(0.0, 0.3)));

    uv *= 2.5;

    var smokeIntensity = layeredNoise1_2(uv * 10.0 + tron.time * 4.0 * MOVEMENT_DIRECTION * MOVEMENT_SPEED, 1.7, 0.7, 6, 0.2);
    smokeIntensity *= pow(smoothstep(-1.0, 1.6, uv.y), 2.0);
    var smoke = smokeIntensity * SMOKE_COLOR * vignette * SMOKE_INTENSITY_MULTIPLIER * SMOKE_ALPHA_MOD;

    //Cutting holes in smoke
    smoke *= pow(layeredNoise1_2(uv * 4.0 + tron.time * 0.5 * MOVEMENT_DIRECTION * MOVEMENT_SPEED, 1.8, 0.5, 3, 0.2), 2.0) * 1.5;

    let particles = layeredParticles(uv, SIZE_MOD, PARTICLES_ALPHA_MOD, LAYERS_COUNT, smokeIntensity);

    var col = particles + smoke + SMOKE_COLOR * 0.02;
    col *= vignette;

    col = smoothstep(vec3<f32>(-0.08), vec3<f32>(1.0), col);

    let terminalColor = terminal(term_uv);

    let alpha = step(length(terminalColor.rgb), BLACK_BLEND_THRESHOLD);
    let blendedColor = mix(terminalColor.rgb, col, vec3<f32>(alpha));

    return vec4<f32>(blendedColor, terminalColor.a);
}
