// Shared by every stage: the full-screen triangle, and BT.709 in both
// directions.
//
// Prepended to each stage's source at pipeline-build time (see `shader()` in
// lib.rs) because WGSL has no include.

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One oversized triangle rather than two triangles: no seam down the diagonal,
// no vertex buffer, three vertices.
@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.uv = uv;
    out.pos = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

// --- BT.709, limited (studio) range --------------------------------------
//
// Limited range, not full: SDI carries 16..235 luma and 16..240 chroma, and
// treating those as 0..255 is the classic "the blacks are grey and the whites
// clip" bug. The constants are the exact BT.709 ones — 0.2126 / 0.7152 / 0.0722
// — not the BT.601 set, which is what generic RGB conversion helpers usually
// hand you and which visibly rotates saturated colours on HD material.

fn ycbcr_to_rgb(y: f32, cb: f32, cr: f32) -> vec3<f32> {
    let yy = (y * 255.0 - 16.0) / 219.0;
    let u = (cb * 255.0 - 128.0) / 224.0;
    let v = (cr * 255.0 - 128.0) / 224.0;
    return vec3<f32>(
        yy + 1.5748 * v,
        yy - 0.187324 * u - 0.468124 * v,
        yy + 1.8556 * u,
    );
}

fn rgb_to_ycbcr(rgb: vec3<f32>) -> vec3<f32> {
    let y = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let cb = (rgb.b - y) / 1.8556;
    let cr = (rgb.r - y) / 1.5748;
    return vec3<f32>(
        (219.0 * y + 16.0) / 255.0,
        (224.0 * cb + 128.0) / 255.0,
        (224.0 * cr + 128.0) / 255.0,
    );
}
