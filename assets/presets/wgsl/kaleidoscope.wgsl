// Kaleidoscope — radial mirror / wedge symmetry of Input A around a
// user-placed center. Divides the frame into N equal wedges and mirrors
// each one into the previous, so a live camera turns into a mandala.
//
// Uniforms:
//   u.segments  — number of mirrored wedges (2..32)
//   u.rotation  — rotation offset of the whole pattern, in radians
//   u.zoom      — zoom factor on the sampled seed region (0.1..4)
//   u.center_x  — kaleidoscope origin X, normalised 0..1
//   u.center_y  — kaleidoscope origin Y, normalised 0..1

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let center = vec2<f32>(u.center_x, u.center_y);
    let zoom   = max(u.zoom, 0.001);
    let segs   = max(u.segments, 2.0);
    let wedge  = 6.28318530718 / segs;

    let centered = (in.uv - center) / zoom;
    let r        = length(centered);
    var theta    = atan2(centered.y, centered.x);

    // Fold into one wedge; alternate wedges mirror so the seams line up.
    let k = floor(theta / wedge);
    theta = theta - k * wedge;
    if ((i32(k) & 1) != 0) {
        theta = wedge - theta;
    }
    theta = theta + u.rotation;

    let sampled = vec2<f32>(cos(theta), sin(theta)) * r;
    let src_uv  = sampled + center;
    return textureSample(image_a, img_sampler, clamp(src_uv, vec2<f32>(0.0), vec2<f32>(1.0)));
}
