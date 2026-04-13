@fragment fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
  let uv = in.uv;
  let t = u.time;
  let center = vec2<f32>(u.x, u.y);
  let d = uv - center;
  let dist = length(d);

  // Multiple particle rings expanding from center
  var brightness = 0.0;
  for (var i = 0; i < 6; i = i + 1) {
    let phase = t * 0.8 + f32(i) * 1.047;  // staggered
    let ring_r = fract(phase) * 0.5;
    let ring_w = 0.015 + fract(phase) * 0.02;
    let ring = smoothstep(ring_w, 0.0, abs(dist - ring_r)) * (1.0 - fract(phase));
    brightness += ring;
  }

  // Core glow at center
  let glow = 0.04 / (dist * dist + 0.01);
  brightness += clamp(glow, 0.0, 2.0);

  // Sparkle particles orbiting
  for (var j = 0; j < 12; j = j + 1) {
    let a = f32(j) * 0.5236 + t * (0.5 + f32(j % 3) * 0.3);
    let r = 0.1 + 0.15 * sin(t * 0.7 + f32(j));
    let pp = center + vec2<f32>(cos(a), sin(a)) * r;
    let pd = length(uv - pp);
    brightness += 0.002 / (pd * pd + 0.0004);
  }

  // Color: core is white/warm, edges are tinted by u.color
  let core_color = vec3<f32>(1.0, 0.95, 0.8);
  let edge_color = vec3<f32>(u.color_r, u.color_g, u.color_b);
  let color_mix = clamp(dist * 4.0, 0.0, 1.0);
  let col = mix(core_color, edge_color, color_mix) * clamp(brightness, 0.0, 1.5);

  return vec4<f32>(col, 1.0);
}
