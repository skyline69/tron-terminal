// Neovide-like cursor trail whose corners stretch and catch up
//
// cursor_warp.glsl from ghostty-cursor-shaders by Sahaj Bhatt.
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
const DURATION: f32 = 0.2; // total animation time
const TRAIL_SIZE: f32 = 0.8; // 0.0 = all corners move together. 1.0 = max smear (leading corners jump instantly)
const THRESHOLD_MIN_DISTANCE: f32 = 1.5; // min distance to show trail (units of cursor height)
const BLUR: f32 = 1.0; // blur size in pixels (for antialiasing)
const TRAIL_THICKNESS: f32 = 1.0; // 1.0 = full cursor height, 0.0 = zero height, >1.0 = funky aah
const TRAIL_THICKNESS_X: f32 = 0.9;

const FADE_ENABLED: f32 = 0.0; // 1.0 to enable fade gradient along the trail, 0.0 to disable
const FADE_EXPONENT: f32 = 5.0; // exponent for fade gradient along the trail

// --- CONSTANTS for easing functions ---
const PI: f32 = 3.14159265359;
const C1_BACK: f32 = 1.70158;
const C2_BACK: f32 = C1_BACK * 1.525;
const C3_BACK: f32 = C1_BACK + 1.0;
const C4_ELASTIC: f32 = (2.0 * PI) / 3.0;
const C5_ELASTIC: f32 = (2.0 * PI) / 4.5;
const SPRING_STIFFNESS: f32 = 9.0;
const SPRING_DAMPING: f32 = 0.9;

// calculating durations for every corner
const DURATION_TRAIL: f32 = DURATION;
const DURATION_LEAD: f32 = DURATION * (1.0 - TRAIL_SIZE);
const DURATION_SIDE: f32 = (DURATION_LEAD + DURATION_TRAIL) / 2.0;

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

fn sdf_convex_quad(p: vec2<f32>, v1: vec2<f32>, v2: vec2<f32>, v3: vec2<f32>, v4: vec2<f32>) -> f32 {
    var s = 1.0;
    var d = dot(p - v1, p - v1);

    d = seg(p, v1, v2, &s, d);
    d = seg(p, v2, v3, &s, d);
    d = seg(p, v3, v4, &s, d);
    d = seg(p, v4, v1, &s, d);

    return s * sqrt(d);
}

// Pixels to Ghostty's normalized coordinates: y from -1 to 1, x scaled to match.
fn norm(value: vec2<f32>, is_position: f32) -> vec2<f32> {
    return (value * 2.0 - tron.resolution * is_position) / tron.resolution.y;
}

fn antialiasing(dist: f32, blur_amount: f32) -> f32 {
    return 1.0 - smoothstep(0.0, norm(vec2<f32>(blur_amount, blur_amount), 0.0).x, dist);
}

