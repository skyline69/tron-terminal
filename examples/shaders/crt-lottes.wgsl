// Timothy Lottes CRT: warped tube, scanlines and shadow mask.
// source: https://gist.github.com/qwerasd205/c3da6c610c8ffe17d6d2d3cc7068f17f
// credits: https://github.com/qwerasd205
//==============================================================
//
//    [CRTS] PUBLIC DOMAIN CRT-STYLED SCALAR by Timothy Lottes
//
//    [+] Adapted with alterations for use in Ghostty by Qwerasd.
//    For more information on changes, see comment below license.
//
//==============================================================
//
//      LICENSE = UNLICENSE (aka PUBLIC DOMAIN)
//
//--------------------------------------------------------------
// This is free and unencumbered software released into the
// public domain.
//--------------------------------------------------------------
// Anyone is free to copy, modify, publish, use, compile, sell,
// or distribute this software, either in source code form or as
// a compiled binary, for any purpose, commercial or
// non-commercial, and by any means.
//--------------------------------------------------------------
// In jurisdictions that recognize copyright laws, the author or
// authors of this software dedicate any and all copyright
// interest in the software to the public domain. We make this
// dedication for the benefit of the public at large and to the
// detriment of our heirs and successors. We intend this
// dedication to be an overt act of relinquishment in perpetuity
// of all present and future rights to this software under
// copyright law.
//--------------------------------------------------------------
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY
// KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE
// WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR
// PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS BE
// LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN
// AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT
// OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.
//--------------------------------------------------------------
// For more information, please refer to
// <http://unlicense.org/>
//==============================================================

// This shader is a modified version of the excellent
// FixingPixelArtFast by Timothy Lottes on Shadertoy.
//
// The original shader can be found at:
// https://www.shadertoy.com/view/MtSfRK
//
// Modifications have been made to reduce the verbosity,
// and many of the comments have been removed / reworded.
// Additionally, the license has been moved to the top of
// the file, and can be read above. I (Qwerasd) choose to
// release the modified version under the same license.

// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/crt.glsl
//
// Coordinates: Ghostty on OpenGL hands the shader a bottom-left fragCoord origin
// (Shadertoy convention). This port flips tron's top-left coordinates to match.
// WGSL has no preprocessor: the #define switches are constants below.

// The appearance of this shader can be altered
// by adjusting the parameters defined below.

// "Scanlines" per real screen pixel.
// e.g. SCALE 0.5 means each scanline is 2 pixels.
// Recommended values:
//  o High DPI displays: 0.33333333
//  - Low DPI displays:  0.66666666
// The original ships 0.33333333. This port defaults to the low DPI value:
// shaders cannot see the display scale, and 0.33333333 makes text unreadable
// at 1x. Use 0.33333333 on high DPI displays.
const SCALE: f32 = 0.66666666;

// "Tube" warp
const CRTS_WARP: bool = true;

// Darkness of vignette in corners after warping
//  0.0 = completely black
//  1.0 = no vignetting
const MIN_VIN: f32 = 0.5;

// Try different masks
const CRTS_MASK_GRILLE: i32 = 0;
const CRTS_MASK_GRILLE_LITE: i32 = 1;
const CRTS_MASK_NONE: i32 = 2;
const CRTS_MASK_SHADOW: i32 = 3;
const CRTS_MASK: i32 = CRTS_MASK_SHADOW;

// Scanline thinness
//  0.50 = fused scanlines
//  0.70 = recommended default
//  1.00 = thinner scanlines (too thin)
const INPUT_THIN: f32 = 0.75;

// Horizonal scan blur
//  -3.0 = pixely
//  -2.5 = default
//  -2.0 = smooth
//  -1.0 = too blurry
const INPUT_BLUR: f32 = -2.75;

// Shadow mask effect, ranges from,
//  0.25 = large amount of mask (not recommended, too dark)
//  0.50 = recommended default
//  1.00 = no shadow mask
const INPUT_MASK: f32 = 0.65;

// iChannel0 with a bottom-left origin.
fn channel0(uv: vec2<f32>) -> vec4<f32> {
    return terminal(vec2<f32>(uv.x, 1.0 - uv.y));
}

