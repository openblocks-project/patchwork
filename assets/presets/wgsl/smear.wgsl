// Smear — self-feedback preset.
// Requires Input A in "Last frame" mode (auto-set when spawning via the
// WGSL Presets node). Samples its previous frame, fades it, and paints a
// moving dot on top.
//
// Uniforms:
//   u.dot_color  — RGB color of the moving dot
//   u.tint       — RGB tint multiplier on the faded history (0.5 = identity)
//   u.fade       — fraction kept per frame  (default 0.97)
//   u.speed      — orbit speed              (default 1.0)
//   u.dot_radius — point radius in NDC      (default 0.04)

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let res = u.resolution;
    let aspect = res.x / max(res.y, 1.0);

    let fade   = clamp(u.fade,     0.0, 0.9999);
    let speed  = max(u.speed,      0.0);
    let radius = max(u.dot_radius, 0.001);

    // Map (0..1) tint slider to a 0..2 multiplier so 0.5 == identity.
    let tint = vec3<f32>(u.tint_r, u.tint_g, u.tint_b) * 2.0;
    let dot_rgb = vec3<f32>(u.dot_color_r, u.dot_color_g, u.dot_color_b);

    // Sample previous frame, fade and tint it.
    let prev = textureSample(image_a, img_sampler, in.uv).rgb * fade * tint;

    // Moving point in NDC.
    var p = (in.uv * 2.0 - vec2<f32>(1.0)) * vec2<f32>(aspect, 1.0);
    p.y = -p.y;
    let center = vec2<f32>(
        0.6 * cos(u.time * speed),
        0.6 * sin(u.time * speed * 1.27)
    );
    let d = length(p - center);
    let core = 1.0 - smoothstep(radius * 0.6, radius, d);

    let new_col = dot_rgb * core;
    let col = clamp(prev + new_col, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(col, 1.0);
}
