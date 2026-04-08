// Mandelbrot — slow-breathing classic with two color stops.
//
// Uniforms:
//   u.bg     — background (interior) color
//   u.fg     — foreground (escape) color
//   u.zoom   — viewport zoom            (default 0.6)
//   u.cx     — viewport center x        (default -0.75)
//   u.cy     — viewport center y        (default 0)

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let res = u.resolution;
    let aspect = res.x / max(res.y, 1.0);
    var p = (in.uv * 2.0 - vec2<f32>(1.0)) * vec2<f32>(aspect, 1.0);
    p.y = -p.y;

    let bg = vec3<f32>(u.bg_r, u.bg_g, u.bg_b);
    let fg = vec3<f32>(u.fg_r, u.fg_g, u.fg_b);
    let base_zoom = max(u.zoom, 0.05);
    let cx = u.cx;
    let cy = u.cy;

    // Animated breathe + slow rotation, always alive.
    let zoom = base_zoom * (1.0 + 0.30 * sin(u.time * 0.15));
    let ang  = u.time * 0.03;
    let cs = cos(ang);
    let sn = sin(ang);
    let pr = vec2<f32>(p.x * cs - p.y * sn, p.x * sn + p.y * cs);
    let c = vec2<f32>(cx, cy) + pr / zoom;

    var z = vec2<f32>(0.0, 0.0);
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

    let log_zn = log(z.x * z.x + z.y * z.y) * 0.5;
    let nu = log(log_zn / log(2.0)) / log(2.0);
    let smoothed = (f32(i) + 1.0 - nu) / f32(max_iter);

    let col = mix(bg, fg, clamp(pow(smoothed, 0.6), 0.0, 1.0));
    return vec4<f32>(col, 1.0);
}
