// What an output shows when it is not carrying a region.
//
// These are not decoration. An SDI output that goes dark is an output the
// switcher downstream has to re-lock, so an idle Kestrel output carries a real,
// legal picture at the full frame rate — it simply has nothing on it.

struct FillU {
    // 0 = black, 1 = colour bars.
    kind: u32,
    _p0: u32,
    _p1: u32,
    _p2: u32,
};
@group(0) @binding(0) var<uniform> u: FillU;

@fragment
fn fs_fill(in: VsOut) -> @location(0) vec4<f32> {
    if u.kind == 1u {
        // EBU 75% bars, in the order a vectorscope expects them.
        let bar = i32(in.uv.x * 8.0);
        let c = 0.75;
        var rgb: vec3<f32>;
        switch bar {
            case 0: { rgb = vec3<f32>(c, c, c); }
            case 1: { rgb = vec3<f32>(c, c, 0.0); }
            case 2: { rgb = vec3<f32>(0.0, c, c); }
            case 3: { rgb = vec3<f32>(0.0, c, 0.0); }
            case 4: { rgb = vec3<f32>(c, 0.0, c); }
            case 5: { rgb = vec3<f32>(c, 0.0, 0.0); }
            case 6: { rgb = vec3<f32>(0.0, 0.0, c); }
            default: { rgb = vec3<f32>(0.0, 0.0, 0.0); }
        }
        return vec4<f32>(rgb, 1.0);
    }

    // Black. RGB zero here, which the pack turns into Y=16 — *legal* black on
    // SDI. Writing zero all the way through to the wire would be super-black,
    // which some switchers clamp and some flag as an error.
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
