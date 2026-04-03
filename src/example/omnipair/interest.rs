//! Interest accrual projection — mirrors on-chain `Pair::update()` between swaps.
//!

use super::state::{ceil_div, OmnipairPair, OmnipairRateModel, BPS_DENOMINATOR};

const NAD: u64 = 1_000_000_000;
const TARGET_MS_PER_SLOT: u64 = 400;
const NATURAL_LOG_OF_TWO_NAD: u64 = 693_147_180;
const TAYLOR_TERMS: u64 = 5;
const MILLISECONDS_PER_YEAR: u64 = 31_536_000_000;

pub(crate) fn slots_to_ms(start_slot: u64, end_slot: u64) -> Option<u64> {
    end_slot
        .checked_sub(start_slot)?
        .checked_mul(TARGET_MS_PER_SLOT)
}

fn taylor_exp(x: i64, scale: u64, precision: u64) -> u64 {
    let is_negative = x < 0;
    let abs_x = if is_negative { -x } else { x };

    let n = 10u64;
    let reduced_x = abs_x / (n as i64);

    let mut term = scale as u128;
    let mut sum = scale as u128;

    for i in 1..=precision {
        term = term
            .checked_mul(reduced_x as u128)
            .and_then(|t| t.checked_div(i as u128 * scale as u128))
            .unwrap_or(0);
        sum = sum.checked_add(term).unwrap_or(u128::MAX);
    }

    let mut result = scale as u128;
    for _ in 0..n {
        result = result
            .checked_mul(sum)
            .and_then(|r| r.checked_div(scale as u128))
            .unwrap_or(u128::MAX);
    }

    if is_negative {
        result = (scale as u128 * scale as u128) / result;
    }

    result as u64
}

