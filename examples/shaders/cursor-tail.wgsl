// Comet-like cursor tail, similar to kitty's cursor_trail
//
// cursor_tail.glsl from ghostty-cursor-shaders by Sahaj Bhatt.
// Ported to WGSL for tron from https://github.com/sahaj-b/ghostty-cursor-shaders (MIT, Copyright (c) 2026 Sahaj Bhatt)
//
// Porting notes:
// - tron has no cursor color uniform, so the trail takes the color of the
//   rendered cursor, sampled from the terminal (see `cursor_color`). The
//   original converts Ghostty's sRGB cursor color to linear; the sampled color
//   is already in the terminal texture's space, so no conversion is needed.
// - No trail when the cursor was hidden before the move (zero-size rectangle).
// Reads tron.cursor_change_time, so tron redraws for a moment after each move.

// --- CONFIGURATION ---
// How long tron redraws after the cursor moves: the effect must not change after it.
const TRON_CURSOR_DURATION: f32 = 0.09; // in seconds
const DURATION: f32 = TRON_CURSOR_DURATION;
const MAX_TRAIL_LENGTH: f32 = 0.2;
const THRESHOLD_MIN_DISTANCE: f32 = 1.5; // min distance to show trail (units of cursor width)
const BLUR: f32 = 2.0; // blur size in pixels (for antialiasing)

// --- CONSTANTS for easing functions ---
const PI: f32 = 3.14159265359;
const C1_BACK: f32 = 1.70158;
const C2_BACK: f32 = C1_BACK * 1.525;
const C3_BACK: f32 = C1_BACK + 1.0;
const C4_ELASTIC: f32 = (2.0 * PI) / 3.0;
const C5_ELASTIC: f32 = (2.0 * PI) / 4.5;
const SPRING_STIFFNESS: f32 = 9.0;
const SPRING_DAMPING: f32 = 0.9;

// --- EASING FUNCTIONS ---
// EaseOutCirc. Alternatives from the original, as the body of `ease`:
//   Linear          return x;
//   EaseOutQuad     return 1.0 - (1.0 - x) * (1.0 - x);
//   EaseOutCubic    return 1.0 - pow(1.0 - x, 3.0);
//   EaseOutQuart    return 1.0 - pow(1.0 - x, 4.0);
//   EaseOutQuint    return 1.0 - pow(1.0 - x, 5.0);
//   EaseOutSine     return sin((x * PI) / 2.0);
//   EaseOutExpo     return select(1.0 - pow(2.0, -10.0 * x), 1.0, x == 1.0);
//   EaseOutBack     let y = x - 1.0; return 1.0 + C3_BACK * y * y * y + C1_BACK * y * y;
//   EaseOutElastic  return select(select(pow(2.0, -10.0 * x) * sin((x * 10.0 - 0.75) * C4_ELASTIC) + 1.0, 1.0, x == 1.0), 0.0, x == 0.0);
//   Spring          let t = clamp(x, 0.0, 1.0);
//                   let decay = exp(-SPRING_DAMPING * SPRING_STIFFNESS * t);
//                   let freq = sqrt(SPRING_STIFFNESS * (1.0 - SPRING_DAMPING * SPRING_DAMPING));
//                   let osc = cos(freq * 6.283185 * t) + (SPRING_DAMPING * sqrt(SPRING_STIFFNESS) / freq) * sin(freq * 6.283185 * t);
//                   return 1.0 - decay * osc;
fn ease(x: f32) -> f32 {
    // (x - 1)^2 written out: pow() is undefined for a negative base in WGSL.
    return sqrt(1.0 - (x - 1.0) * (x - 1.0));
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

// Stands in for iCurrentCursorColor: the rendered pixel half a pixel inside
// the left edge of the cursor cell, where block, beam and hollow cursors draw.
fn cursor_color() -> vec4<f32> {
    let p = vec2<f32>(tron.cursor.x + 0.5, tron.cursor.y + tron.cursor.w * 0.5);
    let c = terminal(p / tron.resolution);
    return vec4<f32>(c.rgb / max(c.a, 1e-4), c.a);
}

fn sdf_rectangle(p: vec2<f32>, xy: vec2<f32>, b: vec2<f32>) -> f32 {
    let d = abs(p - xy) - b;
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0);
}

// Based on Inigo Quilez's 2D distance functions article: https://iquilezles.org/articles/distfunctions2d/
// Potencially optimized by eliminating conditionals and loops to enhance performance and reduce branching

fn seg(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>, s: ptr<function, f32>, d: f32) -> f32 {
    let e = b - a;
    let w = p - a;
    // max() keeps collapsed edges from dividing by zero.
    let proj = a + e * clamp(dot(w, e) / max(dot(e, e), 1e-10), 0.0, 1.0);
    let segd = dot(p - proj, p - proj);
    let dist = min(d, segd);

    let c0 = step(0.0, p.y - a.y);
    let c1 = 1.0 - step(0.0, p.y - b.y);
    let c2 = 1.0 - step(0.0, e.x * w.y - e.y * w.x);
    let all_cond = c0 * c1 * c2;
    let none_cond = (1.0 - c0) * (1.0 - c1) * (1.0 - c2);
    let flip = mix(1.0, -1.0, step(0.5, all_cond + none_cond));
    *s = *s * flip;
    return dist;
}

fn sdf_parallelogram(p: vec2<f32>, v0: vec2<f32>, v1: vec2<f32>, v2: vec2<f32>, v3: vec2<f32>) -> f32 {
    var s = 1.0;
    var d = dot(p - v0, p - v0);

    d = seg(p, v0, v3, &s, d);
    d = seg(p, v1, v0, &s, d);
    d = seg(p, v2, v1, &s, d);
    d = seg(p, v3, v2, &s, d);

    return s * sqrt(d);
}

