//! Kestrel's GPU path.
//!
//! One captured 4:2:2 frame goes in; one packed 4:2:2 frame per output comes
//! out, each a rectangle of that frame scaled to the output raster.
//!
//! Four stages, all fragment shaders over a full-screen triangle:
//!
//! 1. **decode** — UYVY (a half-width RGBA texture of macropixels) to full
//!    raster RGB, BT.709 limited range, with the chroma interpolated rather
//!    than nearest-sampled. Runs once per *captured* frame.
//! 2. **crop** — a rectangle of that, scaled to the output raster, bilinear or
//!    Catmull-Rom. Runs once per output per *output* frame.
//! 3. **fill** — black or bars, for an output carrying nothing.
//! 4. **pack** — RGB back to UYVY, into a half-width target whose rows are
//!    *exactly* the byte layout DeckLink wants.
//!
//! Stage 4 is why the CPU never touches a pixel: it only ever memcpys rows out
//! of a mapped buffer. Colour conversion, chroma siting and scaling all happen
//! on the GPU.
//!
//! Everything intermediate is `Rgba8Unorm`, never sRGB. These pixels arrived
//! over SDI already encoded; a gamma re-encode on the way through would shift
//! every colour on the output.

pub mod engine;
pub mod gpu;
pub mod pattern;
pub mod uniforms;

pub use engine::{read_texture_rgba, Engine};
pub use gpu::{align_up, Gpu, COPY_ALIGN, TARGET_FORMAT};
pub use pattern::{bars_uyvy, gradient_uyvy, solid_uyvy};
