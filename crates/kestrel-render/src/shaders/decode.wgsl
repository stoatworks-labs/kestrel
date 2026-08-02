// UYVY (4:2:2) -> full-raster RGB.
//
// The captured frame arrives as one `Rgba8Unorm` texture of half the width,
// each texel holding a macropixel: (U, Y0, V, Y1). Reading it as RGBA is a
// convenience — the GPU never has to know it is chroma.

@group(0) @binding(0) var src: texture_2d<f32>;

// Padded to 16 bytes with three *scalars*, never a `vec3<u32>`: a vec3 is
// 16-byte aligned in WGSL, so it would not sit at offset 4 the way the Rust
// `[u32; 3]` beside it does — it would push the struct to 32 bytes and the
// binding would mismatch. Pad uniform blocks with scalars.
struct DecodeU {
    // Width of `src` in macropixels, i.e. ceil(frame_width / 2).
    macro_width: u32,
    _p0: u32,
    _p1: u32,
    _p2: u32,
};
@group(0) @binding(1) var<uniform> u: DecodeU;

@fragment
fn fs_decode(in: VsOut) -> @location(0) vec4<f32> {
    // Fragment coordinates, not interpolated UVs: this stage is pixel-exact by
    // construction and an interpolated UV would round the wrong way at the
    // macropixel boundary, swapping Y0 and Y1 on some columns.
    let px = vec2<i32>(in.pos.xy);
    let mx = px.x / 2;
    let t = textureLoad(src, vec2<i32>(mx, px.y), 0);

    let odd = (px.x & 1) == 1;
    let y = select(t.g, t.a, odd);

    // Chroma is co-sited with the even luma sample, so the even pixel takes it
    // verbatim and the odd pixel sits halfway to the next macropixel. Nearest
    // chroma would be free, but this app exists to magnify — and doubling a
    // nearest-sampled chroma edge is exactly where you see the blocks.
    var cb = t.r;
    var cr = t.b;
    if odd {
        let next = min(mx + 1, i32(u.macro_width) - 1);
        let t2 = textureLoad(src, vec2<i32>(next, px.y), 0);
        cb = mix(t.r, t2.r, 0.5);
        cr = mix(t.b, t2.b, 0.5);
    }

    return vec4<f32>(ycbcr_to_rgb(y, cb, cr), 1.0);
}
