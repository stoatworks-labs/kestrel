//! Uniform blocks, laid out to match the WGSL byte for byte.
//!
//! Two rules, both learned the expensive way and both enforced here:
//!
//! * **Never a `vec3` in a uniform block.** WGSL aligns `vec3<T>` to 16 bytes,
//!   so it does not sit where the `[T; 3]` beside it in Rust does — it pushes
//!   everything after it and changes the block size. Pad with scalars.
//! * **Uniform blocks round up to 16.** A struct that is "obviously" 56 bytes
//!   is 64 in the uniform address space, and a 56-byte binding is rejected at
//!   pipeline-creation time with a size error that names no field.
//!
//! The tests at the bottom assert the sizes, so a future edit that breaks
//! either rule fails `cargo test` rather than a shader compile on a show day.

use bytemuck::{Pod, Zeroable};
use kestrel_core::{NormRect, Placement, ScalingFilter, Size};

/// A single `u32` payload, padded to a legal uniform block. Used by the decode,
/// pack and fill stages, which each need exactly one number.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ScalarUniform {
    pub value: u32,
    pub _pad: [u32; 3],
}

impl ScalarUniform {
    pub fn new(value: u32) -> Self {
        Self {
            value,
            _pad: [0; 3],
        }
    }
}

#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct CropUniform {
    /// `xy` origin, `zw` size, normalised in the source.
    pub src_rect: [f32; 4],
    /// `xy` origin, `zw` size, normalised in the output.
    pub dst_rect: [f32; 4],
    pub src_px: [f32; 2],
    pub dst_px: [f32; 2],
    pub mode: u32,
    pub _pad: [u32; 3],
}

impl CropUniform {
    pub fn new(p: &Placement, input: Size, output: Size, filter: ScalingFilter) -> Self {
        Self {
            src_rect: rect_to_array(&p.src),
            dst_rect: rect_to_array(&p.dst),
            src_px: [input.w as f32, input.h as f32],
            dst_px: [output.w as f32, output.h as f32],
            mode: match filter {
                ScalingFilter::Bilinear => 0,
                ScalingFilter::Bicubic => 1,
            },
            _pad: [0; 3],
        }
    }
}

fn rect_to_array(r: &NormRect) -> [f32; 4] {
    [r.x as f32, r.y as f32, r.w as f32, r.h as f32]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_are_multiples_of_sixteen() {
        assert_eq!(std::mem::size_of::<ScalarUniform>(), 16);
        assert_eq!(std::mem::size_of::<CropUniform>(), 64);
        assert_eq!(std::mem::align_of::<CropUniform>(), 16);
    }

    #[test]
    fn crop_fields_sit_where_the_wgsl_expects_them() {
        // Offsets are asserted rather than assumed: the shader reads
        // src_rect at 0, dst_rect at 16, src_px at 32, dst_px at 40, mode at 48.
        let u = CropUniform {
            src_rect: [1.0, 2.0, 3.0, 4.0],
            dst_rect: [5.0, 6.0, 7.0, 8.0],
            src_px: [9.0, 10.0],
            dst_px: [11.0, 12.0],
            mode: 1,
            _pad: [0; 3],
        };
        let bytes: &[u8] = bytemuck::bytes_of(&u);
        let f = |off: usize| f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        assert_eq!(f(0), 1.0);
        assert_eq!(f(16), 5.0);
        assert_eq!(f(32), 9.0);
        assert_eq!(f(40), 11.0);
        assert_eq!(
            u32::from_le_bytes(bytes[48..52].try_into().unwrap()),
            1,
            "mode must land at offset 48"
        );
    }
}
