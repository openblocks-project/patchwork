// Camera Grade — sample image_a (e.g. a Camera node) and apply a few simple
// color effects: brightness, saturation, tint, and a soft vignette.
//
// Wire any image into "Input A" and pick this preset. All knobs are normal
// uniforms so they show up as sliders / color pickers automatically.
//
// Uniforms:
//   u.brightness  — multiplier on RGB           (default 1.0)
//   u.saturation  — 0 = grayscale, 1 = normal   (default 1.0)
//   u.tint        — color multiplied into RGB   (default white)
//   u.vignette    — 0 = off, 1 = strong         (default 0.5)
//   u.invert      — 0 = off, 1 = full invert    (default 0.0)

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Sample the camera / upstream image.
    var col = textureSample(image_a, img_sampler, in.uv).rgb;

    // Brightness.
    col = col * u.brightness;

    // Saturation: lerp between luma and original.
    let luma = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
    col = mix(vec3<f32>(luma), col, u.saturation);

    // Tint (multiplicative color).
    let tint = vec3<f32>(u.tint_r, u.tint_g, u.tint_b);
    col = col * (tint * 2.0); // *2 so a "white" tint slider at 0.5 leaves it unchanged

    // Optional invert.
    col = mix(col, vec3<f32>(1.0) - col, clamp(u.invert, 0.0, 1.0));

    // Soft vignette around the edges.
    let centered = in.uv - vec2<f32>(0.5);
    let dist = length(centered) * 1.4142; // 0 at center, ~1 at corner
    let vig = 1.0 - clamp(u.vignette, 0.0, 1.0) * smoothstep(0.4, 1.0, dist);
    col = col * vig;

    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
