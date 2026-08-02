// The actual job: take a rectangle of the decoded input and put it on an
// output raster, magnified.

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

// `xy` is the origin, `zw` the size. Packed as vec4 so the block is 16-byte
// aligned throughout and matches the Rust struct byte for byte; the tail is
// padded with scalars, never a `vec3<u32>` (which is itself 16-aligned and
// would silently resize the block).
struct CropU {
    // The region, in 0..1 of the source.
    src_rect: vec4<f32>,
    // Where it lands, in 0..1 of the output. Smaller than the whole output
    // under `fit`, which is what puts the bars there.
    dst_rect: vec4<f32>,
    src_px: vec2<f32>,
    dst_px: vec2<f32>,
    // 0 = bilinear, 1 = Catmull-Rom bicubic.
    mode: u32,
    _p0: u32,
    _p1: u32,
    _p2: u32,
};
@group(0) @binding(2) var<uniform> u: CropU;

// Catmull-Rom basis. Interpolating (passes through the samples) and mildly
// sharpening, which is what a 2x blow-up of a stage wide-shot wants.
fn cr_weights(t: f32) -> vec4<f32> {
    let t2 = t * t;
    let t3 = t2 * t;
    return vec4<f32>(
        -0.5 * t3 + t2 - 0.5 * t,
        1.5 * t3 - 2.5 * t2 + 1.0,
        -1.5 * t3 + 2.0 * t2 + 0.5 * t,
        0.5 * t3 - 0.5 * t2,
    );
}

fn sample_bicubic(uv: vec2<f32>) -> vec3<f32> {
    let size = vec2<i32>(u.src_px);
    let p = uv * u.src_px - 0.5;
    let base = floor(p);
    let f = p - base;
    let wx = cr_weights(f.x);
    let wy = cr_weights(f.y);
    let b = vec2<i32>(base);

    // Sixteen explicit taps. The nine-bilinear-sample trick gives the same
    // answer for a third of the fetches, but it is fiddly to get exactly right
    // and this is legible; if a big raster ever needs the speed, that is the
    // change to make and there are readback tests here to prove it did not
    // change the picture.
    var acc = vec3<f32>(0.0);
    for (var j = 0; j < 4; j = j + 1) {
        let sy = clamp(b.y + j - 1, 0, size.y - 1);
        var row = vec3<f32>(0.0);
        for (var i = 0; i < 4; i = i + 1) {
            let sx = clamp(b.x + i - 1, 0, size.x - 1);
            row = row + textureLoad(src, vec2<i32>(sx, sy), 0).rgb * wx[i];
        }
        acc = acc + row * wy[j];
    }
    // Catmull-Rom overshoots at a hard edge. Left alone the ringing becomes
    // out-of-range Y and C after the pack, which is illegal on SDI, so it is
    // clamped here rather than wrapped later.
    return clamp(acc, vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_crop(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.pos.xy / u.dst_px;

    let lo = u.dst_rect.xy;
    let hi = u.dst_rect.xy + u.dst_rect.zw;
    if uv.x < lo.x || uv.y < lo.y || uv.x >= hi.x || uv.y >= hi.y {
        // The bars. Legal black, not zero — see the note in `fill.wgsl`.
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let t = (uv - u.dst_rect.xy) / u.dst_rect.zw;
    let src_uv = u.src_rect.xy + t * u.src_rect.zw;

    var rgb: vec3<f32>;
    if u.mode == 1u {
        rgb = sample_bicubic(src_uv);
    } else {
        rgb = textureSampleLevel(src, samp, src_uv, 0.0).rgb;
    }
    return vec4<f32>(rgb, 1.0);
}
