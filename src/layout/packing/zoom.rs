use std::collections::VecDeque;

pub(super) const LOG_RATE: f32 = 0.12;
const LOOK_AHEAD: usize = 5 * 60;

pub(super) fn plan(rows: &mut [Vec<f32>]) {
    let Some(first) = rows.first() else { return };
    let repositories = first.len();
    let factor = (LOG_RATE / 60.0).exp();
    for repository in 0..repositories {
        let raw: Vec<_> = rows.iter().map(|row| row[repository]).collect();
        let mut window: VecDeque<usize> = VecDeque::new();
        for tick in (0..rows.len()).rev() {
            while window.front().is_some_and(|&i| i > tick + LOOK_AHEAD) {
                window.pop_front();
            }
            if raw[tick] > 0.0 {
                while window.back().is_some_and(|&i| raw[i] >= raw[tick]) {
                    window.pop_back();
                }
                window.push_back(tick);
                rows[tick][repository] =
                    raw[*window.front().expect("current positive scale")];
            }
        }
        for tick in (0..rows.len().saturating_sub(1)).rev() {
            if rows[tick][repository] > 0.0 && rows[tick + 1][repository] > 0.0 {
                rows[tick][repository] =
                    rows[tick][repository].min(rows[tick + 1][repository] * factor);
            }
        }
        for tick in 1..rows.len() {
            if rows[tick][repository] > 0.0 && rows[tick - 1][repository] > 0.0 {
                rows[tick][repository] =
                    rows[tick][repository].min(rows[tick - 1][repository] * factor);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anticipates_growth_without_exceeding_fit_or_rate_after_birth() {
        let raw: Vec<_> = (0..900)
            .map(|i| {
                vec![if i < 30 {
                    0.0
                } else if i < 120 {
                    10.0
                } else if i < 600 {
                    1.0
                } else {
                    5.0
                }]
            })
            .collect();
        let mut planned = raw.clone();
        plan(&mut planned);
        assert_eq!(planned[30][0], 1.0);
        for (row, raw) in planned.iter().zip(&raw) {
            assert!(row[0] <= raw[0]);
        }
        for pair in planned
            .windows(2)
            .filter(|pair| pair[0][0] > 0.0 && pair[1][0] > 0.0)
        {
            assert!((pair[1][0] / pair[0][0]).ln().abs() * 60.0 <= LOG_RATE + 0.0001);
        }
    }
}
