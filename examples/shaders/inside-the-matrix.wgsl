// Walk through a 3D hallway of matrix rain behind dark terminal parts.
/*
  Feel free to do anything you want with this code.
  This shader uses "runes" code by FabriceNeyret2 (https://www.shadertoy.com/view/4ltyDM)
  which is based on "runes" by otaviogood (https://shadertoy.com/view/MsXSRn).
  These random runes look good as matrix symbols and have acceptable performance.

  @pkazmier modified this shader to work in Ghostty.
*/
// Ported to WGSL for tron from https://github.com/hackr-sh/ghostty-shaders/blob/main/inside-the-matrix.glsl
//
// Port note: Ghostty does not support iMouse (always zero), so the mouse look
// branch reads the MOUSE constant below instead.

const ITERATIONS: i32 = 40;   //use less value if you need more performance
const SPEED: f32 = 0.5;

const STRIP_CHARS_MIN: f32 = 7.0;
const STRIP_CHARS_MAX: f32 = 40.0;
const STRIP_CHAR_HEIGHT: f32 = 0.15;
const STRIP_CHAR_WIDTH: f32 = 0.10;
const ZCELL_SIZE: f32 = 1.0 * (STRIP_CHAR_HEIGHT * STRIP_CHARS_MAX);  //the multiplier can't be less than 1.
const XYCELL_SIZE: f32 = 12.0 * STRIP_CHAR_WIDTH;  //the multiplier can't be less than 1.

const BLOCK_SIZE: i32 = 10;  //in cells
const BLOCK_GAP: i32 = 2;    //in cells

const WALK_SPEED: f32 = 0.5 * XYCELL_SIZE;
const BLOCKS_BEFORE_TURN: f32 = 3.0;

const PI: f32 = 3.14159265359;

// Stand-in for Shadertoy's iMouse, which Ghostty leaves at zero.
const MOUSE: vec4<f32> = vec4<f32>(0.0);

// GLSL mod(): the result has the sign of y.
fn glsl_mod(x: f32, y: f32) -> f32 {
    return x - y * floor(x / y);
}

//        ----  random  ----

fn hash(v: f32) -> f32 {
    return fract(sin(v) * 43758.5453123);
}

fn hash_vec2(v: vec2<f32>) -> f32 {
    return hash(dot(v, vec2<f32>(5.3983, 5.4427)));
}

fn hash2(v: vec2<f32>) -> vec2<f32> {
    let w = v * mat2x2<f32>(127.1, 311.7, 269.5, 183.3);
    return fract(sin(w) * 43758.5453123);
}

fn hash4_vec2(v: vec2<f32>) -> vec4<f32> {
    let p = v * mat4x2<f32>(127.1, 311.7,
                            269.5, 183.3,
                            113.5, 271.9,
                            246.1, 124.6);
    return fract(sin(p) * 43758.5453123);
}

fn hash4_vec3(v: vec3<f32>) -> vec4<f32> {
    let p = v * mat4x3<f32>(127.1, 311.7, 74.7,
                            269.5, 183.3, 246.1,
                            113.5, 271.9, 124.6,
                            271.9, 269.5, 311.7);
    return fract(sin(p) * 43758.5453123);
}

//        ----  symbols  ----
//  Slightly modified version of "runes" by FabriceNeyret2 -  https://www.shadertoy.com/view/4ltyDM
//  Which is based on "runes" by otaviogood -  https://shadertoy.com/view/MsXSRn

fn rune_line(p_in: vec2<f32>, a: vec2<f32>, b_in: vec2<f32>) -> f32 {   // from https://www.shadertoy.com/view/4dcfW8
    let p = p_in - a;
    let b = b_in - a;
    let h = clamp(dot(p, b) / dot(b, b), 0.0, 1.0);   // proj coord on line
    return length(p - b * h);                         // dist to segment
}