impl OmnipairRateModel {
    /// Returns `(current_rate_NAD, integral_NAD)` — mirrors on-chain `RateModel::calculate_rate`.
    pub fn calculate_rate(&self, last_rate: u64, time_elapsed: u64, last_util: u64) -> (u64, u64) {
        let dt = time_elapsed as u128;
        if dt == 0 {
            return (last_rate, 0);
        }

        let exp_rate = self.exp_rate as u128;
        let x = exp_rate.saturating_mul(dt);
        let gd = taylor_exp(-(x as i64), NAD, TAYLOR_TERMS) as u128;

        let min_nad = self.min_rate as u128;
        let max_nad = self.max_rate as u128;
        let has_max_cap = max_nad > 0;
        let last = (last_rate as u128).max(min_nad);

        if (last_util as u128) > (self.target_util_end as u128) {
            let curr_unclamped = last.saturating_mul(NAD as u128) / gd.max(1);

            if has_max_cap && curr_unclamped > max_nad {
                if last >= max_nad {
                    let integral = ceil_div(
                        max_nad.saturating_mul(dt),
                        MILLISECONDS_PER_YEAR as u128,
                    )
                    .unwrap_or(
                        max_nad.saturating_mul(dt) / (MILLISECONDS_PER_YEAR as u128),
                    );
                    return (
                        max_nad.min(u64::MAX as u128) as u64,
                        integral.min(u64::MAX as u128) as u64,
                    );
                }
                let t_to_max =
                    Self::time_to_reach_closed_form(last, max_nad, exp_rate, true).min(dt);
                let exp_part = ceil_div(
                    max_nad.saturating_sub(last).saturating_mul(NAD as u128),
                    exp_rate,
                )
                .unwrap_or(
                    max_nad
                        .saturating_sub(last)
                        .saturating_mul(NAD as u128)
                        / exp_rate,
                );
                let flat_part = max_nad.saturating_mul(dt.saturating_sub(t_to_max));
                let integral = ceil_div(exp_part + flat_part, MILLISECONDS_PER_YEAR as u128)
                    .unwrap_or((exp_part + flat_part) / (MILLISECONDS_PER_YEAR as u128));
                return (
                    max_nad.min(u64::MAX as u128) as u64,
                    integral.min(u64::MAX as u128) as u64,
                );
            }

            let curr = curr_unclamped;
            let numer = curr.saturating_sub(last).saturating_mul(NAD as u128);
            let integral_pre = numer / exp_rate;
            let integral = ceil_div(integral_pre, MILLISECONDS_PER_YEAR as u128)
                .unwrap_or(integral_pre / (MILLISECONDS_PER_YEAR as u128));
            return (
                curr.min(u64::MAX as u128) as u64,
                integral.min(u64::MAX as u128) as u64,
            );
        }

        if (last_util as u128) < (self.target_util_start as u128) {
            let r1_unclamped = last.saturating_mul(gd) / (NAD as u128);

            if r1_unclamped >= min_nad {
                let curr = r1_unclamped;
                let numer = last.saturating_sub(curr).saturating_mul(NAD as u128);
                let integral_pre = numer / exp_rate;
                let integral = ceil_div(integral_pre, MILLISECONDS_PER_YEAR as u128)
                    .unwrap_or(integral_pre / (MILLISECONDS_PER_YEAR as u128));
                return (
                    curr.min(u64::MAX as u128) as u64,
                    integral.min(u64::MAX as u128) as u64,
                );
            } else {
                if last <= min_nad {
                    let integral = ceil_div(
                        min_nad.saturating_mul(dt),
                        MILLISECONDS_PER_YEAR as u128,
                    )
                    .unwrap_or(min_nad.saturating_mul(dt) / (MILLISECONDS_PER_YEAR as u128));
                    return (
                        min_nad.min(u64::MAX as u128) as u64,
                        integral.min(u64::MAX as u128) as u64,
                    );
                }
                let t_to_min =
                    Self::time_to_reach_closed_form(last, min_nad, exp_rate, false).min(dt);
                let exp_part = ceil_div(
                    last.saturating_sub(min_nad).saturating_mul(NAD as u128),
                    exp_rate,
                )
                .unwrap_or(
                    last.saturating_sub(min_nad).saturating_mul(NAD as u128) / exp_rate,
                );
                let flat_part = min_nad.saturating_mul(dt.saturating_sub(t_to_min));
                let integral = ceil_div(exp_part + flat_part, MILLISECONDS_PER_YEAR as u128)
                    .unwrap_or((exp_part + flat_part) / (MILLISECONDS_PER_YEAR as u128));
                return (
                    min_nad.min(u64::MAX as u128) as u64,
                    integral.min(u64::MAX as u128) as u64,
                );
            }
        }

        let integral = ceil_div(last.saturating_mul(dt), MILLISECONDS_PER_YEAR as u128)
            .unwrap_or(last.saturating_mul(dt) / (MILLISECONDS_PER_YEAR as u128));
        (
            last.min(u64::MAX as u128) as u64,
            integral.min(u64::MAX as u128) as u64,
        )
    }

    fn time_to_reach_closed_form(r0: u128, target: u128, exp_rate: u128, up: bool) -> u128 {
        if up {
            if target <= r0 {
                return 0;
            }
            let ratio_nad = (target.saturating_mul(NAD as u128)) / r0.max(1);
            let ratio_nad_u64 = u64::try_from(ratio_nad).unwrap_or(u64::MAX);
            if ratio_nad_u64 == 0 {
                return 0;
            }
            let ln_ratio = Self::ln_nad(ratio_nad_u64);
            let t = ln_ratio / (exp_rate as i128);
            if t <= 0 { 0 } else { t as u128 }
        } else {
            if r0 <= target {
                return 0;
            }
            let ratio_nad = (r0.saturating_mul(NAD as u128)) / target.max(1);
            let ratio_nad_u64 = u64::try_from(ratio_nad).unwrap_or(u64::MAX);
            if ratio_nad_u64 == 0 {
                return 0;
            }
            let ln_ratio = Self::ln_nad(ratio_nad_u64);
            let t = ln_ratio / (exp_rate as i128);
            if t <= 0 { 0 } else { t as u128 }
        }
    }

