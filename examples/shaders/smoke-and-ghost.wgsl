// Smoke rising from black pixels, with ghost faces in thick smoke.
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/smoke-and-ghost.glsl
//
// Port notes: the effect only triggers on pure black pixels (TARGET_COLOR),
// so it needs a black terminal background, as in Ghostty. Ghostty ignores the
// output alpha on opaque windows; tron composites with premultiplied alpha,
// so the computed alpha is never allowed to drop below the terminal's alpha.

// Settings for detection
const TARGET_COLOR: vec3<f32> = vec3<f32>(0.0, 0.0, 0.0);      // RGB target pixels to transform
const REPLACE_COLOR: vec3<f32> = vec3<f32>(0.0, 0.0, 0.0);    // Color to replace target pixels
const COLOR_TOLERANCE: f32 = 0.001;                  // Color matching tolerance

// Smoke effect settings
const SMOKE_COLOR: vec3<f32> = vec3<f32>(1.0, 1.0, 1.0);       // Base color of smoke
const SMOKE_RADIUS: f32 = 0.011;                 // How far the smoke spreads
const SMOKE_SPEED: f32 = 0.5;                        // Speed of smoke movement
const SMOKE_SCALE: f32 = 25.0;                       // Scale of smoke detail
const SMOKE_INTENSITY: f32 = 0.2;                    // Intensity of the smoke effect
const SMOKE_RISE_HEIGHT: f32 = 0.14;                 // How high the smoke rises
const ALPHA_MAX: f32 = 0.5;                          // Maximum opacity for smoke
const VERTICAL_BIAS: f32 = 1.0;

// Ghost face settings
const FACE_COUNT: i32 = 1;                           // Number of ghost faces
const FACE_SCALE: vec2<f32> = vec2<f32>(0.03, 0.05);            // Size of faces, can be wider/elongated
const FACE_DURATION: f32 = 1.2;                      // How long faces last, can be wider/elongated
const FACE_TRANSITION: f32 = 1.5;                    // Face fade in/out duration
const FACE_COLOR: vec3<f32> = vec3<f32>(0.0, 0.0, 0.0);
const GHOST_BG_COLOR: vec3<f32> = vec3<f32>(1.0, 1.0, 1.0);
const GHOST_BG_SCALE: vec2<f32> = vec2<f32>(0.03, 0.06);

// GLSL mod(): the result has the sign of y.
fn glsl_mod(x: f32, y: f32) -> f32 {
    return x - y * floor(x / y);
}

fn random(st: vec2<f32>) -> f32 {
    return fract(sin(dot(st.xy, vec2<f32>(12.9898, 78.233))) * 43758.5453123);
}

fn random1(n: f32) -> f32 {
    return fract(sin(n) * 43758.5453123);
}

fn random2(n: f32) -> vec2<f32> {
    return vec2<f32>(
        random1(n),
        random1(n + 1234.5678)
    );
}

fn noise(st: vec2<f32>) -> f32 {
    let i = floor(st);
    let f = fract(st);

    let a = random(i);
    let b = random(i + vec2<f32>(1.0, 0.0));
    let c = random(i + vec2<f32>(0.0, 1.0));
    let d = random(i + vec2<f32>(1.0, 1.0));

    let u = f * f * (3.0 - 2.0 * f);
    return mix(a, b, u.x) + (c - a) * u.y * (1.0 - u.x) + (d - b) * u.x * u.y;
}

// Modified elongated ellipse for more cartoon-like shapes
fn cartoonEllipse(uv: vec2<f32>, center: vec2<f32>, scale: vec2<f32>) -> f32 {
    let d = (uv - center) / scale;
    let len = length(d);
    // Add cartoon-like falloff
    return smoothstep(1.0, 0.8, len);
}

// Function to create ghost background shape
fn ghostBackground(uv: vec2<f32>, center: vec2<f32>) -> f32 {
    let d = (uv - center) / GHOST_BG_SCALE;
    let baseShape = length(d * vec2<f32>(1.0, 0.8)); // Slightly oval

    // Add wavy bottom
    let wave = sin(d.x * 6.28 + tron.time) * 0.2;
    let bottomWave = smoothstep(0.0, -0.5, d.y + wave);

    return smoothstep(1.0, 0.8, baseShape) + bottomWave;
}

fn ghostFace(uv: vec2<f32>, center: vec2<f32>, time: f32, seed: f32) -> f32 {
    let faceUV = (uv - center) / FACE_SCALE;

    let eyeSize = 0.25 + random1(seed) * 0.05;
    let eyeSpacing = 0.35;
    let leftEyePos = vec2<f32>(-eyeSpacing, 0.2);
    let rightEyePos = vec2<f32>(eyeSpacing, 0.2);

    let leftEye = cartoonEllipse(faceUV, leftEyePos, vec2<f32>(eyeSize));
    let rightEye = cartoonEllipse(faceUV, rightEyePos, vec2<f32>(eyeSize));

    // Add simple eye highlights
    let leftHighlight = cartoonEllipse(faceUV, leftEyePos + vec2<f32>(0.1, 0.1), vec2<f32>(eyeSize * 0.3));
    let rightHighlight = cartoonEllipse(faceUV, rightEyePos + vec2<f32>(0.1, 0.1), vec2<f32>(eyeSize * 0.3));

    let mouthUV = faceUV - vec2<f32>(0.0, -0.9);
    let mouthWidth = 0.5 + random1(seed + 3.0) * 0.1;
    let mouthHeight = 0.8 + random1(seed + 7.0) * 0.1;

    let mouth = cartoonEllipse(mouthUV, vec2<f32>(0.0), vec2<f32>(mouthWidth, mouthHeight));

    // Combine features
    var face = max(max(leftEye, rightEye), mouth);
    face = max(face, max(leftHighlight, rightHighlight));

    // Add border falloff
    face *= smoothstep(1.2, 0.8, length(faceUV));

    return face;
}

