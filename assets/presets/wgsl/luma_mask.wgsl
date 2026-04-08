// Luma Mask — use Input B's brightness as a mask for Input A. Bright parts
// of B reveal A; dark parts fade to a background color. Great for compositing
// shapes (B = mask, A = content), masking video with a shader, or punching
// edge-detect output through a camera feed.
//
// Uniforms:
//   u.bg         — fill color where B is dark
//   u.threshold  — luma cutoff (default 0.5)
//   u.softness   — edge softness around the threshold (default 0.15)
//   u.invert     — 0 = bright B reveals A, 1 = dark B reveals A
//   u.mix        — 0 = pure A, 1 = fully masked (default 1.0)

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let a = textureSample(image_a, img_sampler, in.uv).rgb;
    let b = textureSample(image_b, img_sampler, in.uv).rgb;
    let bg = vec3<f32>(u.bg_r, u.bg_g, u.bg_b);

    var l = luma(b);
    if (u.invert > 0.5) { l = 1.0 - l; }

    let edge = max(u.softness, 0.0001);
    let lo = u.threshold - edge;
    let hi = u.threshold + edge;
    let mask = smoothstep(lo, hi, l);

    let masked = mix(bg, a, mask);
    let col = mix(a, masked, clamp(u.mix, 0.0, 1.0));
    return vec4<f32>(col, 1.0);
}
