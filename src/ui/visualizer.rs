//! The spectrum bars drawn under the album art.

use crate::*;

pub(crate) fn render_visualizer(
    f: &mut Frame,
    app: &App,
    out: &mut FrameOut,
    theme: Theme,
    area: Rect,
) {
    // One try_lock covers the activity probe and the data read — the earliest
    // lock also decides whether we draw at all (non-blocking on both).
    let Ok(guard) = app.svc.engine.bands.try_lock() else {
        return;
    };
    if !guard.is_active {
        return;
    }
    let values: [f32; NUM_BANDS] = guard.values;
    let peak = guard.peak_envelope.max(1e-6);
    drop(guard);

    // Cap the spectrum to a centered band — full-pane bars are too tall/wide.
    let vh = ((area.height as u32 * 3 / 5) as u16)
        .clamp(6, 14)
        .min(area.height);
    let vw = ((area.width as u32 * 9 / 10) as u16)
        .clamp(24, 80)
        .min(area.width);
    let vrect = Rect {
        x: area.x + area.width.saturating_sub(vw) / 2,
        y: area.y + area.height.saturating_sub(vh) / 2,
        width: vw,
        height: vh,
    };
    let w = vrect.width as usize;
    let h = vrect.height as usize;
    if w == 0 || h == 0 {
        return;
    }

    // 1. Box-average the bands into each column (anti-aliasing vs. single-pick).
    let cols = &mut out.scratch.cols;
    cols.resize(w, 0.0);
    for (x, c) in cols.iter_mut().enumerate() {
        let lo = x * NUM_BANDS / w;
        let hi = (((x + 1) * NUM_BANDS / w).max(lo + 1)).min(NUM_BANDS);
        let sum: f32 = values[lo..hi].iter().sum();
        let v = sum / (hi - lo) as f32;
        // Perceptual curve so quiet detail stays visible.
        *c = (v / peak).sqrt().clamp(0.0, 1.0);
    }

    // 2. Spatial smoothing based on user setting:
    let passes = match app.config.visualizer_smoothing {
        tuna_tui::config::VisualizerSmoothing::Snappy => 0,
        tuna_tui::config::VisualizerSmoothing::Balanced => 2,
        tuna_tui::config::VisualizerSmoothing::Liquid => 4,
    };
    let src = &mut out.scratch.src;
    for _ in 0..passes {
        src.clear();
        src.extend_from_slice(cols);
        for x in 0..w {
            let l = src[x.saturating_sub(1)];
            let r = src[(x + 1).min(w - 1)];
            cols[x] = l * 0.25 + src[x] * 0.5 + r * 0.25;
        }
    }

    // 3. Paint cells directly using selected visualizer style glyphs:
    let (solid_glyph, levels): (&str, &[&str; 8]) = match app.config.visualizer_style {
        tuna_tui::config::VisualizerStyle::Block => {
            ("█", &["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"])
        }
        tuna_tui::config::VisualizerStyle::Braille => {
            ("⣿", &["⠁", "⠃", "⠇", "⡇", "⣇", "⣧", "⣷", "⣿"])
        }
        tuna_tui::config::VisualizerStyle::Line => ("─", &["─", "─", "─", "─", "─", "─", "─", "─"]),
        tuna_tui::config::VisualizerStyle::Solid => {
            ("█", &["█", "█", "█", "█", "█", "█", "█", "█"])
        }
    };
    let stops = [theme.info, theme.primary, theme.accent];
    let buf = f.buffer_mut();
    for row in 0..h {
        let from_bottom = (h - 1 - row) as f32;
        let vfrac = if h > 1 {
            from_bottom / (h - 1) as f32
        } else {
            0.0
        };
        let color: ratatui::style::Color = gradient::interpolate(&stops, vfrac).into();
        for (x, &v) in cols.iter().enumerate() {
            let filled = v * h as f32 - from_bottom;
            let ch: &str = if filled >= 1.0 {
                solid_glyph
            } else if filled <= 0.0 {
                " "
            } else {
                levels[((filled * 8.0) as usize).clamp(1, 8) - 1]
            };
            // In-bounds by construction: vrect is clamped to `area`, which the
            // renderer sizes from the frame.
            if let Some(cell) = buf.cell_mut((vrect.x + x as u16, vrect.y + row as u16)) {
                cell.set_symbol(ch);
                cell.set_fg(color);
            }
        }
    }
}
