// Edge Detect — Sobel filter on Input A's luma. Outputs colored edges over
// a flat background. Looks great on a Camera feed.
//
// Uniforms:
//   u.bg         — background color where there are no edges
//   u.fg         — edge color
//   u.thickness  — edge sample radius in UV space (default 0.003)
//   u.amount     — edge gain / contrast (default 1.0)

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let r = max(u.thickness, 0.0001);
    let amt = max(u.amount, 0.0);

    let tl = luma(textureSample(image_a, img_sampler, in.uv + vec2<f32>(-r, -r)).rgb);
    let t  = luma(textureSample(image_a, img_sampler, in.uv + vec2<f32>( 0.0, -r)).rgb);
    let tr = luma(textureSample(image_a, img_sampler, in.uv + vec2<f32>( r, -r)).rgb);
    let l  = luma(textureSample(image_a, img_sampler, in.uv + vec2<f32>(-r,  0.0)).rgb);
    let rr = luma(textureSample(image_a, img_sampler, in.uv + vec2<f32>( r,  0.0)).rgb);
    let bl = luma(textureSample(image_a, img_sampler, in.uv + vec2<f32>(-r,  r)).rgb);
    let b  = luma(textureSample(image_a, img_sampler, in.uv + vec2<f32>( 0.0,  r)).rgb);
    let br = luma(textureSample(image_a, img_sampler, in.uv + vec2<f32>( r,  r)).rgb);

    // Sobel kernels.
    let gx = -tl - 2.0*l - bl + tr + 2.0*rr + br;
    let gy = -tl - 2.0*t - tr + bl + 2.0*b  + br;

    let mag = clamp(length(vec2<f32>(gx, gy)) * amt, 0.0, 1.0);

    let bg = vec3<f32>(u.bg_r, u.bg_g, u.bg_b);
    let fg = vec3<f32>(u.fg_r, u.fg_g, u.fg_b);
    let col = mix(bg, fg, mag);
    return vec4<f32>(col, 1.0);
}
