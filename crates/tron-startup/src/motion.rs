//! Easing helpers shared by the terminal side and the shader parameters.

/// Smoothstep from 0 to 1 for `x` in 0..1.
pub fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// A bump that peaks at `at` seconds and lasts `width` seconds.
pub fn pulse(t: f32, at: f32, width: f32) -> f32 {
    (1.0 - ((t - at).abs() / (width / 2.0))).max(0.0)
}

pub fn lerp(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount
}