    fn ln_nad(x_nad: u64) -> i128 {
        assert!(x_nad > 0, "ln_nad: x must be > 0");
        let mut z = x_nad as u128;
        let mut k: i128 = 0;

        while z < (NAD as u128) / 2 {
            z = z.saturating_mul(2);
            k -= 1;
        }
        while z >= (NAD as u128) * 2 {
            z /= 2;
            k += 1;
        }

        let z_i = z as i128;
        let num = (z_i - NAD as i128) * NAD as i128;
        let den = (z_i + NAD as i128).max(1);
        let v = num / den;

        let v2 = (v * v) / (NAD as i128);
        let v3 = (v2 * v) / (NAD as i128);
        let v5 = (v3 * v2) / (NAD as i128);
        let v7 = (v5 * v2) / (NAD as i128);
        let v9 = (v7 * v2) / (NAD as i128);

        let series = v + v3 / 3 + v5 / 5 + v7 / 7 + v9 / 9;

        let ln2 = NATURAL_LOG_OF_TWO_NAD as i128;
        2 * series + k * ln2
    }
}

impl OmnipairPair {
    /// Projects reserves to `current_slot` using the rate model.
    pub fn simulate_update(
        &mut self,
        current_slot: u64,
        rate_model: &OmnipairRateModel,
        interest_bps: u16,
    ) {
        if current_slot <= self.last_update {
            return;
        }

        let time_elapsed = match slots_to_ms(self.last_update, current_slot) {
            Some(t) if t > 0 => t,
            _ => return,
        };

        let nad = NAD as u128;

        let util0 = match self.reserve0 {
            0 => 0,
            _ => u64::try_from((self.total_debt0 as u128 * nad) / self.reserve0 as u128)
                .unwrap_or(u64::MAX),
        };
        let util1 = match self.reserve1 {
            0 => 0,
            _ => u64::try_from((self.total_debt1 as u128 * nad) / self.reserve1 as u128)
                .unwrap_or(u64::MAX),
        };

        let (new_rate0, integral0) =
            rate_model.calculate_rate(self.last_rate0, time_elapsed, util0);
        let (new_rate1, integral1) =
            rate_model.calculate_rate(self.last_rate1, time_elapsed, util1);

        self.last_rate0 = new_rate0;
        self.last_rate1 = new_rate1;

        let total_interest0 =
            ceil_div(self.total_debt0 as u128 * integral0 as u128, nad).unwrap_or(0);
        let total_interest1 =
            ceil_div(self.total_debt1 as u128 * integral1 as u128, nad).unwrap_or(0);

        let protocol_fee0 =
            u64::try_from((total_interest0 * interest_bps as u128) / BPS_DENOMINATOR)
                .unwrap_or(u64::MAX);
        let protocol_fee1 =
            u64::try_from((total_interest1 * interest_bps as u128) / BPS_DENOMINATOR)
                .unwrap_or(u64::MAX);

        let lp_interest0 = u64::try_from(total_interest0).unwrap_or(u64::MAX);
        let lp_interest1 = u64::try_from(total_interest1).unwrap_or(u64::MAX);

        let total_borrower_cost0 = total_interest0
            .checked_add(protocol_fee0 as u128)
            .unwrap_or(u128::MAX);
        let total_borrower_cost1 = total_interest1
            .checked_add(protocol_fee1 as u128)
            .unwrap_or(u128::MAX);

        self.total_debt0 = self
            .total_debt0
            .saturating_add(u64::try_from(total_borrower_cost0).unwrap_or(u64::MAX));
        self.total_debt1 = self
            .total_debt1
            .saturating_add(u64::try_from(total_borrower_cost1).unwrap_or(u64::MAX));

        let cash_covered_fee0 = protocol_fee0.min(self.cash_reserve0);
        let cash_covered_fee1 = protocol_fee1.min(self.cash_reserve1);

        self.reserve0 = self
            .reserve0
            .saturating_add(lp_interest0 + (protocol_fee0 - cash_covered_fee0));
        self.reserve1 = self
            .reserve1
            .saturating_add(lp_interest1 + (protocol_fee1 - cash_covered_fee1));

        self.cash_reserve0 -= cash_covered_fee0;
        self.cash_reserve1 -= cash_covered_fee1;

        self.last_update = current_slot;
    }
}