// Pixels to Ghostty's normalized coordinates: y from -1 to 1, x scaled to match.
fn norm(value: vec2<f32>, is_position: f32) -> vec2<f32> {
    return (value * 2.0 - tron.resolution * is_position) / tron.resolution.y;
}

fn antialiasing(dist: f32) -> f32 {
    return 1.0 - smoothstep(0.0, norm(vec2<f32>(BLUR, BLUR), 0.0).x, dist);
}

fn determine_if_top_right_is_leading(a: vec2<f32>, b: vec2<f32>) -> f32 {
    let condition1 = step(b.x, a.x) * step(a.y, b.y); // a.x < b.x && a.y > b.y
    let condition2 = step(a.x, b.x) * step(b.y, a.y); // a.x > b.x && a.y < b.y

    // if neither condition is met, return 1 (else case)
    return 1.0 - max(condition1, condition2);
}

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let frag_color = terminal(uv);
    if tron.cursor.z <= 0.0 || tron.previous_cursor.z <= 0.0 {
        return frag_color;
    }

    // normalization & setup(-1, 1 coords)
    let vu = norm(ghostty_point(frag_coord), 1.0);
    let offset_factor = vec2<f32>(-0.5, 0.5);

    let cc = ghostty_rect(tron.cursor);
    let cp = ghostty_rect(tron.previous_cursor);
    let current_cursor = vec4<f32>(norm(cc.xy, 1.0), norm(cc.zw, 0.0));
    let previous_cursor = vec4<f32>(norm(cp.xy, 1.0), norm(cp.zw, 0.0));

    let center_cc = current_cursor.xy - (current_cursor.zw * offset_factor);
    let center_cp = previous_cursor.xy - (previous_cursor.zw * offset_factor);

    let delta = center_cp - center_cc;
    let line_length = length(delta);

    let sdf_current_cursor = sdf_rectangle(vu, center_cc, current_cursor.zw * 0.5);

    var new_color = frag_color;

    let min_dist = current_cursor.w * THRESHOLD_MIN_DISTANCE;
    let progress = clamp((tron.time - tron.cursor_change_time) / DURATION, 0.0, 1.0);
    if line_length > min_dist {
        // ANIMATION logic

        let tail_delay_factor = MAX_TRAIL_LENGTH / line_length;

        let is_long_move = step(MAX_TRAIL_LENGTH, line_length);

        let head_eased_short = ease(progress);
        let tail_eased_short = ease(smoothstep(tail_delay_factor, 1.0, progress));
        let head_eased_long = 1.0;
        let tail_eased_long = ease(progress);

        // select() instead of mix(): on short moves the smoothstep above has
        // edges out of order and can produce NaN, which mix() would spread.
        let head_eased = select(head_eased_long, head_eased_short, is_long_move > 0.5);
        let tail_eased = select(tail_eased_long, tail_eased_short, is_long_move > 0.5);

        // detect straight moves
        let delta_abs = abs(center_cc - center_cp);
        let threshold = 0.001;
        let is_horizontal = step(delta_abs.y, threshold);
        let is_vertical = step(delta_abs.x, threshold);
        let is_straight_move = max(is_horizontal, is_vertical);

        // -- Making the parallelogram sdf (diagonal move) --

        // animate the TOP-LEFT corners
        let head_pos_tl = mix(previous_cursor.xy, current_cursor.xy, head_eased);
        let tail_pos_tl = mix(previous_cursor.xy, current_cursor.xy, tail_eased);

        let is_top_right_leading = determine_if_top_right_is_leading(current_cursor.xy, previous_cursor.xy);
        let is_bottom_left_leading = 1.0 - is_top_right_leading;

        // v0, v1 : "front" of the trail (head)
        let v0 = vec2<f32>(head_pos_tl.x + current_cursor.z * is_top_right_leading, head_pos_tl.y - current_cursor.w);
        let v1 = vec2<f32>(head_pos_tl.x + current_cursor.z * is_bottom_left_leading, head_pos_tl.y);

        // v2, v3: "back" of the trail (tail)
        let v2 = vec2<f32>(tail_pos_tl.x + current_cursor.z * is_bottom_left_leading, tail_pos_tl.y);
        let v3 = vec2<f32>(tail_pos_tl.x + current_cursor.z * is_top_right_leading, tail_pos_tl.y - previous_cursor.w);

        let sdf_trail_diag = sdf_parallelogram(vu, v0, v1, v2, v3);

        // -- Making the rectangle sdf (straight move) --

        let head_center = mix(center_cp, center_cc, head_eased);
        let tail_center = mix(center_cp, center_cc, tail_eased);

        let min_center = min(head_center, tail_center);
        let max_center = max(head_center, tail_center);

        let box_size = (max_center - min_center) + current_cursor.zw;
        let box_center = (min_center + max_center) * 0.5;

        let sdf_trail_rect = sdf_rectangle(vu, box_center, box_size * 0.5);

        // -- FINAL SELECTING AND DRAWING --
        let sdf_trail = select(sdf_trail_diag, sdf_trail_rect, is_straight_move > 0.5);

        let trail = cursor_color();
        let trail_alpha = antialiasing(sdf_trail);
        new_color = mix(new_color, vec4<f32>(trail.rgb * trail.a, trail.a), trail_alpha);

        // punch hole
        new_color = mix(new_color, frag_color, step(sdf_current_cursor, 0.0));
    }

    return new_color;
}
