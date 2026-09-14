// Drifting volumetric starfield and nebula behind dark areas.
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/galaxy.glsl
// (The original file has no credit or license notice.)
//
// Coordinates: Ghostty on OpenGL hands the shader a bottom-left fragCoord origin
// (Shadertoy convention). This port flips tron's top-left coordinates to match.

const baseSpeed: f32 = 0.02;
const maxIterations: i32 = 16;
const formulaParameter: f32 = 0.79;
const volumeSteps: f32 = 7.0;
const stepSize: f32 = 0.24;
const zoomFactor: f32 = 0.1;
const tilingFactor: f32 = 0.85;
const baseBrightness: f32 = 0.0008;
const darkMatter: f32 = 0.2;
const distanceFading: f32 = 0.56;
const colorSaturation: f32 = 0.9;
const transverseMotion: f32 = 0.2;
const cloudOpacity: f32 = 0.48;
const zoomSpeed: f32 = 0.0002;

// GLSL mod: x - y * floor(x / y).
fn glsl_mod(x: f32, y: f32) -> f32 {
    return x - y * floor(x / y);
}
fn glsl_mod3(x: vec3<f32>, y: vec3<f32>) -> vec3<f32> {
    return x - y * floor(x / y);
}

fn triangle(x: f32, period: f32) -> f32 {
    return 2.0 * abs(3.0 * ((x / period) - floor((x / period) + 0.5))) - 1.0;
}

fn field(position_in: vec3<f32>) -> f32 {
    var position = position_in;
    let iTime = tron.time;
    let strength = 7.0 + 0.03 * log(1.0e-6 + fract(sin(iTime) * 373.11));
    var accumulated = 0.0;
    var previousMagnitude = 0.0;
    var totalWeight = 0.0;

    for (var i = 0; i < 6; i++) {
        let magnitude = dot(position, position);
        position = abs(position) / magnitude + vec3<f32>(-0.5, -0.8 + 0.1 * sin(-iTime * 0.1 + 2.0), -1.1 + 0.3 * cos(iTime * 0.3));
        let weight = exp(-f32(i) / 7.0);
        accumulated += weight * exp(-strength * pow(abs(magnitude - previousMagnitude), 2.3));
        totalWeight += weight;
        previousMagnitude = magnitude;
    }

    return max(0.0, 5.0 * accumulated / totalWeight - 0.7);
}

// iChannel0 with a bottom-left origin.
fn channel0(uv: vec2<f32>) -> vec4<f32> {
    return terminal(vec2<f32>(uv.x, 1.0 - uv.y));
}

fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = vec2<f32>(frag_coord.x, tron.resolution.y - frag_coord.y);
    let iTime = tron.time;

    let normalizedCoordinates = 2.0 * fragCoord / vec2<f32>(512.0) - 1.0;
    let scaledCoordinates = normalizedCoordinates * vec2<f32>(512.0) / 512.0;

    let timeElapsed = iTime;
    var speedAdjustment = -baseSpeed;
    let formulaAdjustment = formulaParameter;

    speedAdjustment = zoomSpeed * cos(iTime * 0.02 + 3.1415926 / 4.0);

    let uvCoordinates = scaledCoordinates;

    let rotationXZ = 0.9;
    let rotationYZ = -0.6;
    let rotationXY = 0.9 + iTime * 0.08;

    let rotationMatrixXZ = mat2x2<f32>(vec2<f32>(cos(rotationXZ), sin(rotationXZ)), vec2<f32>(-sin(rotationXZ), cos(rotationXZ)));
    let rotationMatrixYZ = mat2x2<f32>(vec2<f32>(cos(rotationYZ), sin(rotationYZ)), vec2<f32>(-sin(rotationYZ), cos(rotationYZ)));
    let rotationMatrixXY = mat2x2<f32>(vec2<f32>(cos(rotationXY), sin(rotationXY)), vec2<f32>(-sin(rotationXY), cos(rotationXY)));

    let canvasCenter = vec2<f32>(0.5, 0.5);
    var rayDirection = vec3<f32>(uvCoordinates * zoomFactor, 1.0);
    var cameraPosition = vec3<f32>(0.0, 0.0, 0.0);
    cameraPosition.x -= 2.0 * (canvasCenter.x - 0.5);
    cameraPosition.y -= 2.0 * (canvasCenter.y - 0.5);

    var forwardVector = vec3<f32>(0.0, 0.0, 1.0);
    cameraPosition.x += transverseMotion * cos(0.01 * iTime) + 0.001 * iTime;
    cameraPosition.y += transverseMotion * sin(0.01 * iTime) + 0.001 * iTime;
    cameraPosition.z += 0.003 * iTime;

    // WGSL cannot assign to swizzles, so `v.xz *= m` is spelled out.
    var tmp = rayDirection.xz * rotationMatrixXZ;
    rayDirection.x = tmp.x; rayDirection.z = tmp.y;
    tmp = forwardVector.xz * rotationMatrixXZ;
    forwardVector.x = tmp.x; forwardVector.z = tmp.y;
    tmp = rayDirection.yz * rotationMatrixYZ;
    rayDirection.y = tmp.x; rayDirection.z = tmp.y;
    tmp = forwardVector.yz * rotationMatrixYZ;
    forwardVector.y = tmp.x; forwardVector.z = tmp.y;

    tmp = cameraPosition.xy * (-1.0 * rotationMatrixXY);
    cameraPosition.x = tmp.x; cameraPosition.y = tmp.y;
    tmp = cameraPosition.xz * rotationMatrixXZ;
    cameraPosition.x = tmp.x; cameraPosition.z = tmp.y;
    tmp = cameraPosition.yz * rotationMatrixYZ;
    cameraPosition.y = tmp.x; cameraPosition.z = tmp.y;

    let zoomOffset = (timeElapsed - 3311.0) * speedAdjustment;
    cameraPosition += forwardVector * zoomOffset;
    let sampleOffset = glsl_mod(zoomOffset, stepSize);
    let normalizedSampleOffset = sampleOffset / stepSize;

    var stepDistance = 0.24;
    var secondaryStepDistance = stepDistance + stepSize / 2.0;
    var accumulatedColor = vec3<f32>(0.0);
    var fieldContribution = 0.0;
    var backgroundColor = vec3<f32>(0.0);

    for (var stepIndex = 0.0; stepIndex < volumeSteps; stepIndex += 1.0) {
        var primaryPosition = cameraPosition + (stepDistance + sampleOffset) * rayDirection;
        var secondaryPosition = cameraPosition + (secondaryStepDistance + sampleOffset) * rayDirection;

        primaryPosition = abs(vec3<f32>(tilingFactor) - glsl_mod3(primaryPosition, vec3<f32>(tilingFactor * 2.0)));
        secondaryPosition = abs(vec3<f32>(tilingFactor) - glsl_mod3(secondaryPosition, vec3<f32>(tilingFactor * 2.0)));

        fieldContribution = field(secondaryPosition);

        var particleAccumulator = 0.0;
        var particleDistance = 0.0;
        for (var i = 0; i < maxIterations; i++) {
            primaryPosition = abs(primaryPosition) / dot(primaryPosition, primaryPosition) - formulaAdjustment;
            let distanceChange = abs(length(primaryPosition) - particleDistance);
            particleAccumulator += select(distanceChange, min(12.0, distanceChange), i > 2);
            particleDistance = length(primaryPosition);
        }
        particleAccumulator *= particleAccumulator * particleAccumulator;

        let fadeFactor = pow(distanceFading, max(0.0, stepIndex - normalizedSampleOffset));
        accumulatedColor += vec3<f32>(stepDistance, stepDistance * stepDistance, stepDistance * stepDistance * stepDistance * stepDistance)
            * particleAccumulator * baseBrightness * fadeFactor;
        backgroundColor += mix(0.4, 1.0, cloudOpacity) * vec3<f32>(1.8 * fieldContribution * fieldContribution * fieldContribution,
            1.4 * fieldContribution * fieldContribution, fieldContribution) * fadeFactor;
        stepDistance += stepSize;
        secondaryStepDistance += stepSize;
    }

    accumulatedColor = mix(vec3<f32>(length(accumulatedColor)), accumulatedColor, colorSaturation);

    let foregroundColor = vec4<f32>(accumulatedColor * 0.01, 1.0);
    backgroundColor *= cloudOpacity;
    backgroundColor.b *= 1.8;
    backgroundColor.r *= 0.05;

    backgroundColor.b = 0.5 * mix(backgroundColor.g, backgroundColor.b, 0.8);
    backgroundColor.g = 0.0;
    let bg = mix(backgroundColor.gb, backgroundColor.bg, 0.5 * (cos(iTime * 0.01) + 1.0));
    backgroundColor.b = bg.x;
    backgroundColor.g = bg.y;

    let terminalUV = fragCoord / tron.resolution;
    let terminalColor = channel0(terminalUV);

    let brightnessThreshold = 0.1;
    let terminalBrightness = dot(terminalColor.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));

    if terminalBrightness < brightnessThreshold {
        return mix(terminalColor, vec4<f32>(foregroundColor.rgb + backgroundColor, 1.0), 0.24);
    } else {
        return terminalColor;
    }
}
