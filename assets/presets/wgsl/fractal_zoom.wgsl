// Fractal Zoom — Mandelbrot deep zoom toward Seahorse Valley with a
// smooth animated palette. Wire a Time node into u.time for continuous
// motion; positive u.zoom_rate zooms in, negative zooms out. The nice
// thing about this preset is that it needs almost nothing to look good:
// drop a Time node into u.time and it'll breathe on its own.
//
// Uniforms:
//   u.time        — seconds (wire a Time node to animate)
//   u.zoom_rate   — exp zoom rate, ±1.0 (default 0.1 — slow breathing zoom)
//   u.iterations  — max Mandelbrot iterations, 32..512 (default 128)
//   u.hue_shift   — palette rotation in radians (default 0.5)
//
// Why Seahorse Valley: (-0.745, 0.112) sits just off the spine of the
// cardioid where the set's filigree detail keeps getting more intricate
// as you zoom — f32 precision holds for ~minutes of continuous zoom.

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let target = vec2<f32>(-0.745, 0.112);
    let aspect = u.resolution.x / max(u.resolution.y, 1.0);
    let zoom   = exp(-u.time * u.zoom_rate);

    // UV → complex plane, aspect-corrected and centred on target.
    let uv = (in.uv - vec2<f32>(0.5, 0.5)) * 3.0 * zoom;
    let c  = target + vec2<f32>(uv.x * aspect, uv.y);

    let max_iter = i32(clamp(u.iterations, 16.0, 1024.0));
    var z: vec2<f32> = vec2<f32>(0.0, 0.0);
    var i: i32 = 0;
    loop {
        if (i >= max_iter) { break; }
        if (dot(z, z) > 4.0) { break; }
        z = vec2<f32>(z.x * z.x - z.y * z.y, 2.0 * z.x * z.y) + c;
        i = i + 1;
    }

    // Inside the set — solid colour so the silhouette reads clean.
    if (i >= max_iter) {
        return vec4<f32>(0.03, 0.03, 0.05, 1.0);
    }

    // Smooth iteration count (Lyapunov-style) kills the banding you get
    // from just `f32(i)`. `max(..., 1.01)` guards log2 against 0.
    let mag2 = max(dot(z, z), 1.01);
    let n    = f32(i) + 1.0 - log2(log2(mag2) * 0.5);

    // Palette — three cosines phase-shifted by 120° give full-hue-circle
    // coverage without any lookup table.
    let t = fract(n * 0.04);
    let h = t * 6.28318 + u.hue_shift;
    let r = 0.5 + 0.5 * cos(h);
    let g = 0.5 + 0.5 * cos(h + 2.09439);
    let b = 0.5 + 0.5 * cos(h + 4.18879);
    return vec4<f32>(r, g, b, 1.0);
}
