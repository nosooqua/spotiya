//! Cucumber SCLED frames for the native compositor (not sysfs segment writes).
const DIGITS: [u8; 10] = [63, 6, 91, 79, 102, 109, 125, 7, 127, 111];
pub fn animation(volume: u16, decreasing: bool) -> Vec<u8> {
    // Key 0: 45 raw brightness bytes, then a big-endian u16 duration in ms.
    // Each digit occupies nine slots: a..g, p, q. The remaining nine are icons.
    let mut out = vec![0; 48];
    let value = volume.min(99) as usize;
    let start = if decreasing { 0 } else { 2 };
    for (digit, number) in [(start, value / 10), (start + 1, value % 10)] {
        for segment in 0..7 {
            if DIGITS[number] & (1 << segment) != 0 {
                out[1 + digit * 9 + segment] = 255;
            }
        }
    }
    out[46..].copy_from_slice(&2000u16.to_be_bytes());
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_clock_direction_scale_and_duration() {
        for (volume, left, expected) in [
            (45, true, [4, 5]),
            (50, false, [5, 0]),
            (100, false, [9, 9]),
            (0, true, [0, 0]),
        ] {
            let data = animation(volume, left);
            assert_eq!(data.len(), 48);
            assert_eq!(data[0], 0);
            assert_eq!(u16::from_be_bytes(data[46..].try_into().unwrap()), 2000);
            let start = if left { 0 } else { 2 };
            for digit in 0..4 {
                let mask = data[1 + digit * 9..8 + digit * 9]
                    .iter()
                    .enumerate()
                    .fold(0, |m, (i, v)| m | if *v > 0 { 1 << i } else { 0 });
                assert_eq!(
                    mask,
                    if (start..start + 2).contains(&digit) {
                        DIGITS[expected[digit - start]]
                    } else {
                        0
                    }
                );
                assert_eq!(&data[8 + digit * 9..10 + digit * 9], &[0, 0]);
            }
            assert!(data[37..46].iter().all(|v| *v == 0));
        }
    }
}
