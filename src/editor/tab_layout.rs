//! Correct tab advances without expanding the source or its character indices.
use eframe::egui::{self, Galley};
use std::sync::Arc;

pub(super) fn tab_stops(galley: Arc<Galley>, stop: f32) -> Arc<Galley> {
    if !galley.job.text.contains('\t') || stop <= 0.0 {
        return galley;
    }
    let mut galley = (*galley).clone();
    let mut width = 0.0_f32;
    let mut bounds = egui::Rect::NOTHING;
    for placed in &mut galley.rows {
        let row = Arc::make_mut(&mut placed.row);
        let old_glyphs = row.glyphs.clone();
        let mut intervals = Vec::new();
        let mut shift = 0.0;
        for glyph in &mut row.glyphs {
            let old_x = glyph.pos.x;
            let old_width = glyph.advance_width;
            glyph.pos.x += shift;
            if glyph.chr == '\t' {
                let x = glyph.pos.x;
                // Tolerate pixel rounding at exact tab stops.
                let column = (x / stop + 0.0001).floor();
                glyph.advance_width = ((column + 1.0) * stop - x).max(0.0);
                intervals.push((
                    old_x,
                    old_x + old_width,
                    shift,
                    glyph.advance_width - old_width,
                ));
                shift += glyph.advance_width - old_width;
            }
        }
        let map_x = |x: f32| {
            let mut delta = 0.0;
            for &(start, end, before, change) in &intervals {
                if x < start {
                    break;
                }
                if x < end {
                    return x + before + change * (x - start) / (end - start);
                }
                delta = before + change;
            }
            x + delta
        };
        let vertices = &mut row.visuals.mesh.vertices;
        // Backgrounds and decorations span whitespace, so resize their geometry.
        for (index, vertex) in vertices.iter_mut().enumerate() {
            if !row.visuals.glyph_vertex_range.contains(&index) {
                vertex.pos.x = map_x(vertex.pos.x);
            }
        }
        // Translate glyph quads rather than distorting shaped glyphs or ligatures.
        for (old, new) in old_glyphs.iter().zip(&row.glyphs) {
            let first = old.first_vertex as usize;
            if !old.uv_rect.is_nothing()
                && first >= row.visuals.glyph_vertex_range.start
                && first
                    .checked_add(4)
                    .is_some_and(|end| end <= row.visuals.glyph_vertex_range.end)
            {
                for vertex in &mut vertices[first..first + 4] {
                    vertex.pos.x += new.pos.x - old.pos.x;
                }
            }
        }
        row.size.x = (row.size.x + shift).max(0.0);
        row.visuals.mesh_bounds = vertices.iter().fold(egui::Rect::NOTHING, |rect, v| {
            rect.union(egui::Rect::from_min_max(v.pos, v.pos))
        });
        bounds = bounds.union(row.visuals.mesh_bounds.translate(placed.pos.to_vec2()));
        width = width.max(placed.pos.x + row.size.x);
    }
    galley.rect.max.x = galley.rect.min.x + width;
    galley.mesh_bounds = bounds;
    Arc::new(galley)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::text::CCursor;
    #[test]
    fn tabs_reach_stops_and_carets_hit_test_original_character_offsets() {
        let ctx = egui::Context::default();
        crate::test_frame(&ctx, Default::default(), |root| {
            egui::CentralPanel::default().show(root, |ui| {
                let font = egui::FontId::monospace(14.0);
                let space = ui.fonts_mut(|f| f.glyph_width(&font, ' '));
                for source in ["abcdef\tx", "ab\t\tz", "\t文\té\tz", "a\t\t", "a\tb\n\tc"] {
                    let mut job = egui::text::LayoutJob::single_section(
                        source.into(),
                        egui::TextFormat {
                            font_id: font.clone(),
                            background: egui::Color32::DARK_GRAY,
                            ..Default::default()
                        },
                    );
                    job.wrap.max_width = f32::INFINITY;
                    let original = ui.fonts_mut(|f| f.layout_job(job));
                    for width in [4, 6, 8] {
                        let stop = width as f32 * space;
                        let galley = tab_stops(original.clone(), stop);
                        assert_eq!(galley.job.text, source);
                        assert_eq!(galley.rows.len(), original.rows.len());
                        let mut offset = 0;
                        for (placed, old_placed) in galley.rows.iter().zip(&original.rows) {
                            let row = &placed.row;
                            assert_eq!(row.glyphs.len(), old_placed.row.glyphs.len());
                            for (i, (glyph, old)) in
                                row.glyphs.iter().zip(&old_placed.row.glyphs).enumerate()
                            {
                                if glyph.chr == '\t' {
                                    let expected =
                                        ((glyph.pos.x / stop + 0.0001).floor() + 1.0) * stop;
                                    assert!((glyph.max_x() - expected).abs() < 0.01);
                                    assert!(
                                        glyph.advance_width > 0.0
                                            && glyph.advance_width <= stop + 0.01
                                    );
                                }
                                if !old.uv_rect.is_nothing() {
                                    let vertex = old.first_vertex as usize;
                                    let before = old_placed.row.visuals.mesh.vertices[vertex].pos.x;
                                    let after = row.visuals.mesh.vertices[vertex].pos.x;
                                    assert!(
                                        (after - before - (glyph.pos.x - old.pos.x)).abs() < 0.01
                                    );
                                }
                                let cursor = CCursor::new(offset + i);
                                let pos = galley.pos_from_cursor(cursor).center();
                                let found = galley.cursor_from_pos(pos.to_vec2());
                                assert_eq!(
                                    found.index, cursor.index,
                                    "source={source:?} width={width}"
                                );
                            }
                            offset += row.glyphs.len() + usize::from(placed.ends_with_newline);
                        }
                    }
                }
            });
        });
    }
}
