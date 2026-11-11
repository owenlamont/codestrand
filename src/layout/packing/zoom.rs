pub(super) const LOG_RATE: f32 = 0.12;
const CONTRACT_RATE: f32 = 0.08;
const EASE_SECONDS: f32 = 0.5;

pub(super) fn plan(rows: &mut [Vec<f32>]) {
    let Some(first) = rows.first() else { return };
    let repositories = first.len();
    let outward = LOG_RATE / 60.0;
    let inward = CONTRACT_RATE / 60.0;
    let blend = 1.0 - (-1.0 / (60.0 * EASE_SECONDS)).exp();
    for repository in 0..repositories {
        for tick in (0..rows.len().saturating_sub(1)).rev() {
            if rows[tick][repository] > 0.0 && rows[tick + 1][repository] > 0.0 {
                let now = rows[tick][repository].ln();
                let next = rows[tick + 1][repository].ln();
                rows[tick][repository] = now.min(next + outward).exp();
            }
        }
        for tick in (0..rows.len().saturating_sub(1)).rev() {
            if rows[tick][repository] > 0.0 && rows[tick + 1][repository] > 0.0 {
                let now = rows[tick][repository].ln();
                let next = rows[tick + 1][repository].ln();
                rows[tick][repository] = now.min(next + (now - next) * blend).exp();
            }
        }
        for tick in 1..rows.len() {
            if rows[tick][repository] > 0.0 && rows[tick - 1][repository] > 0.0 {
                let now = rows[tick][repository].ln();
                let previous = rows[tick - 1][repository].ln();
                rows[tick][repository] = now
                    .min(previous + inward)
                    .min(previous + (now - previous) * blend)
                    .exp();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_fit_jitter_does_not_make_the_zoom_oscillate() {
        let mut rows: Vec<_> = (0..1200)
            .map(|tick| vec![if tick % 2 == 0 { 1.0 } else { 1.01 }])
            .collect();
        plan(&mut rows);
        for row in &rows[300..900] {
            assert!((row[0] - 1.0).abs() < 0.0002);
        }
    }

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
        assert!(planned[30][0] < 1.2);
        assert!(planned[899][0] > planned[600][0] * 1.2);
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
