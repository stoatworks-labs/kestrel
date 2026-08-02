// RGB -> UYVY (4:2:2), straight into the layout DeckLink wants.
//
// The target is a half-width `Rgba8Unorm` texture, so one texel is one
// macropixel and a row of it is *exactly* `width * 2` bytes — the row size
// `bmdFormat8BitYUV` expects. Doing the pack on the GPU rather than on the way
// out of the readback buffer is what keeps the CPU out of the per-pixel path
// entirely: it only ever memcpys rows.

@group(0) @binding(0) var src: texture_2d<f32>;

struct PackU {
    // Width of the *full* raster in pixels.
    width: u32,
    _p0: u32,
    _p1: u32,
    _p2: u32,
};
@group(0) @binding(1) var<uniform> u: PackU;

@fragment
fn fs_pack(in: VsOut) -> @location(0) vec4<f32> {
    let mx = vec2<i32>(in.pos.xy);
    let x0 = mx.x * 2;
    let x1 = min(x0 + 1, i32(u.width) - 1);

    let a = rgb_to_ycbcr(textureLoad(src, vec2<i32>(x0, mx.y), 0).rgb);
    let b = rgb_to_ycbcr(textureLoad(src, vec2<i32>(x1, mx.y), 0).rgb);

    // Box-average the pair for chroma rather than dropping the odd sample.
    // Half the chroma resolution is the format's price; throwing away the
    // second sample as well would be paying it twice.
    let cb = (a.y + b.y) * 0.5;
    let cr = (a.z + b.z) * 0.5;

    return vec4<f32>(cb, a.x, cr, b.x);
}
