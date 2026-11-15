//! The tray icon, drawn in code: a bold lowercase "c" that reads as a circular rewind arrow.
//!
//! Colour shows the state at a glance: grey = idle, accent = buffering, red with a dot = recording.
//! A real designed .ico replaces this once the logo exists; the state colours stay.

use clipr_core::status::RecState;

pub const SIZE: u32 = 32;

const GREY: [u8; 3] = [0x9C, 0x9C, 0xA8];
const ACCENT: [u8; 3] = [0x8B, 0x5C, 0xF6];
const RED: [u8; 3] = [0xEF, 0x44, 0x44];

/// RGBA pixels, `SIZE × SIZE`.
pub fn rgba(state: RecState) -> Vec<u8> {
    let colour = match state {
        RecState::Idle => GREY,
        RecState::Buffering => ACCENT,
        RecState::Recording => RED,
    };
    let s = SIZE as f32;
    let (cx, cy) = (s / 2.0, s / 2.0);
    let (outer, inner) = (s * 0.46, s * 0.27);
    let mut px = vec![0u8; (SIZE * SIZE * 4) as usize];

    for y in 0..SIZE {
        for x in 0..SIZE {
            // Sample 4×4 points per pixel for smooth edges.
            let mut cover = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = x as f32 + (sx as f32 + 0.5) / 4.0 - cx;
                    let fy = y as f32 + (sy as f32 + 0.5) / 4.0 - cy;
                    let r = (fx * fx + fy * fy).sqrt();
                    // The "c" opening on the right: leave out a 70° wedge around the +x axis.
                    let angle = fy.atan2(fx).to_degrees();
                    let in_ring = r <= outer && r >= inner;
                    let in_gap = angle.abs() < 35.0;
                    // Recording: a solid dot in the middle.
                    let in_dot = state == RecState::Recording && r <= s * 0.14;
                    if (in_ring && !in_gap) || in_dot {
                        cover += 1;
                    }
                }
            }
            let i = ((y * SIZE + x) * 4) as usize;
            px[i..i + 3].copy_from_slice(&colour);
            px[i + 3] = (cover * 255 / 16) as u8;
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_ring_gap_and_dot_where_expected() {
        let alpha = |state, x: u32, y: u32| rgba(state)[((y * SIZE + x) * 4 + 3) as usize];
        // Left side of the ring is drawn, the right side is the "c" opening.
        assert_eq!(alpha(RecState::Idle, 3, 16), 255);
        assert_eq!(alpha(RecState::Idle, 28, 16), 0);
        // The centre is empty unless recording.
        assert_eq!(alpha(RecState::Buffering, 16, 16), 0);
        assert_eq!(alpha(RecState::Recording, 16, 16), 255);
    }
}