fn FromSrgb1(c: f32) -> f32 {
    return select(pow(c * (1.0 / 1.055) + (0.055 / 1.055), 2.4), c * (1.0 / 12.92), c <= 0.04045);
}
fn FromSrgb(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        FromSrgb1(c.r), FromSrgb1(c.g), FromSrgb1(c.b));
}

fn CrtsFetch(uv: vec2<f32>) -> vec3<f32> {
    return FromSrgb(channel0(uv.xy).rgb);
}

fn CrtsRcpF1(x: f32) -> f32 {
    return 1.0 / x;
}
fn CrtsSatF1(x: f32) -> f32 {
    return clamp(x, 0.0, 1.0);
}

fn CrtsMax3F1(a: f32, b: f32, c: f32) -> f32 {
    return max(a, max(b, c));
}

fn CrtsTone(
    thin: f32,
    mask_in: f32) -> vec2<f32> {
    var mask = mask_in;
    if CRTS_MASK == CRTS_MASK_NONE {
        mask = 1.0;
    }

    if CRTS_MASK == CRTS_MASK_GRILLE_LITE {
        // Normal R mask is {1.0,mask,mask}
        // LITE   R mask is {mask,1.0,1.0}
        mask = 0.5 + mask * 0.5;
    }

    var ret: vec2<f32>;
    let midOut = 0.18 / ((1.5 - thin) * (0.5 * mask + 0.5));
    let pMidIn = 0.18;
    ret.x = ((-pMidIn) + midOut) / ((1.0 - pMidIn) * midOut);
    ret.y = ((-pMidIn) * midOut + pMidIn) / (midOut * (-pMidIn) + midOut);

    return ret;
}

fn CrtsMask(pos_in: vec2<f32>, dark: f32) -> vec3<f32> {
    var pos = pos_in;
    if CRTS_MASK == CRTS_MASK_GRILLE {
        var m = vec3<f32>(dark, dark, dark);
        let x = fract(pos.x * (1.0 / 3.0));
        if x < (1.0 / 3.0) { m.r = 1.0; }
        else if x < (2.0 / 3.0) { m.g = 1.0; }
        else { m.b = 1.0; }
        return m;
    }

    if CRTS_MASK == CRTS_MASK_GRILLE_LITE {
        var m = vec3<f32>(1.0, 1.0, 1.0);
        let x = fract(pos.x * (1.0 / 3.0));
        if x < (1.0 / 3.0) { m.r = dark; }
        else if x < (2.0 / 3.0) { m.g = dark; }
        else { m.b = dark; }
        return m;
    }

    if CRTS_MASK == CRTS_MASK_NONE {
        return vec3<f32>(1.0, 1.0, 1.0);
    }

    // CRTS_MASK_SHADOW
    pos.x += pos.y * 3.0;
    var m = vec3<f32>(dark, dark, dark);
    let x = fract(pos.x * (1.0 / 6.0));
    if x < (1.0 / 3.0) { m.r = 1.0; }
    else if x < (2.0 / 3.0) { m.g = 1.0; }
    else { m.b = 1.0; }
    return m;
}