fn rune(U: vec2<f32>, seed_in: vec2<f32>, highlight: f32) -> f32 {
    var seed = seed_in;
    var d = 1e5;
    for (var i: i32 = 0; i < 4; i++) { // number of strokes
        var pos = hash4_vec2(seed);
        seed += 1.0;

        // each rune touches the edge of its box on all 4 sides
        if i == 0 { pos.y = 0.0; }
        if i == 1 { pos.x = 0.999; }
        if i == 2 { pos.x = 0.0; }
        if i == 3 { pos.y = 0.999; }
        // snap the random line endpoints to a grid 2x3
        let snaps = vec4<f32>(2.0, 3.0, 2.0, 3.0);
        pos = (floor(pos * snaps) + 0.5) / snaps;

        if any(pos.xy != pos.zw) {  //filter out single points (when start and end are the same)
            d = min(d, rune_line(U, pos.xy, pos.zw + 0.001)); // closest line
        }
    }
    return smoothstep(0.1, 0.0, d) + highlight * smoothstep(0.4, 0.0, d);
}

fn random_char(outer: vec2<f32>, inner: vec2<f32>, highlight: f32) -> f32 {
    let seed = vec2<f32>(dot(outer, vec2<f32>(269.5, 183.3)), dot(outer, vec2<f32>(113.5, 271.9)));
    return rune(inner, seed, highlight);
}

//        ----  digital rain  ----

