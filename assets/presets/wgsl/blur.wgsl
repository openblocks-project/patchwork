// Blur — 9-tap gaussian on Input A. Cheap, single pass, looks soft and clean.
//
// Uniforms:
//   u.radius  — sample offset in UV space (default ~0.005)
//   u.amount  — 0 = original, 1 = full blur (default 1.0)

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let r = max(u.radius, 0.0);
    let amt = clamp(u.amount, 0.0, 1.0);

    // 3x3 gaussian kernel weights (sum = 16).
    let w0: f32 = 4.0; // center
    let w1: f32 = 2.0; // edges
    let w2: f32 = 1.0; // corners
    let inv_sum: f32 = 1.0 / 16.0;

    let o = vec2<f32>(r, r);

    var acc = vec3<f32>(0.0);
    acc = acc + textureSample(image_a, img_sampler, in.uv).rgb * w0;

    acc = acc + textureSample(image_a, img_sampler, in.uv + vec2<f32>( o.x,  0.0)).rgb * w1;
    acc = acc + textureSample(image_a, img_sampler, in.uv + vec2<f32>(-o.x,  0.0)).rgb * w1;
    acc = acc + textureSample(image_a, img_sampler, in.uv + vec2<f32>( 0.0,  o.y)).rgb * w1;
    acc = acc + textureSample(image_a, img_sampler, in.uv + vec2<f32>( 0.0, -o.y)).rgb * w1;

    acc = acc + textureSample(image_a, img_sampler, in.uv + vec2<f32>( o.x,  o.y)).rgb * w2;
    acc = acc + textureSample(image_a, img_sampler, in.uv + vec2<f32>(-o.x,  o.y)).rgb * w2;
    acc = acc + textureSample(image_a, img_sampler, in.uv + vec2<f32>( o.x, -o.y)).rgb * w2;
    acc = acc + textureSample(image_a, img_sampler, in.uv + vec2<f32>(-o.x, -o.y)).rgb * w2;

    let blurred = acc * inv_sum;
    let original = textureSample(image_a, img_sampler, in.uv).rgb;
    let col = mix(original, blurred, amt);
    return vec4<f32>(col, 1.0);
}
