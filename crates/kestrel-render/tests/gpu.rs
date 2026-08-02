//! End-to-end GPU tests: real device, real shaders, pixels read back.
//!
//! These need a working adapter and they fail loudly without one rather than
//! skipping. A GPU test that quietly skips is a GPU test that has never run.

use kestrel_core::{
    FitMode, NormRect, OutputId, OutputPlan, Pattern, PlanSource, Scan, ScalingFilter, Size,
    VideoFormat,
};
use kestrel_render::pattern::{
    bars_uyvy, quadrant_colour, quadrants_uyvy, rgb_to_ycbcr, row_bytes, solid_uyvy, BARS_75,
};
use kestrel_render::{Engine, Gpu};

const IN: Size = Size::new(256, 256);
const OUT: Size = Size::new(256, 256);

fn engine(input: Size, output: Size) -> Engine {
    let gpu = pollster::block_on(Gpu::new()).expect(
        "no GPU adapter. These tests exercise the real shaders; there is no CPU \
         fallback worth having, because the thing under test is the shader.",
    );
    let fmt = VideoFormat::new(output.w, output.h, 50, 1, Scan::Progressive);
    Engine::new(gpu, input, fmt).expect("engine")
}

/// The pixel at a fractional position, from a tightly packed RGBA buffer.
fn px(rgba: &[u8], size: Size, fx: f64, fy: f64) -> [u8; 3] {
    let x = ((fx * size.w as f64) as u32).min(size.w - 1) as usize;
    let y = ((fy * size.h as f64) as u32).min(size.h - 1) as usize;
    let i = (y * size.w as usize + x) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

#[track_caller]
fn near(got: [u8; 3], want: [u8; 3], tol: i32, what: &str) {
    for i in 0..3 {
        let d = got[i] as i32 - want[i] as i32;
        assert!(
            d.abs() <= tol,
            "{what}: got {got:?} want {want:?} (channel {i} off by {d}, tolerance {tol})"
        );
    }
}

/// A plan that routes one rectangle to one output.
fn crop_plan(id: OutputId, rect: NormRect, fit: FitMode) -> Vec<OutputPlan> {
    vec![OutputPlan {
        output: id,
        source: PlanSource::Crop {
            roi: None,
            rect,
            fit,
        },
    }]
}

fn render_once(e: &mut Engine, plan: &[OutputPlan]) -> Vec<(OutputId, Vec<u8>, u32)> {
    let mut got = Vec::new();
    e.render(plan, &mut |id, bytes, row| {
        got.push((id, bytes.to_vec(), row))
    })
    .expect("render");
    got
}

// --- stage 1: decode ------------------------------------------------------

#[test]
fn decode_recovers_the_primaries_from_422() {
    let mut e = engine(IN, OUT);
    e.upload_input(&quadrants_uyvy(IN), row_bytes(IN)).unwrap();
    let rgba = e.read_input_rgba().unwrap();

    // Sampled well inside each block. 4:2:2 halves the chroma resolution, so
    // anywhere near a colour boundary is legitimately a blend and asserting
    // there would be asserting on the format's own limitation.
    for (fx, fy) in [(0.25, 0.25), (0.75, 0.25), (0.75, 0.75), (0.25, 0.75)] {
        let x = (fx * IN.w as f64) as u32;
        let y = (fy * IN.h as f64) as u32;
        near(
            px(&rgba, IN, fx, fy),
            quadrant_colour(x, y, IN),
            4,
            &format!("quadrant at ({fx}, {fy})"),
        );
    }
}

#[test]
fn decode_puts_luma_in_the_right_column() {
    // The Y0/Y1 selection is the easiest thing in the whole pipeline to get
    // off by one, and a one-column luma shift looks like a soft picture rather
    // than like a bug. A hard vertical edge catches it.
    let mut e = engine(Size::new(64, 8), Size::new(64, 8));
    let size = Size::new(64, 8);
    let frame = kestrel_render::pattern::build_uyvy(size, |x, _| {
        if x < 32 {
            [255, 255, 255]
        } else {
            [0, 0, 0]
        }
    });
    e.upload_input(&frame, row_bytes(size)).unwrap();
    let rgba = e.read_input_rgba().unwrap();

    let at = |x: usize| -> u8 { rgba[(4 * 64 + x) * 4] };
    assert!(at(31) > 200, "last white column is dark: {}", at(31));
    assert!(at(32) < 40, "first black column is bright: {}", at(32));
}

// --- stage 2: crop and scale ---------------------------------------------

#[test]
fn a_full_frame_crop_at_unity_reproduces_the_input() {
    let mut e = engine(IN, OUT);
    e.upload_input(&quadrants_uyvy(IN), row_bytes(IN)).unwrap();
    let id = OutputId(1);
    render_once(&mut e, &crop_plan(id, NormRect::FULL, FitMode::Fit));
    let out = e.read_output_rgba(id).unwrap();

    for (fx, fy) in [(0.25, 0.25), (0.75, 0.25), (0.75, 0.75), (0.25, 0.75)] {
        let x = (fx * IN.w as f64) as u32;
        let y = (fy * IN.h as f64) as u32;
        near(
            px(&out, OUT, fx, fy),
            quadrant_colour(x, y, IN),
            4,
            "unity crop",
        );
    }
}

#[test]
fn cropping_one_quadrant_magnifies_it_to_fill_the_output() {
    let mut e = engine(IN, OUT);
    e.upload_input(&quadrants_uyvy(IN), row_bytes(IN)).unwrap();
    let id = OutputId(1);
    // The top-left quadrant is flat red; blown up 2x it should fill the output.
    render_once(
        &mut e,
        &crop_plan(id, NormRect::new(0.0, 0.0, 0.5, 0.5), FitMode::Fit),
    );
    let out = e.read_output_rgba(id).unwrap();

    // The interior only. A bicubic tap reaches a pixel and a half past the
    // region edge, which is correct — it is what a resampler over the
    // continuous image does, and it is why crops do not get a dark rim — but it
    // means the outermost pixels legitimately carry a little of the neighbour.
    for fy in [0.1, 0.5, 0.9] {
        for fx in [0.1, 0.5, 0.9] {
            near(px(&out, OUT, fx, fy), [255, 0, 0], 4, "magnified quadrant");
        }
    }
}

#[test]
fn fit_leaves_black_bars_and_fill_does_not() {
    let mut e = engine(IN, OUT);
    e.upload_input(&solid_uyvy(IN, [191, 191, 191]), row_bytes(IN))
        .unwrap();
    let id = OutputId(1);

    // A region twice as wide as it is tall, on a square output.
    let wide = NormRect::new(0.1, 0.4, 0.8, 0.4);

    render_once(&mut e, &crop_plan(id, wide, FitMode::Fit));
    let fitted = e.read_output_rgba(id).unwrap();
    near(px(&fitted, OUT, 0.5, 0.02), [0, 0, 0], 2, "fit top bar");
    near(px(&fitted, OUT, 0.5, 0.5), [191, 191, 191], 4, "fit centre");

    render_once(&mut e, &crop_plan(id, wide, FitMode::Fill));
    let filled = e.read_output_rgba(id).unwrap();
    near(
        px(&filled, OUT, 0.5, 0.02),
        [191, 191, 191],
        4,
        "fill must reach the top edge",
    );
}

#[test]
fn both_scaling_filters_agree_on_a_flat_region() {
    // Bilinear and bicubic must differ only in how they treat detail. On a flat
    // colour they must agree exactly, which pins down that the bicubic weights
    // sum to one — the classic Catmull-Rom typo brightens or darkens
    // everything by a few percent and is invisible without this check.
    let mut e = engine(IN, OUT);
    e.upload_input(&solid_uyvy(IN, [100, 150, 200]), row_bytes(IN))
        .unwrap();
    let id = OutputId(1);
    let plan = crop_plan(id, NormRect::new(0.2, 0.2, 0.3, 0.3), FitMode::Fit);

    e.set_scaling(ScalingFilter::Bilinear);
    render_once(&mut e, &plan);
    let bilinear = px(&e.read_output_rgba(id).unwrap(), OUT, 0.5, 0.5);

    e.set_scaling(ScalingFilter::Bicubic);
    render_once(&mut e, &plan);
    let bicubic = px(&e.read_output_rgba(id).unwrap(), OUT, 0.5, 0.5);

    near(bicubic, bilinear, 1, "filters disagree on a flat colour");
}

// --- the "always output" invariant ---------------------------------------

#[test]
fn every_output_produces_a_frame_whatever_the_plan_says() {
    let mut e = engine(IN, OUT);
    e.upload_input(&solid_uyvy(IN, [200, 100, 50]), row_bytes(IN))
        .unwrap();

    let plan = vec![
        OutputPlan {
            output: OutputId(1),
            source: PlanSource::Crop {
                roi: None,
                rect: NormRect::FULL,
                fit: FitMode::Fit,
            },
        },
        OutputPlan {
            output: OutputId(2),
            source: PlanSource::Pattern(Pattern::Black),
        },
        OutputPlan {
            output: OutputId(3),
            source: PlanSource::Pattern(Pattern::Bars),
        },
        OutputPlan {
            output: OutputId(4),
            source: PlanSource::Muted,
        },
        OutputPlan {
            output: OutputId(5),
            source: PlanSource::NoInput,
        },
    ];

    let got = render_once(&mut e, &plan);
    assert_eq!(got.len(), 5, "one finished frame per output, always");
    let want_len = row_bytes(OUT) as usize * OUT.h as usize;
    for (id, bytes, row) in &got {
        assert_eq!(bytes.len(), want_len, "{id} produced a short frame");
        assert_eq!(*row, row_bytes(OUT), "{id} row stride");
    }
}

#[test]
fn a_muted_or_unrouted_output_carries_legal_black_not_zero() {
    let mut e = engine(IN, OUT);
    e.upload_input(&solid_uyvy(IN, [255, 255, 255]), row_bytes(IN))
        .unwrap();

    for source in [
        PlanSource::Muted,
        PlanSource::NoInput,
        PlanSource::Pattern(Pattern::Black),
    ] {
        let got = render_once(
            &mut e,
            &[OutputPlan {
                output: OutputId(1),
                source,
            }],
        );
        let frame = &got[0].1;
        // Y=16, C=128 everywhere. Super-black (Y=0) is what you get if the
        // conversion is skipped, and some switchers clamp it and some flag it.
        for (i, chunk) in frame.chunks_exact(4).enumerate().take(4096) {
            assert_eq!(
                chunk,
                [128, 16, 128, 16],
                "{source:?} macropixel {i} is not legal black"
            );
        }
    }
}

#[test]
fn the_bars_idle_really_is_bars() {
    let mut e = engine(IN, OUT);
    let got = render_once(
        &mut e,
        &[OutputPlan {
            output: OutputId(1),
            source: PlanSource::Pattern(Pattern::Bars),
        }],
    );
    let frame = &got[0].1;
    let stride = row_bytes(OUT) as usize;

    // Luma of the middle of each of the eight bars, against the CPU reference.
    for bar in 0..8u32 {
        let x = (bar * OUT.w / 8) + OUT.w / 16;
        let i = 128 * stride + (x as usize / 2) * 4;
        let want = rgb_to_ycbcr(BARS_75[bar as usize])[0];
        let got_y = if x % 2 == 0 { frame[i + 1] } else { frame[i + 3] };
        let d = got_y as i32 - want as i32;
        assert!(d.abs() <= 3, "bar {bar}: Y {got_y} want {want}");
    }
}

// --- stage 4: pack and readback ------------------------------------------

#[test]
fn a_raster_whose_rows_need_padding_reads_back_without_shear() {
    // 1366 wide: the UYVY row is 2732 bytes, which is *not* a multiple of the
    // 256-byte copy alignment, so the staging buffer is padded to 2816. At
    // 1920 (3840 = 15 x 256) the padding is zero and a de-padding bug is
    // invisible — this is the raster that catches it. A mishandled pad shows
    // up as a picture that shears progressively down the frame.
    let odd = Size::new(1366, 768);
    let mut e = engine(odd, odd);
    e.upload_input(&solid_uyvy(odd, [191, 0, 191]), row_bytes(odd))
        .unwrap();

    let id = OutputId(1);
    let got = render_once(&mut e, &crop_plan(id, NormRect::FULL, FitMode::Fit));
    let (_, frame, row) = &got[0];

    assert_eq!(*row, 2732, "unpadded stride");
    assert_eq!(frame.len(), 2732 * 768);

    let want = {
        let ycc = rgb_to_ycbcr([191, 0, 191]);
        [ycc[1], ycc[0], ycc[2], ycc[0]]
    };
    // Check the last macropixel of the last row: the furthest point from the
    // start of the buffer, and the first thing a wrong stride corrupts.
    let last = frame.len() - 4;
    for (label, i) in [("first", 0usize), ("last", last)] {
        for c in 0..4 {
            let d = frame[i + c] as i32 - want[c] as i32;
            assert!(
                d.abs() <= 3,
                "{label} macropixel byte {c}: {} want {} — stride handling is wrong",
                frame[i + c],
                want[c]
            );
        }
    }
}

#[test]
fn the_pack_survives_a_round_trip_through_the_decoder() {
    // Feed the packed output back in as if it were a capture. A colour that
    // comes out where it went in proves the two conversions really are
    // inverses, which no amount of staring at matrix constants does.
    let mut e = engine(IN, OUT);
    e.upload_input(&bars_uyvy(IN), row_bytes(IN)).unwrap();
    let id = OutputId(1);
    let got = render_once(&mut e, &crop_plan(id, NormRect::FULL, FitMode::Fit));

    let mut back = engine(OUT, OUT);
    back.upload_input(&got[0].1, got[0].2).unwrap();
    let rgba = back.read_input_rgba().unwrap();

    for bar in 0..8usize {
        let fx = (bar as f64 + 0.5) / 8.0;
        near(
            px(&rgba, OUT, fx, 0.5),
            BARS_75[bar],
            6,
            &format!("bar {bar} after a full round trip"),
        );
    }
}

// --- reconfiguration ------------------------------------------------------

#[test]
fn changing_the_output_format_resizes_every_target() {
    let mut e = engine(IN, OUT);
    e.upload_input(&solid_uyvy(IN, [191, 191, 191]), row_bytes(IN))
        .unwrap();
    let id = OutputId(1);
    render_once(&mut e, &crop_plan(id, NormRect::FULL, FitMode::Fit));

    let bigger = VideoFormat::new(640, 360, 50, 1, Scan::Progressive);
    e.set_output_format(bigger);
    let got = render_once(&mut e, &crop_plan(id, NormRect::FULL, FitMode::Fit));
    assert_eq!(got[0].2, 640 * 2, "stride must follow the new raster");
    assert_eq!(got[0].1.len(), 640 * 2 * 360);
}

#[test]
fn changing_the_input_raster_does_not_leave_a_stale_binding() {
    // The crop bind groups point at the decoded texture. Resizing the input
    // replaces it, and anything still holding the old view renders the old
    // picture forever — which looks like a frozen source, not like a bug.
    let mut e = engine(IN, OUT);
    e.upload_input(&solid_uyvy(IN, [255, 0, 0]), row_bytes(IN))
        .unwrap();
    let id = OutputId(1);
    render_once(&mut e, &crop_plan(id, NormRect::FULL, FitMode::Fit));

    let bigger = Size::new(512, 512);
    e.set_input_size(bigger);
    assert!(!e.input_live(), "a resize must invalidate the live flag");
    e.upload_input(&solid_uyvy(bigger, [0, 0, 255]), row_bytes(bigger))
        .unwrap();
    render_once(&mut e, &crop_plan(id, NormRect::FULL, FitMode::Fit));

    near(
        px(&e.read_output_rgba(id).unwrap(), OUT, 0.5, 0.5),
        [0, 0, 255],
        4,
        "output still shows the pre-resize frame",
    );
}

#[test]
fn a_short_frame_is_refused_rather_than_read_past_the_end() {
    let mut e = engine(IN, OUT);
    let err = e
        .upload_input(&vec![0u8; 16], row_bytes(IN))
        .expect_err("a truncated capture buffer must not be uploaded");
    assert!(err.to_string().contains("short frame"), "{err}");
}
