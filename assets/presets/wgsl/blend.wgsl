// Blend — combine Input A and Input B with several blend modes selectable
// via `u.mode`. Wire two image sources, drag `u.mix` to crossfade, and pick
// a mode integer to change how they combine.
//
// Modes (round to nearest int):
//   0 = mix    (linear crossfade)
//   1 = add    (additive)
//   2 = mult   (multiply)
//   3 = screen (1 - (1-a)(1-b))
//   4 = diff   (abs difference)
//   5 = max    (component-wise max)
//
// Uniforms:
//   u.mix    — 0 = pure A, 1 = pure B (default 0.5)
//   u.mode   — blend mode integer     (default 0)
//   u.amount — strength of the chosen blend vs. plain A (default 1.0)

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let a = textureSample(image_a, img_sampler, in.uv).rgb;
    let b = textureSample(image_b, img_sampler, in.uv).rgb;
    let m = clamp(u.mix, 0.0, 1.0);

    // Default: linear mix.
    var blended = mix(a, b, m);

    let mode = i32(round(u.mode));
    if (mode == 1) {
        blended = a + b * m;
    } else if (mode == 2) {
        blended = mix(a, a * b, m);
    } else if (mode == 3) {
        let scr = vec3<f32>(1.0) - (vec3<f32>(1.0) - a) * (vec3<f32>(1.0) - b);
        blended = mix(a, scr, m);
    } else if (mode == 4) {
        blended = mix(a, abs(a - b), m);
    } else if (mode == 5) {
        blended = mix(a, max(a, b), m);
    }

    let col = mix(a, blended, clamp(u.amount, 0.0, 1.0));
    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