fn calculateSmoke(uv: vec2<f32>, sourcePos: vec2<f32>) -> f32 {
    let verticalDisp = (uv.y - sourcePos.y) * VERTICAL_BIAS;
    var smokeUV = uv * SMOKE_SCALE;
    smokeUV.y -= tron.time * SMOKE_SPEED * (1.0 + verticalDisp);
    smokeUV.x += sin(tron.time * 0.5 + uv.y * 4.0) * 0.1;

    var n = noise(smokeUV) * 0.5 + 0.5;
    n += noise(smokeUV * 2.0 + tron.time * 0.1) * 0.25;

    let verticalFalloff = 1.0 - smoothstep(0.0, SMOKE_RISE_HEIGHT, verticalDisp);
    return n * verticalFalloff;
}

fn isTargetPixel(uv: vec2<f32>) -> f32 {
    let color = terminal(uv);
    return f32(all(abs(color.rgb - TARGET_COLOR) < vec3<f32>(COLOR_TOLERANCE)));
}

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let originalColor = terminal(uv);

    // Calculate smoke effect
    var smokeAccum = 0.0;
    var targetInfluence = 0.0;

    let stepSize = SMOKE_RADIUS / 4.0;
    for (var x: f32 = -SMOKE_RADIUS; x <= SMOKE_RADIUS; x += stepSize) {
        for (var y: f32 = -SMOKE_RADIUS; y <= 0.0; y += stepSize) {
            let offset = vec2<f32>(x, y);
            let sampleUV = uv + offset;

            if sampleUV.x >= 0.0 && sampleUV.x <= 1.0 &&
                sampleUV.y >= 0.0 && sampleUV.y <= 1.0 {
                let isTarget = isTargetPixel(sampleUV);
                if isTarget > 0.0 {
                    let dist = length(offset);
                    let falloff = 1.0 - smoothstep(0.0, SMOKE_RADIUS, dist);
                    let smoke = calculateSmoke(uv, sampleUV);
                    smokeAccum += smoke * falloff;
                    targetInfluence += falloff;
                }
            }
        }
    }

    smokeAccum /= max(targetInfluence, 1.0);
    targetInfluence = smoothstep(0.0, 1.0, targetInfluence);
    let smokePresence = smokeAccum * targetInfluence;

    // Calculate ghost faces with backgrounds
    var faceAccum = 0.0;
    var bgAccum = 0.0;
    let timeBlock = floor(tron.time / FACE_DURATION);

    if smokePresence > 0.2 {
        for (var i: i32 = 0; i < FACE_COUNT; i++) {
            var facePos = random2(timeBlock + f32(i) * 1234.5);
            facePos = facePos * 0.8 + 0.1;

            let faceTime = glsl_mod(tron.time, FACE_DURATION);
            let fadeFactor = smoothstep(0.0, FACE_TRANSITION, faceTime) *
                              (1.0 - smoothstep(FACE_DURATION - FACE_TRANSITION, FACE_DURATION, faceTime));

            // Add ghost background
            let ghostBg = ghostBackground(uv, facePos) * fadeFactor;
            bgAccum = max(bgAccum, ghostBg);

            // Add face features
            let face = ghostFace(uv, facePos, tron.time, timeBlock + f32(i) * 100.0) * fadeFactor;
            faceAccum = max(faceAccum, face);
        }

        bgAccum *= smoothstep(0.2, 0.4, smokePresence);
        faceAccum *= smoothstep(0.2, 0.4, smokePresence);
    }

    // Combine all elements
    let isTarget = all(abs(originalColor.rgb - TARGET_COLOR) < vec3<f32>(COLOR_TOLERANCE));
    let baseColor = select(originalColor.rgb, REPLACE_COLOR, isTarget);

    // Layer the effects: base -> smoke -> ghost background -> face features
    let smokeEffect = mix(baseColor, SMOKE_COLOR, vec3<f32>(smokeAccum * SMOKE_INTENSITY * targetInfluence * (1.0 - faceAccum)));
    let withBackground = mix(smokeEffect, GHOST_BG_COLOR, vec3<f32>(bgAccum * 0.7));
    let finalColor = mix(withBackground, FACE_COLOR, vec3<f32>(faceAccum));

    let alpha = mix(originalColor.a, ALPHA_MAX, max(smokePresence, max(bgAccum, faceAccum) * smokePresence));

    return vec4<f32>(finalColor, max(alpha, originalColor.a));
}