// Determines animation duration based on a corner's alignment with the move direction(dot product)
// dot_val will be in [-2, 2]
// > 0.5 (1 or 2) = Leading
// > -0.5 (0)     = Side
// <= -0.5 (-1 or -2) = Trailing
fn duration_from_dot(dot_val: f32, duration_lead: f32, duration_side: f32, duration_trail: f32) -> f32 {
    let is_lead = step(0.5, dot_val);
    let is_side = step(-0.5, dot_val) * (1.0 - is_lead);

    // Start with trailing duration
    let duration = mix(duration_trail, duration_side, is_side);
    // Mix in leading duration
    return mix(duration, duration_lead, is_lead);
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
    let half_size_cc = current_cursor.zw * 0.5;
    let center_cp = previous_cursor.xy - (previous_cursor.zw * offset_factor);

    let sdf_current_cursor = sdf_rectangle(vu, center_cc, half_size_cc);

    let line_length = distance(center_cc, center_cp);
    let min_dist = current_cursor.w * THRESHOLD_MIN_DISTANCE;

    var new_color = frag_color;

    let base_progress = tron.time - tron.cursor_change_time;

    if line_length > min_dist && base_progress < DURATION - 0.001 {
        // defining corners of cursors

        // Y (Height) with TRAIL_THICKNESS
        let cc_half_height = current_cursor.w * 0.5;
        let cc_center_y = current_cursor.y - cc_half_height;
        let cc_new_half_height = cc_half_height * TRAIL_THICKNESS;
        let cc_new_top_y = cc_center_y + cc_new_half_height;
        let cc_new_bottom_y = cc_center_y - cc_new_half_height;

        // X (Width) with TRAIL_THICKNESS
        let cc_half_width = current_cursor.z * 0.5;
        let cc_center_x = current_cursor.x + cc_half_width;
        let cc_new_half_width = cc_half_width * TRAIL_THICKNESS_X;
        let cc_new_left_x = cc_center_x - cc_new_half_width;
        let cc_new_right_x = cc_center_x + cc_new_half_width;

        let cc_tl = vec2<f32>(cc_new_left_x, cc_new_top_y);
        let cc_tr = vec2<f32>(cc_new_right_x, cc_new_top_y);
        let cc_bl = vec2<f32>(cc_new_left_x, cc_new_bottom_y);
        let cc_br = vec2<f32>(cc_new_right_x, cc_new_bottom_y);

        // same thing for previous cursor
        let cp_half_height = previous_cursor.w * 0.5;
        let cp_center_y = previous_cursor.y - cp_half_height;
        let cp_new_half_height = cp_half_height * TRAIL_THICKNESS;
        let cp_new_top_y = cp_center_y + cp_new_half_height;
        let cp_new_bottom_y = cp_center_y - cp_new_half_height;

        let cp_half_width = previous_cursor.z * 0.5;
        let cp_center_x = previous_cursor.x + cp_half_width;
        let cp_new_half_width = cp_half_width * TRAIL_THICKNESS_X;
        let cp_new_left_x = cp_center_x - cp_new_half_width;
        let cp_new_right_x = cp_center_x + cp_new_half_width;

        let cp_tl = vec2<f32>(cp_new_left_x, cp_new_top_y);
        let cp_tr = vec2<f32>(cp_new_right_x, cp_new_top_y);
        let cp_bl = vec2<f32>(cp_new_left_x, cp_new_bottom_y);
        let cp_br = vec2<f32>(cp_new_right_x, cp_new_bottom_y);

        let move_vec = center_cc - center_cp;
        let s = sign(move_vec);

        // dot products for each corner, determining alignment with movement direction
        let dot_tl = dot(vec2<f32>(-1.0, 1.0), s);
        let dot_tr = dot(vec2<f32>(1.0, 1.0), s);
        let dot_bl = dot(vec2<f32>(-1.0, -1.0), s);
        let dot_br = dot(vec2<f32>(1.0, -1.0), s);

        // assign durations based on dot products
        let dur_tl = duration_from_dot(dot_tl, DURATION_LEAD, DURATION_SIDE, DURATION_TRAIL);
        let dur_tr = duration_from_dot(dot_tr, DURATION_LEAD, DURATION_SIDE, DURATION_TRAIL);
        let dur_bl = duration_from_dot(dot_bl, DURATION_LEAD, DURATION_SIDE, DURATION_TRAIL);
        let dur_br = duration_from_dot(dot_br, DURATION_LEAD, DURATION_SIDE, DURATION_TRAIL);

        // check direction of horizontal movement
        let is_moving_right = step(0.5, s.x);
        let is_moving_left = step(0.5, -s.x);

        // calculate vertical-rail durations
        let dot_right_edge = (dot_tr + dot_br) * 0.5;
        let dur_right_rail = duration_from_dot(dot_right_edge, DURATION_LEAD, DURATION_SIDE, DURATION_TRAIL);

        let dot_left_edge = (dot_tl + dot_bl) * 0.5;
        let dur_left_rail = duration_from_dot(dot_left_edge, DURATION_LEAD, DURATION_SIDE, DURATION_TRAIL);

        let final_dur_tl = mix(dur_tl, dur_left_rail, is_moving_left);
        let final_dur_bl = mix(dur_bl, dur_left_rail, is_moving_left);

        let final_dur_tr = mix(dur_tr, dur_right_rail, is_moving_right);
        let final_dur_br = mix(dur_br, dur_right_rail, is_moving_right);

        // calculate progress for each corner based on the duration and time since cursor change
        let prog_tl = ease(clamp(base_progress / final_dur_tl, 0.0, 1.0));
        let prog_tr = ease(clamp(base_progress / final_dur_tr, 0.0, 1.0));
        let prog_bl = ease(clamp(base_progress / final_dur_bl, 0.0, 1.0));
        let prog_br = ease(clamp(base_progress / final_dur_br, 0.0, 1.0));

        // get the trial corner positions based on progress
        let v_tl = mix(cp_tl, cc_tl, prog_tl);
        let v_tr = mix(cp_tr, cc_tr, prog_tr);
        let v_br = mix(cp_br, cc_br, prog_br);
        let v_bl = mix(cp_bl, cc_bl, prog_bl);

        // DRAWING THE TRAIL
        let sdf_trail = sdf_convex_quad(vu, v_tl, v_tr, v_br, v_bl);

        // --- FADE GRADIENT CALCULATION ---
        let frag_vec = vu - center_cp;

        // project fragment onto movement vector, normalize to [0, 1]
        // 0.0 at tail, 1.0 at head
        // tiny epsilon to avoid division by zero if moveVec is (0,0)
        let fade_progress = clamp(dot(frag_vec, move_vec) / (dot(move_vec, move_vec) + 1e-6), 0.0, 1.0);

        var trail = cursor_color();

        // The original computes a diagonal-only blur for BLUR < 2.5 but stores it
        // in a shadowed local, so BLUR always applies. Kept that way.
        let effective_blur = BLUR;
        let shape_alpha = antialiasing(sdf_trail, effective_blur); // shape mask

        if FADE_ENABLED > 0.5 {
            // apply fade gradient along the trail
            let eased_progress = pow(fade_progress, FADE_EXPONENT);
            trail.a = trail.a * eased_progress;
        }

        let final_alpha = trail.a * shape_alpha;

        // new_color.a to preserve the background alpha (premultiplied here).
        new_color = mix(new_color, vec4<f32>(trail.rgb * new_color.a, new_color.a), final_alpha);

        // punch hole on the trail, so current cursor is drawn on top
        new_color = mix(new_color, frag_color, step(sdf_current_cursor, 0.0));
    }

    return new_color;
}
