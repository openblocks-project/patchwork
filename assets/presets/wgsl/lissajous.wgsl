// Lissajous — minimal animated curve. One bg, one fg, ratio, thickness,
// and a speed knob. Cheap enough to drop into a fullscreen theme background.
//
// Uniforms:
//   u.bg         — background color
//   u.fg         — line color
//   u.ratio      — y-frequency / x-frequency  (default 3.0)
//   u.thickness  — line thickness in NDC      (default 0.008)
//   u.speed      — animation rate multiplier  (default 1.0)
//                  Wire a Time node into u.time and turn this up/down,
//                  or leave time unwired and just drive everything from speed.

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let res = u.resolution;
    let aspect = res.x / max(res.y, 1.0);
    var p = (in.uv * 2.0 - vec2<f32>(1.0)) * vec2<f32>(aspect, 1.0);
    p.y = -p.y;

    let bg = vec3<f32>(u.bg_r, u.bg_g, u.bg_b);
    let fg = vec3<f32>(u.fg_r, u.fg_g, u.fg_b);
    let ratio = max(u.ratio, 0.1);
    let thickness = max(u.thickness, 0.001);

    // Asymmetric x/y phases so the curve actively morphs (not just rotates).
    // u.speed scales the wired/internal time so the slider feels responsive.
    let ta = u.time * u.speed;
    let phase_x = ta;
    let phase_y = ta * 0.7;

    let r = 0.85;
    let n: i32 = 96;
    var prev = vec2<f32>(r * sin(phase_x), r * sin(phase_y));
    var min_d: f32 = 1e9;
    for (var i: i32 = 1; i <= n; i = i + 1) {
        let t = f32(i) / f32(n) * 6.28318;
        let cur = vec2<f32>(
            r * sin(ratio * t + phase_x),
            r * sin(t + phase_y)
        );
        let pa = p - prev;
        let ba = cur - prev;
        let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0);
        let d = length(pa - ba * h);
        min_d = min(min_d, d);
        prev = cur;
    }

    let line = 1.0 - smoothstep(thickness * 0.6, thickness, min_d);
    let col = mix(bg, fg, line);
    return vec4<f32>(col, 1.0);
}
