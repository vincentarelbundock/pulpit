//! Frame scaling shared by the media workers.

use std::num::NonZeroU32;

/// Scale a decoded frame into the viewport with a box filter.
///
/// A slide overlay is almost always downscaled, where averaging the covered
/// source pixels is both cheap and visibly better than nearest-neighbour.
pub fn scale_rgba(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> Vec<u8> {
    let target_len = target_width as usize * target_height as usize * 4;
    if source_width == target_width && source_height == target_height {
        return source.to_vec();
    }
    let mut out = vec![0u8; target_len];
    if source_width == 0 || source_height == 0 || target_width == 0 || target_height == 0 {
        return out;
    }
    for y in 0..target_height {
        let y0 = (y as u64 * source_height as u64 / target_height as u64) as u32;
        let y1 = ((y as u64 + 1) * source_height as u64)
            .div_ceil(target_height as u64)
            .min(source_height as u64) as u32;
        let y1 = y1.max(y0 + 1);
        for x in 0..target_width {
            let x0 = (x as u64 * source_width as u64 / target_width as u64) as u32;
            let x1 = ((x as u64 + 1) * source_width as u64)
                .div_ceil(target_width as u64)
                .min(source_width as u64) as u32;
            let x1 = x1.max(x0 + 1);

            let mut sums = [0u32; 4];
            let mut count = 0u32;
            for sy in y0..y1 {
                let row = sy as usize * source_width as usize * 4;
                for sx in x0..x1 {
                    let index = row + sx as usize * 4;
                    if let Some(pixel) = source.get(index..index + 4) {
                        for channel in 0..4 {
                            sums[channel] += pixel[channel] as u32;
                        }
                        count += 1;
                    }
                }
            }
            let out_index = (y as usize * target_width as usize + x as usize) * 4;
            // A target pixel whose source rectangle fell entirely outside the
            // frame contributed nothing. Leaving it at zero is correct;
            // carrying the count as a non-zero type is what says so.
            if let Some(contributors) = NonZeroU32::new(count) {
                for channel in 0..4 {
                    out[out_index + channel] = (sums[channel] / contributors) as u8;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_already_at_the_target_size_is_passed_through() {
        let frame = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(scale_rgba(&frame, 2, 1, 2, 1), frame);
    }

    #[test]
    fn downscaling_averages_the_pixels_it_covers() {
        // Two pixels, black and white, collapsed into one: grey, not either
        // end, which is what nearest-neighbour would give.
        let frame = vec![0, 0, 0, 255, 255, 255, 255, 255];
        assert_eq!(scale_rgba(&frame, 2, 1, 1, 1), vec![127, 127, 127, 255]);
    }

    #[test]
    fn a_degenerate_size_yields_a_blank_frame_rather_than_panicking() {
        assert!(scale_rgba(&[], 0, 0, 2, 2).iter().all(|byte| *byte == 0));
        assert!(scale_rgba(&[1, 2, 3, 4], 1, 1, 0, 0).is_empty());
    }
}