// xy - horizontal, z - vertical
fn rain(ro3: vec3<f32>, rd3: vec3<f32>, time: f32) -> vec3<f32> {
    var result = vec4<f32>(0.0);

    // normalized 2d projection
    let ro2 = ro3.xy;
    let rd2 = normalize(rd3.xy);

    // we use formulas `ro3 + rd3 * t3` and `ro2 + rd2 * t2`, `t3_to_t2` is a multiplier to convert t3 to t2
    let prefer_dx = abs(rd2.x) > abs(rd2.y);
    let t3_to_t2 = select(rd3.y / rd2.y, rd3.x / rd2.x, prefer_dx);

    // at first, horizontal space (xy) is divided into cells (which are columns in 3D)
    // then each xy-cell is divided into vertical cells (along z) - each of these cells contains one raindrop

    let cell_side = vec3<i32>(step(vec3<f32>(0.0), rd3));      //for positive rd.x use cell side with higher x (1) as the next side, for negative - with lower x (0), the same for y and z
    let cell_shift = vec3<i32>(sign(rd3));         //shift to move to the next cell

    //  move through xy-cells in the ray direction
    var t2 = 0.0;  // the ray formula is: ro2 + rd2 * t2, where t2 is positive as the ray has a direction.
    var next_cell = vec2<i32>(floor(ro2 / XYCELL_SIZE));  //first cell index where ray origin is located
    for (var i: i32 = 0; i < ITERATIONS; i++) {
        let cell = next_cell;  //save cell value before changing
        let t2s = t2;          //and t

        //  find the intersection with the nearest side of the current xy-cell (since we know the direction, we only need to check one vertical side and one horizontal side)
        let side = vec2<f32>(next_cell + cell_side.xy) * XYCELL_SIZE;  //side.x is x coord of the y-axis side, side.y - y of the x-axis side
        let t2_side = (side - ro2) / rd2;  // t2_side.x and t2_side.y are two candidates for the next value of t2, we need the nearest
        if t2_side.x < t2_side.y {
            t2 = t2_side.x;
            next_cell.x += cell_shift.x;  //cross through the y-axis side
        } else {
            t2 = t2_side.y;
            next_cell.y += cell_shift.y;  //cross through the x-axis side
        }
        //now t2 is the value of the end point in the current cell (and the same point is the start value in the next cell)

        //  gap cells
        let cell_in_block = fract(vec2<f32>(cell) / f32(BLOCK_SIZE));
        let gap = f32(BLOCK_GAP) / f32(BLOCK_SIZE);
        if cell_in_block.x < gap || cell_in_block.y < gap || (cell_in_block.x < (gap + 0.1) && cell_in_block.y < (gap + 0.1)) {
            continue;
        }

        //  return to 3d - we have start and end points of the ray segment inside the column (t3s and t3e)
        let t3s = t2s / t3_to_t2;

        //  move through z-cells of the current column in the ray direction (don't need much to check, two nearest cells are enough)
        let pos_z = ro3.z + rd3.z * t3s;
        let xycell_hash = hash_vec2(vec2<f32>(cell));
        var z_shift = xycell_hash * 11.0 - time * (0.5 + xycell_hash * 1.0 + xycell_hash * xycell_hash * 1.0 + pow(xycell_hash, 16.0) * 3.0);  //a different z shift for each xy column
        let char_z_shift = floor(z_shift / STRIP_CHAR_HEIGHT);
        z_shift = char_z_shift * STRIP_CHAR_HEIGHT;
        var zcell = i32(floor((pos_z - z_shift) / ZCELL_SIZE));  //z-cell index
        for (var j: i32 = 0; j < 2; j++) {  //2 iterations is enough if camera doesn't look much up or down
            //  calcaulate coordinates of the target (raindrop)
            let cell_hash = hash4_vec3(vec3<f32>(vec3<i32>(cell, zcell)));
            let cell_hash2 = fract(cell_hash * vec4<f32>(127.1, 311.7, 271.9, 124.6));

            let chars_count = cell_hash.w * (STRIP_CHARS_MAX - STRIP_CHARS_MIN) + STRIP_CHARS_MIN;
            let target_length = chars_count * STRIP_CHAR_HEIGHT;
            let target_rad = STRIP_CHAR_WIDTH / 2.0;
            let target_z = (f32(zcell) * ZCELL_SIZE + z_shift) + cell_hash.z * (ZCELL_SIZE - target_length);
            let target_xy = vec2<f32>(cell) * XYCELL_SIZE + target_rad + cell_hash.xy * (XYCELL_SIZE - target_rad * 2.0);

            //  We have a line segment (t0,t). Now calculate the distance between line segment and cell target (it's easier in 2d)
            let s = target_xy - ro2;
            let tmin = dot(s, rd2);  //tmin - point with minimal distance to target
            if tmin >= t2s && tmin <= t2 {
                var u = s.x * rd2.y - s.y * rd2.x;  //horizontal coord in the matrix strip
                if abs(u) < target_rad {
                    u = (u / target_rad + 1.0) / 2.0;
                    let z = ro3.z + rd3.z * tmin / t3_to_t2;
                    let v = (z - target_z) / target_length;  //vertical coord in the matrix strip
                    if v >= 0.0 && v < 1.0 {
                        let c = floor(v * chars_count);  //symbol index relative to the start of the strip, with addition of char_z_shift it becomes an index relative to the whole cell
                        let q = fract(v * chars_count);
                        let char_hash = hash2(vec2<f32>(c + char_z_shift, cell_hash2.x));
                        if char_hash.x >= 0.1 || c == 0.0 {  //10% of missed symbols
                            let time_factor = floor(select(
                                time * (1.0 * cell_hash2.z +   //strips are changed sometime with different speed
                                        cell_hash2.w * cell_hash2.w * 4.0 * pow(char_hash.y, 4.0)),  //some symbols in some strips are changed relatively often
                                time,  //first symbol is changed fast
                                c == 0.0));
                            var a = random_char(vec2<f32>(char_hash.x, time_factor), vec2<f32>(u, q), max(1.0, 3.0 - c / 2.0) * 0.2);  //alpha
                            a *= clamp((chars_count - 0.5 - c) / 2.0, 0.0, 1.0);  //tail fade
                            if a > 0.0 {
                                let attenuation = 1.0 + pow(0.06 * tmin / t3_to_t2, 2.0);
                                let col = select(vec3<f32>(0.25, 0.80, 0.40), vec3<f32>(0.67, 1.0, 0.82), c == 0.0) / attenuation;
                                let a1 = result.a;
                                result.a = a1 + (1.0 - a1) * a;
                                result = vec4<f32>((result.xyz * a1 + col * (1.0 - a1) * a) / result.a, result.a);
                                if result.a > 0.98 {
                                    return result.xyz;
                                }
                            }
                        }
                    }
                }
            }
            // not found in this cell - go to next vertical cell
            zcell += cell_shift.z;
        }
        // go to next horizontal cell
    }

    return result.xyz * result.a;
}

//        ----  main, camera  ----

fn rotate(v: vec2<f32>, a: f32) -> vec2<f32> {
    let s = sin(a);
    let c = cos(a);
    let m = mat2x2<f32>(c, -s, s, c);
    return m * v;
}

