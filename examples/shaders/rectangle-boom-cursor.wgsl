// Filled rectangle that bursts out of the cursor when it changes
//
// rectangle_boom_cursor.glsl from ghostty-cursor-shaders by Sahaj Bhatt.
// Ported to WGSL for tron from https://github.com/sahaj-b/ghostty-cursor-shaders (MIT, Copyright (c) 2026 Sahaj Bhatt)
//
// Porting notes:
// - The original fires when the cursor width changes, as when a block cursor
//   turns into a beam. tron's cursor rectangle is always the full cell, so
//   TRIGGER_ON_MOVE (on by default) fires on every cursor move as well.
// - No effect when the cursor was hidden before the change.
// Reads tron.cursor_change_time, so tron redraws for a moment after each move.

// CONFIGURATION
const DURATION: f32 = 0.15; // How long the ripple animates (seconds)
const MAX_SIZE: f32 = 0.05; // Max radius in normalized coords (0.5 = 1/4 screen height)
const ANIMATION_START_OFFSET: f32 = 0.0; // Start the ripple slightly progressed (0.0 - 1.0)
// Straight alpha. The original suggests iCurrentCursorColor for your cursor's
// color; tron has none, see cursor_color() in cursor-sweep.wgsl for a stand-in.
const COLOR: vec4<f32> = vec4<f32>(0.35, 0.36, 0.44, 1.0);
const CURSOR_WIDTH_CHANGE_THRESHOLD: f32 = 0.5; // Triggers ripple if cursor width changes by this fraction
const BLUR: f32 = 3.0; // Blur level in pixels
const TRIGGER_ON_MOVE: bool = true; // tron only: also trigger on every cursor move

// Easing: easeOutCirc. Alternatives from the original, as the body of `ease`:
//   linear         return t;
//   easeOutQuad    return 1.0 - (1.0 - t) * (1.0 - t);
//   easeInOutQuad  return select(1.0 - pow(-2.0 * t + 2.0, 2.0) / 2.0, 2.0 * t * t, t < 0.5);
//   easeOutCubic   return 1.0 - pow(1.0 - t, 3.0);
//   easeOutQuart   return 1.0 - pow(1.0 - t, 4.0);
//   easeOutQuint   return 1.0 - pow(1.0 - t, 5.0);
//   easeOutExpo    return select(1.0 - pow(2.0, -10.0 * t), 1.0, t == 1.0);
//   easeOutSine    return sin((t * 3.1415916) / 2.0);
//   easeOutBack    let y = t - 1.0; return 1.0 + 2.70158 * y * y * y + 1.70158 * y * y;
fn ease(t: f32) -> f32 {
    // (t - 1)^2 written out: pow() is undefined for a negative base in WGSL.
    return sqrt(1.0 - (t - 1.0) * (t - 1.0));
}

// Pulse fade: 1.0 - easeOutPulse. Alternatives from the original, as the body of `fade`:
//   no fade               return 1.0;
//   linear fade           return 1.0 - ease(t);
//   smoothstepPulse       return 1.0 - 4.0 * t * (1.0 - t);
//   powerCurvePulse       let x = t * 2.0 - 1.0; return x * x;
//   doubleSmoothstepPulse return smoothstep(0.0, 0.5, t) * (1.0 - smoothstep(0.5, 1.0, t));
//   exponentialDecayPulse return exp(-3.0 * t) * sin(t * 3.1415916);
//   sinPulse              return sin(t * 3.1415916);
fn fade(t: f32) -> f32 {
    return 1.0 - t * (2.0 - t);
}

// Ghostty gives shaders pixel coordinates with the origin at the bottom left
// and y up, and iCurrentCursor.xy is the cursor's top-left corner in that
// space. tron uses a top-left origin with y down, so flip y into Ghostty's
// space and keep the original math.
fn ghostty_point(p: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(p.x, tron.resolution.y - p.y);
}

fn ghostty_rect(r: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(r.x, tron.resolution.y - r.y, r.z, r.w);
}

// Pixels to Ghostty's normalized coordinates: y from -1 to 1, x scaled to match.
fn norm(value: vec2<f32>, is_position: f32) -> vec2<f32> {
    return (value * 2.0 - tron.resolution * is_position) / tron.resolution.y;
}

fn sdf_rectangle(p: vec2<f32>, xy: vec2<f32>, b: vec2<f32>) -> f32 {
    let d = abs(p - xy) - b;
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0);
}

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let frag_color = terminal(uv);
    if tron.cursor.z <= 0.0 || tron.previous_cursor.z <= 0.0 {
        return frag_color;
    }

    // Normalization & setup (-1 to 1 coords)
    let vu = norm(ghostty_point(frag_coord), 1.0);
    let offset_factor = vec2<f32>(-0.5, 0.5);

    let cc = ghostty_rect(tron.cursor);
    let cp = ghostty_rect(tron.previous_cursor);
    let current_cursor = vec4<f32>(norm(cc.xy, 1.0), norm(cc.zw, 0.0));
    let previous_cursor = vec4<f32>(norm(cp.xy, 1.0), norm(cp.zw, 0.0));

    let center_cc = current_cursor.xy - (current_cursor.zw * offset_factor);

    let cell_width = max(current_cursor.z, previous_cursor.z); // width of the 'block' cursor

    // check for significant width change
    let width_change = abs(current_cursor.z - previous_cursor.z);
    let width_threshold_norm = cell_width * CURSOR_WIDTH_CHANGE_THRESHOLD;
    let is_mode_change = step(width_threshold_norm, width_change);

    // ANIMATION
    let ripple_progress = (tron.time - tron.cursor_change_time) / DURATION + ANIMATION_START_OFFSET;
    // don't clamp yet; we need to know if it's > 1.0 (finished)
    let is_animating = 1.0 - step(1.0, ripple_progress); // progress < 1.0 ? 1.0: 0.0

    if (is_mode_change > 0.0 || TRIGGER_ON_MOVE) && is_animating > 0.0 {
        let eased_progress = ease(ripple_progress);

        // RIPPLE CALCULATION
        let ripple_expansion = eased_progress * MAX_SIZE;

        let fade_amount = fade(ripple_progress);

        let half_size_cc = vec2<f32>(current_cursor.z, current_cursor.w) * 0.5 + vec2<f32>(ripple_expansion);
        let sdf_rect_ring = sdf_rectangle(vu, center_cc, half_size_cc);

        // Antialias (1-pixel width in normalized coords)
        let anti_alias_size = norm(vec2<f32>(BLUR, BLUR), 0.0).x;
        let ripple = (1.0 - smoothstep(-anti_alias_size, anti_alias_size, sdf_rect_ring)) * fade_amount;
        // Apply ripple effect
        return mix(frag_color, vec4<f32>(COLOR.rgb * COLOR.a, COLOR.a), ripple * COLOR.a);
    }
    // else: do nothing, keep original frag_color
    return frag_color;
}
