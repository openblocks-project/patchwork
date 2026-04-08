// Julia Set — animated escape-time fractal with two color stops.
//
// Uniforms:
//   u.bg     — background (interior) color
//   u.fg     — foreground (escape) color
//   u.scale  — c-walk radius            (default 0.7)
//   u.speed  — c-walk angular speed     (default 0.4)
//   u.zoom   — viewport zoom            (default 1.0)

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let res = u.resolution;
    let aspect = res.x / max(res.y, 1.0);
    var p = (in.uv * 2.0 - vec2<f32>(1.0)) * vec2<f32>(aspect, 1.0);
    p.y = -p.y;

    let bg = vec3<f32>(u.bg_r, u.bg_g, u.bg_b);
    let fg = vec3<f32>(u.fg_r, u.fg_g, u.fg_b);
    let scale = max(u.scale, 0.05);
    let speed = max(u.speed, 0.0);
    let zoom  = max(u.zoom,  0.05);

    var z = p / zoom;
    let c = vec2<f32>(
        scale * cos(u.time * speed),
        scale * sin(u.time * speed * 1.13)
    );

    var i: i32 = 0;
    let max_iter: i32 = 192;
    var escaped: bool = false;
    loop {
        if (i >= max_iter) { break; }
        let x2 = z.x * z.x;
        let y2 = z.y * z.y;
        if (x2 + y2 > 64.0) { escaped = true; break; }
        z = vec2<f32>(x2 - y2 + c.x, 2.0 * z.x * z.y + c.y);
        i = i + 1;
    }

    if (!escaped) {
        return vec4<f32>(bg, 1.0);
    }

    // Smoothed iteration count for a continuous gradient.
    let log_zn = log(z.x * z.x + z.y * z.y) * 0.5;
    let nu = log(log_zn / log(2.0)) / log(2.0);
    let smoothed = (f32(i) + 1.0 - nu) / f32(max_iter);

    let col = mix(bg, fg, clamp(pow(smoothed, 0.6), 0.0, 1.0));
    return vec4<f32>(col, 1.0);
}
