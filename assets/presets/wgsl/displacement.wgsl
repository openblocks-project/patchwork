// Displacement — wavy / liquid distortion of Input A driven by sin/cos.
// No second input needed; the distortion is procedural and animated by
// `u.time` so it works even with a still image.
//
// Uniforms:
//   u.amount  — displacement strength in UV space (default 0.02)
//   u.scale   — wave frequency multiplier         (default 8.0)
//   u.speed   — animation rate                    (default 1.0)

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let amt = u.amount;
    let freq = max(u.scale, 0.0) * 6.28318;
    let t = u.time * u.speed;

    // Two crossed sinusoids — gives a swimmy / heat-haze look.
    let dx = sin(in.uv.y * freq + t * 1.3) * amt;
    let dy = cos(in.uv.x * freq * 0.7 + t * 1.7) * amt;

    let uv = clamp(in.uv + vec2<f32>(dx, dy), vec2<f32>(0.0), vec2<f32>(1.0));
    let col = textureSample(image_a, img_sampler, uv).rgb;
    return vec4<f32>(col, 1.0);
}