fn CrtsFilter(
    ipos: vec2<f32>,
    inputSizeDivOutputSize: vec2<f32>,
    halfInputSize: vec2<f32>,
    rcpInputSize: vec2<f32>,
    rcpOutputSize: vec2<f32>,
    twoDivOutputSize: vec2<f32>,
    inputHeight: f32,
    warp: vec2<f32>,
    thin: f32,
    blur: f32,
    mask: f32,
    tone: vec2<f32>
) -> vec3<f32> {
    // Optional apply warp
    var pos: vec2<f32>;
    var vin = 1.0;
    if CRTS_WARP {
        // Convert to {-1 to 1} range
        pos = ipos * twoDivOutputSize - vec2<f32>(1.0, 1.0);

        // Distort pushes image outside {-1 to 1} range
        pos *= vec2<f32>(
            1.0 + (pos.y * pos.y) * warp.x,
            1.0 + (pos.x * pos.x) * warp.y);

        // TODO: Vignette needs optimization
        vin = 1.0 - (
            (1.0 - CrtsSatF1(pos.x * pos.x)) * (1.0 - CrtsSatF1(pos.y * pos.y)));
        vin = CrtsSatF1((-vin) * inputHeight + inputHeight);

        // Leave in {0 to inputSize}
        pos = pos * halfInputSize + halfInputSize;
    } else {
        pos = ipos * inputSizeDivOutputSize;
    }

    // Snap to center of first scanline
    let y0 = floor(pos.y - 0.5) + 0.5;
    // Snap to center of one of four pixels
    let x0 = floor(pos.x - 1.5) + 0.5;

    // Inital UV position
    var p = vec2<f32>(x0 * rcpInputSize.x, y0 * rcpInputSize.y);
    // Fetch 4 nearest texels from 2 nearest scanlines
    let colA0 = CrtsFetch(p);
    p.x += rcpInputSize.x;
    let colA1 = CrtsFetch(p);
    p.x += rcpInputSize.x;
    let colA2 = CrtsFetch(p);
    p.x += rcpInputSize.x;
    let colA3 = CrtsFetch(p);
    p.y += rcpInputSize.y;
    let colB3 = CrtsFetch(p);
    p.x -= rcpInputSize.x;
    let colB2 = CrtsFetch(p);
    p.x -= rcpInputSize.x;
    let colB1 = CrtsFetch(p);
    p.x -= rcpInputSize.x;
    let colB0 = CrtsFetch(p);

    // Vertical filter
    // Scanline intensity is using sine wave
    // Easy filter window and integral used later in exposure
    let off = pos.y - y0;
    let pi2 = 6.28318530717958;
    let hlf = 0.5;
    var scanA = cos(min(0.5, off * thin) * pi2) * hlf + hlf;
    var scanB = cos(min(0.5, (-off) * thin + thin) * pi2) * hlf + hlf;

    // Horizontal kernel is simple gaussian filter
    let off0 = pos.x - x0;
    let off1 = off0 - 1.0;
    let off2 = off0 - 2.0;
    let off3 = off0 - 3.0;
    let pix0 = pow(2.0, blur * off0 * off0);
    let pix1 = pow(2.0, blur * off1 * off1);
    let pix2 = pow(2.0, blur * off2 * off2);
    let pix3 = pow(2.0, blur * off3 * off3);
    var pixT = CrtsRcpF1(pix0 + pix1 + pix2 + pix3);

    if CRTS_WARP {
        // Get rid of wrong pixels on edge
        pixT *= max(MIN_VIN, vin);
    }

    scanA *= pixT;
    scanB *= pixT;

    // Apply horizontal and vertical filters
    var color =
        (colA0 * pix0 + colA1 * pix1 + colA2 * pix2 + colA3 * pix3) * scanA +
        (colB0 * pix0 + colB1 * pix1 + colB2 * pix2 + colB3 * pix3) * scanB;

    // Apply phosphor mask
    color *= CrtsMask(ipos, mask);

    // Tonal control, start by protecting from /0
    var peak = max(1.0 / (256.0 * 65536.0),
        CrtsMax3F1(color.r, color.g, color.b));
    // Compute the ratios of {R,G,B}
    let ratio = color * CrtsRcpF1(peak);
    // Apply tonal curve to peak value
    peak = peak * CrtsRcpF1(peak * tone.x + tone.y);
    // Reconstruct color
    return ratio * peak;
}

fn ToSrgb1(c: f32) -> f32 {
    return select(1.055 * pow(c, 0.41666) - 0.055, c * 12.92, c < 0.0031308);
}
fn ToSrgb(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        ToSrgb1(c.r), ToSrgb1(c.g), ToSrgb1(c.b));
}

fn shade(screen_uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let fragCoord = vec2<f32>(frag_coord.x, tron.resolution.y - frag_coord.y);
    let iResolution = tron.resolution;
    let aspect = iResolution.x / iResolution.y;
    let rgb = CrtsFilter(
        fragCoord.xy,
        vec2<f32>(1.0),
        iResolution.xy * SCALE * 0.5,
        1.0 / (iResolution.xy * SCALE),
        1.0 / iResolution.xy,
        2.0 / iResolution.xy,
        iResolution.y,
        vec2<f32>(1.0 / (50.0 * aspect), 1.0 / 50.0),
        INPUT_THIN,
        INPUT_BLUR,
        INPUT_MASK,
        CrtsTone(INPUT_THIN, INPUT_MASK)
    );

    // Linear to SRGB for output.
    return vec4<f32>(ToSrgb(rgb), 1.0);
}