fn rotateX(v: vec3<f32>, a: f32) -> vec3<f32> {
    let s = sin(a);
    let c = cos(a);
    return mat3x3<f32>(1.0, 0.0, 0.0, 0.0, c, -s, 0.0, s, c) * v;
}

fn rotateY(v: vec3<f32>, a: f32) -> vec3<f32> {
    let s = sin(a);
    let c = cos(a);
    return mat3x3<f32>(c, 0.0, -s, 0.0, 1.0, 0.0, s, 0.0, c) * v;
}

fn rotateZ(v: vec3<f32>, a: f32) -> vec3<f32> {
    let s = sin(a);
    let c = cos(a);
    return mat3x3<f32>(c, -s, 0.0, s, c, 0.0, 0.0, 0.0, 1.0) * v;
}

fn smoothstep1(x: f32) -> f32 {
    return smoothstep(0.0, 1.0, x);
}

const turn_rad: f32 = 0.25 / BLOCKS_BEFORE_TURN;   //0 .. 0.5
const turn_abs_time: f32 = (PI / 2.0 * turn_rad) * 1.5;  //multiplier different than 1 means a slow down on turns
const turn_time: f32 = turn_abs_time / (1.0 - 2.0 * turn_rad + turn_abs_time);  //0..1, but should be <= 0.5

const first_turn_look_angle: f32 = 0.4;
const second_turn_drift_angle: f32 = 0.5;
const fifth_turn_drift_angle: f32 = 0.25;

fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    if STRIP_CHAR_WIDTH > XYCELL_SIZE || STRIP_CHAR_HEIGHT * STRIP_CHARS_MAX > ZCELL_SIZE {
        // error
        return vec4<f32>(1.0, 0.0, 0.0, 1.0);
    }

    let time = glsl_mod(tron.time, 300.0) * SPEED; //reset time every 5 minutes, as large values lead to the same (and eventually no) rune(s)

    let level1_size = f32(BLOCK_SIZE) * BLOCKS_BEFORE_TURN * XYCELL_SIZE;
    let level2_size = 4.0 * level1_size;
    let gap_size = f32(BLOCK_GAP) * XYCELL_SIZE;

    var ro = vec3<f32>(gap_size / 2.0, gap_size / 2.0, 0.0);
    var rd = vec3<f32>(uv.x, 2.0, uv.y);

    let tq = fract(time / (level2_size * 4.0) * WALK_SPEED);  //the whole cycle time counter
    let t8 = fract(tq * 4.0);  //time counter while walking on one of the four big sides
    var t1 = fract(t8 * 8.0);  //time counter while walking on one of the eight sides of the big side

    var prev: vec2<f32>;
    var dir: vec2<f32>;
    if tq < 0.25 {
        prev = vec2<f32>(0.0, 0.0);
        dir = vec2<f32>(0.0, 1.0);
    } else if tq < 0.5 {
        prev = vec2<f32>(0.0, 1.0);
        dir = vec2<f32>(1.0, 0.0);
    } else if tq < 0.75 {
        prev = vec2<f32>(1.0, 1.0);
        dir = vec2<f32>(0.0, -1.0);
    } else {
        prev = vec2<f32>(1.0, 0.0);
        dir = vec2<f32>(-1.0, 0.0);
    }
    var angle = floor(tq * 4.0);  //0..4 wich means 0..2*PI

    prev *= 4.0;

    var turn: vec2<f32>;
    var turn_sign = 0.0;
    let dirL = rotate(dir, -PI / 2.0);
    let dirR = -dirL;
    var up_down = 0.0;
    var rotate_on_turns = 1.0;
    var roll_on_turns = 1.0;
    var add_angel = 0.0;
    if t8 < 0.125 {
        turn = dirL;
        //dir = dir;
        turn_sign = -1.0;
        angle -= first_turn_look_angle * (max(0.0, t1 - (1.0 - turn_time * 2.0)) / turn_time - max(0.0, t1 - (1.0 - turn_time)) / turn_time * 2.5);
        roll_on_turns = 0.0;
    } else if t8 < 0.250 {
        prev += dir;
        turn = dir;
        dir = dirL;
        angle -= 1.0;
        turn_sign = 1.0;
        add_angel += first_turn_look_angle * 0.5 + (-first_turn_look_angle * 0.5 + 1.0 + second_turn_drift_angle) * t1;
        rotate_on_turns = 0.0;
        roll_on_turns = 0.0;
    } else if t8 < 0.375 {
        prev += dir + dirL;
        turn = dirR;
        //dir = dir;
        turn_sign = 1.0;
        add_angel += second_turn_drift_angle * sqrt(1.0 - t1);
        //roll_on_turns = 0.;
    } else if t8 < 0.5 {
        prev += dir + dir + dirL;
        turn = dirR;
        dir = dirR;
        angle += 1.0;
        turn_sign = 0.0;
        up_down = sin(t1 * PI) * 0.37;
    } else if t8 < 0.625 {
        prev += dir + dir;
        turn = dir;
        dir = dirR;
        angle += 1.0;
        turn_sign = -1.0;
        up_down = sin(-min(1.0, t1 / (1.0 - turn_time)) * PI) * 0.37;
    } else if t8 < 0.750 {
        prev += dir + dir + dirR;
        turn = dirL;
        //dir = dir;
        turn_sign = -1.0;
        add_angel -= (fifth_turn_drift_angle + 1.0) * smoothstep1(t1);
        rotate_on_turns = 0.0;
        roll_on_turns = 0.0;
    } else if t8 < 0.875 {
        prev += dir + dir + dir + dirR;
        turn = dir;
        dir = dirL;
        angle -= 1.0;
        turn_sign = 1.0;
        add_angel -= fifth_turn_drift_angle - smoothstep1(t1) * (fifth_turn_drift_angle * 2.0 + 1.0);
        rotate_on_turns = 0.0;
        roll_on_turns = 0.0;
    } else {
        prev += dir + dir + dir;
        turn = dirR;
        //dir = dir;
        turn_sign = 1.0;
        angle += fifth_turn_drift_angle * (1.5 * min(1.0, (1.0 - t1) / turn_time) - 0.5 * smoothstep1(1.0 - min(1.0, t1 / (1.0 - turn_time))));
    }

    if MOUSE.x > 10.0 || MOUSE.y > 10.0 {
        let mouse = MOUSE.xy / tron.resolution * 2.0 - 1.0;
        up_down = -0.7 * mouse.y;
        angle += mouse.x;
        rotate_on_turns = 1.0;
        roll_on_turns = 0.0;
    } else {
        angle += add_angel;
    }

    rd = rotateX(rd, up_down);

    var p: vec2<f32>;
    if turn_sign == 0.0 {
        //  move forward
        p = prev + dir * (turn_rad + 1.0 * t1);
    } else if t1 > (1.0 - turn_time) {
        //  turn
        let tr = (t1 - (1.0 - turn_time)) / turn_time;
        let c = prev + dir * (1.0 - turn_rad) + turn * turn_rad;
        p = c + turn_rad * rotate(dir, (tr - 1.0) * turn_sign * PI / 2.0);
        angle += tr * turn_sign * rotate_on_turns;
        rd = rotateY(rd, sin(tr * turn_sign * PI) * 0.2 * roll_on_turns);  //roll
    } else {
        //  move forward
        t1 /= (1.0 - turn_time);
        p = prev + dir * (turn_rad + (1.0 - turn_rad * 2.0) * t1);
    }

    rd = rotateZ(rd, angle * PI / 2.0);

    ro = vec3<f32>(ro.xy + level1_size * p, ro.z);

    rd = normalize(rd);
    ro += rd * 0.2;

    // vec3 col = rain(ro, rd, time);
    let col = rain(ro, rd, time) * 0.25;

    // Sample the terminal screen texture including alpha channel
    let terminalColor = terminal(uv);

    // Combine the matrix effect with the terminal color
    // vec3 blendedColor = terminalColor.rgb + col;

    // Make a mask that is 1.0 where the terminal content is not black
    let mask = 1.2 - step(0.5, dot(terminalColor.rgb, vec3<f32>(1.0)));
    let blendedColor = mix(terminalColor.rgb * 1.2, col, vec3<f32>(mask));

    return vec4<f32>(blendedColor, terminalColor.a);
}
