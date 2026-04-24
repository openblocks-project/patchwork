// Dense particle swarm orbiting (u.x, u.y). Up to 2000 particles, each
// with its own orbit + lifecycle. Brightness is intentionally low
// per-particle so a few hundred accumulate into a visible swarm without
// blowing out at the high end.
//
// Knobs (auto-detected → sliders + input ports):
//   u.x, u.y            orbit centre                 (0–1)
//   u.particle_count    how many are visible         (0–2000, default 400)
//   u.dot_size          softness of each particle    (0–0.15, default 0.04)
//   u.speed             orbital rate                 (0–10,  default 1)
//   u.spread            orbit radius                 (0–1,   default 0.5)
//   u.swirl             angular bias vs drift        (0–1,   default 0.5)
//   u.color             tint (picker → _r/_g/_b)
//
// Wire a Time node into u.time to animate. The hard cap of 2000 is a
// fragment-shader loop bound — the GPU still has to touch every particle
// for every pixel, so very large viewers will slow down. 400 default is
// a comfortable mid-point on modern hardware.

const MAX_PARTICLES: i32 = 2000;

// Deterministic pseudo-random in [0,1) from an integer seed. Gives each
// particle its own orbit without any random uniforms.
fn hash11(p: f32) -> f32 {
    var x = fract(p * 0.1031);
    x *= x + 33.33;
    x *= x + x;
    return fract(x);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let aspect = u.resolution.x / max(u.resolution.y, 1.0);
    // Aspect-correct so orbits are round on wide screens.
    let uv = vec2<f32>((in.uv.x - 0.5) * aspect + 0.5, in.uv.y);
    let center = vec2<f32>((u.x - 0.5) * aspect + 0.5, u.y);

    let t = u.time;
    let speed = max(u.speed, 0.01);
    let spread = max(u.spread, 0.01);
    let swirl = u.swirl;
    let size = max(u.dot_size, 0.001);
    let visible_n = i32(clamp(u.particle_count, 0.0, f32(MAX_PARTICLES)));

    let tint = vec3<f32>(u.color_r, u.color_g, u.color_b);

    var brightness = 0.0;
    for (var i: i32 = 0; i < MAX_PARTICLES; i = i + 1) {
        if (i >= visible_n) { break; }
        let fi = f32(i);
        // Per-particle constants — stable across frames so each has its
        // own consistent "personality".
        let h1 = hash11(fi);
        let h2 = hash11(fi + 13.7);
        let h3 = hash11(fi + 41.3);

        // Lifecycle: loops 0→1. Staggered so the swarm doesn't pulse.
        // Short life = lots of respawns; long life = slow drifters.
        let life_len = 0.8 + h2 * 3.2;
        let life = fract((t / life_len) + h1);

        // Orbit: each particle's radius, angular speed, and starting
        // angle are all derived from its index. `swirl` biases more
        // rotational motion; low swirl = slower orbit, more wobble.
        let radius = spread * (0.10 + 0.90 * h3) * (1.0 - 0.35 * life);
        let ang_speed = speed * (0.3 + 0.7 * h2) * (0.3 + swirl * 0.9);
        let angle = h1 * 6.2832 + t * ang_speed;

        // Independent wobble per particle so the ring isn't mechanical.
        let wobble = 0.07 * spread * sin(t * (0.8 + h3 * 1.8) + fi);

        let pos = center + vec2<f32>(
            cos(angle) * (radius + wobble),
            sin(angle) * (radius + wobble)
        );

        // Soft dot + bell-curve fade over lifetime (bright mid-life,
        // fading at both ends so respawns feel like natural sparks).
        let d = length(uv - pos);
        let fade = smoothstep(0.0, 0.15, life) * smoothstep(1.0, 0.75, life);
        // Per-particle glow, deliberately low so hundreds add up cleanly
        // without saturating. Users can raise u.dot_size for a denser
        // bloom feel.
        let glow = size * size * 0.22 / (d * d + size * size * 0.12);
        brightness += glow * fade;
    }

    let col = tint * brightness;
    return vec4<f32>(col, 1.0);
}
