// Flow Displace — sample Input A, but offset each pixel's UV by Input B's
// brightness and color channels. B becomes a "flow map": its red/green
// channels push A in x/y, blue gives a brightness boost. Try wiring a
// noise / camera / shader output as B and see Input A liquefy.
//
// Uniforms:
//   u.amount   — displacement strength in UV space (default 0.05)
//   u.rotation — rotates the flow direction        (default 0)
//   u.mix      — 0 = pure A, 1 = fully displaced   (default 1.0)

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let flow = textureSample(image_b, img_sampler, in.uv).rgb;
    // Recenter flow so 0.5 = no offset.
    let off = (flow.rg - vec2<f32>(0.5)) * 2.0 * u.amount;

    // Optional rotation of the flow vector.
    let s = sin(u.rotation);
    let c = cos(u.rotation);
    let off_rot = vec2<f32>(c * off.x - s * off.y, s * off.x + c * off.y);

    let uv = clamp(in.uv + off_rot, vec2<f32>(0.0), vec2<f32>(1.0));
    let a_orig = textureSample(image_a, img_sampler, in.uv).rgb;
    let a_warp = textureSample(image_a, img_sampler, uv).rgb;

    // Brightness boost from B.b — gives a wet/glossy highlight where B is bright.
    let bright = 1.0 + flow.b * 0.4;
    let col = mix(a_orig, a_warp * bright, clamp(u.mix, 0.0, 1.0));
    return vec4<f32>(col, 1.0);
}
