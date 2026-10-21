//! 衰减加权的可靠性统计。
//!
//! 健康状态机回答的是「现在能不能用」（连续 2 次失败 → unavailable），
//! 它看不见「长期成功率」：一个每天挂两次但每次都很快恢复的模型，
//! 和一个一个月只挂过一次的模型，在状态机里可能一模一样。
//!
//! 这里用一个 Beta 后验补上这一层：每次成功/失败都是带时间戳的样本，
//! 时间越久权重越低（**半衰期 2 天**），所以模型修好之后几天就能爬回来，
//! 而一次历史事故也不会被永久记住。
//!
//! 为什么是「期望值」而不是 Thompson 抽样：这里给 SWRR 供权重，
//! 平滑轮转需要稳定的权重；每请求重新抽样会让同一份候选顺序抖动，
//! 反而把「平滑」破坏掉。探索留给 Pick 层那 8%。

/// 样本权重减半所需的时间。
pub const HALF_LIFE_MS: f64 = 2.0 * 86_400_000.0;
/// 后验的先验：Beta(1,1)（均匀）。与「没有样本」区分开——
/// 没有样本返回 `None`（没意见），而不是返回 0.5。
const PRIOR: f64 = 1.0;
/// 有效样本少于此值就不给意见：一两次请求说明不了什么，
/// 而一个 0/1 的结论足以让好模型被压下去。
const MIN_WEIGHT: f64 = 4.0;

/// 记一次样本。先把旧权重衰减到此刻，再累加 1。
pub fn note(ok: bool, at_ms: i64, ok_w: &mut f64, fail_w: &mut f64, decay_at: &mut i64) {
    if *decay_at > 0 && at_ms > *decay_at {
        let factor = 0.5_f64.powf((at_ms - *decay_at) as f64 / HALF_LIFE_MS);
        *ok_w *= factor;
        *fail_w *= factor;
    }
    if ok {
        *ok_w += 1.0;
    } else {
        *fail_w += 1.0;
    }
    *decay_at = at_ms;
}

/// 把权重衰减到 `now`，返回 `(α, β)`。有效样本不足时返回 `None`。
pub fn posterior(ok_w: f64, fail_w: f64, decay_at: i64, now_ms: i64) -> Option<(f64, f64)> {
    let factor = if decay_at > 0 && now_ms > decay_at {
        0.5_f64.powf((now_ms - decay_at) as f64 / HALF_LIFE_MS)
    } else {
        1.0
    };
    let w = (ok_w + fail_w) * factor;
    if !w.is_finite() || w < MIN_WEIGHT {
        return None;
    }
    Some((ok_w * factor + PRIOR, fail_w * factor + PRIOR))
}

/// 后验期望 `α/(α+β)`：稳定、可排序，给权重用。
pub fn expected(ok_w: f64, fail_w: f64, decay_at: i64, now_ms: i64) -> Option<f64> {
    let (a, b) = posterior(ok_w, fail_w, decay_at, now_ms)?;
    Some(a / (a + b))
}

/// 后验抽样：单测与未来「按不确定性探索」用。
pub fn sample(ok_w: f64, fail_w: f64, decay_at: i64, now_ms: i64, seed: u64) -> Option<f64> {
    let (a, b) = posterior(ok_w, fail_w, decay_at, now_ms)?;
    let mut rng = Rng::new(seed);
    let x = gamma(a, &mut rng);
    let y = gamma(b, &mut rng);
    if x + y <= 0.0 {
        return None;
    }
    Some(x / (x + y))
}

/// xorshift64*：不引依赖，够用。种子由调用方给，测试可复现。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// (0, 1)
    fn unit(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 2.0)
    }

    /// Box–Muller，标准正态
    fn normal(&mut self) -> f64 {
        let u1 = self.unit();
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// Marsaglia–Tsang 抽 Gamma(shape, 1)。
fn gamma(shape: f64, rng: &mut Rng) -> f64 {
    if shape <= 0.0 {
        return 0.0;
    }
    if shape < 1.0 {
        // 提升到 shape+1 再降回来，避免 shape 很小时的数值问题
        let u = rng.unit();
        return gamma(shape + 1.0, rng) * u.powf(1.0 / shape);
    }
    let d = shape - 1.0 / 3.0;
    let c = 1.0 / (9.0 * d).sqrt();
    loop {
        let x = rng.normal();
        let v = (1.0 + c * x).powi(3);
        if v <= 0.0 {
            continue;
        }
        let u = rng.unit();
        if u < 1.0 - 0.0331 * x.powi(4) {
            return d * v;
        }
        if u.ln() < 0.5 * x * x + d * (1.0 - v + v.ln()) {
            return d * v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;

    #[test]
    fn samples_need_evidence_before_they_count() {
        let mut ok = 0.0;
        let mut fail = 0.0;
        let mut at = 0;
        note(true, 1_000, &mut ok, &mut fail, &mut at);
        assert!(posterior(ok, fail, at, 1_000).is_none(), "一次成功不代表什么");
        for i in 0..5 {
            note(true, 2_000 + i * 1_000, &mut ok, &mut fail, &mut at);
        }
        let p = expected(ok, fail, at, 10_000).expect("now there is evidence");
        assert!(p > 0.7 && p < 1.0);
    }

    #[test]
    fn weight_halves_every_two_days() {
        let mut ok = 0.0;
        let mut fail = 0.0;
        let mut at = 0;
        let t0 = 1_000_000;
        for i in 0..9 {
            note(true, t0 + i, &mut ok, &mut fail, &mut at);
        }
        assert!((ok - 9.0).abs() < 0.01, "毫秒级的衰减可以忽略，实际 {ok}");
        // 两整天之后：9 → 4.5（外加先验 1）
        let (a, b) = posterior(ok, fail, at, at + 2 * DAY).unwrap();
        assert!((a - 5.5).abs() < 0.01, "9*0.5 + 先验 1，实际 {a}");
        assert!((b - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_bad_history_costs_less_as_it_ages() {
        let mut ok = 0.0;
        let mut fail = 0.0;
        let mut at = 0;
        let t0 = 1_000_000;
        for i in 0..12 {
            note(false, t0 + i, &mut ok, &mut fail, &mut at);
        }
        let bad = expected(ok, fail, at, at + DAY).unwrap();
        assert!(bad < 0.15, "刚挂 12 次应当很低，实际 {bad}");

        // 三天无事故：失败权重衰减到 1/2.8，后验往先验回
        let better = expected(ok, fail, at, at + 3 * DAY).unwrap();
        assert!(better > bad, "实际 {bad} → {better}");

        // 两周无事故：样本权重已经低于门槛，宁可「没意见」也不留一个过期的坏印象
        assert!(
            posterior(ok, fail, at, at + 14 * DAY).is_none(),
            "陈旧样本应当自动失去话语权"
        );
    }

    #[test]
    fn sampling_lands_near_the_expected_value() {
        let mut ok = 0.0;
        let mut fail = 0.0;
        let mut at = 0;
        for i in 0..40 {
            note(true, i, &mut ok, &mut fail, &mut at);
        }
        for i in 0..10 {
            note(false, 100 + i, &mut ok, &mut fail, &mut at);
        }
        let e = expected(ok, fail, at, at).unwrap();
        let mut sum = 0.0;
        for s in 0..400 {
            sum += sample(ok, fail, at, at, s * 7919 + 13).unwrap();
        }
        let avg = sum / 400.0;
        assert!((avg - e).abs() < 0.05, "抽样均值 {avg} 应当接近期望 {e}");
    }
}
