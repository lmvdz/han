//! The session budget at the proxy (SR-41). A model call is admitted only if its worst-case cost fits what
//! is left after the spend so far and the calls still in flight. The check and the reservation are one
//! step under one lock, so concurrent calls cannot overshoot the budget. A call then settles (its actual
//! cost replaces the reservation) or, if it ends abnormally, releases.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use super::usage::Usage;

/// The default cap on output tokens for a request that sets no `max_tokens`. Deliberately high, so the
/// reservation errs towards refusing rather than overshooting.
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 32_768;

/// What a model call can cost at most, as far as the proxy can tell before forwarding it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallBound {
    /// The `model` the request names, if any.
    pub model: Option<String>,
    /// An upper estimate of the input tokens (the proxy uses two bytes of request body per token).
    pub input_tokens: u64,
    /// The `max_tokens` the request sets, if any.
    pub max_output: Option<u64>,
}

/// Prices in budget units per token. A budget's unit is whatever its limit is in (for example micro-dollars).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Price {
    /// Per input token.
    pub input: u64,
    /// Per output token.
    pub output: u64,
    /// Per input token read from the prompt cache.
    pub cache_read: u64,
    /// Per input token written to the prompt cache.
    pub cache_creation: u64,
}

impl Price {
    /// The cost of what a call used.
    pub fn cost(&self, u: &Usage) -> u64 {
        u.input
            .saturating_mul(self.input)
            .saturating_add(u.output.saturating_mul(self.output))
            .saturating_add(u.cache_read.saturating_mul(self.cache_read))
            .saturating_add(u.cache_creation.saturating_mul(self.cache_creation))
    }

    /// The most a call can cost: every input token at the dearer of the input prices, and the output cap.
    fn worst(&self, input_tokens: u64, max_output: u64) -> u64 {
        input_tokens
            .saturating_mul(self.input.max(self.cache_creation))
            .saturating_add(max_output.saturating_mul(self.output))
    }
}

/// What a reservation holds. Hand it back to [`Budget::settle`] or [`Budget::release`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hold {
    /// The amount reserved; 0 when the call has no price.
    pub amount: u64,
    /// The price the call is settled at; `None` when the call has no price.
    pub price: Option<Price>,
}

impl Hold {
    /// A hold that reserves nothing.
    pub fn none() -> Self {
        Hold::default()
    }
}

/// Why a call is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The budget is already spent (or fully reserved).
    Spent,
    /// The call's worst-case cost does not fit what is left.
    WouldExceed,
}

impl Refusal {
    /// The reason written to the decision log and the `x-han-refusal` header.
    pub fn reason(self) -> &'static str {
        match self {
            Refusal::Spent => "budget-spent",
            Refusal::WouldExceed => "budget-would-exceed",
        }
    }
}

/// The budget the proxy asks before each model call (SR-41).
pub trait Budget: Send + Sync + 'static {
    /// Whether the session's budget is spent.
    fn exhausted(&self) -> bool;
    /// Records what one call used.
    fn record(&self, usage: &Usage);
    /// Admits a call and reserves its worst-case cost, or refuses it. The default admits while the budget
    /// is not spent and reserves nothing.
    fn reserve(&self, _call: &CallBound) -> Result<Hold, Refusal> {
        if self.exhausted() {
            Err(Refusal::Spent)
        } else {
            Ok(Hold::none())
        }
    }
    /// Ends a reservation and records what the call used.
    fn settle(&self, _hold: Hold, usage: &Usage) {
        self.record(usage);
    }
    /// Ends a reservation without a result (the call never completed).
    fn release(&self, _hold: Hold) {}
}

/// A reservation that releases itself when dropped unless it was settled, so a call cancelled at any
/// point cannot leak what it held.
pub(super) struct Held {
    slot: Option<(Arc<dyn Budget>, Hold)>,
}

impl Held {
    pub(super) fn none() -> Self {
        Held { slot: None }
    }

    pub(super) fn new(budget: Arc<dyn Budget>, hold: Hold) -> Self {
        Held {
            slot: Some((budget, hold)),
        }
    }

    pub(super) fn settle(mut self, usage: &Usage) {
        if let Some((b, h)) = self.slot.take() {
            b.settle(h, usage);
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some((b, h)) = self.slot.take() {
            b.release(h);
        }
    }
}

#[derive(Default)]
struct State {
    spent: u64,
    reserved: u64,
}

/// A budget with a limit and per-model prices. Reservations apply only to models that have a price: a call
/// to an unpriced model (and any subscription-mode session, which has no per-call price) reserves nothing
/// and adds nothing to the spend, and is refused only once the priced spend has used up the limit.
pub struct Ledger {
    limit: u64,
    prices: HashMap<String, Price>,
    default_max_output: u64,
    state: Mutex<State>,
}

impl Ledger {
    /// A ledger with this limit and these prices, keyed by model name. A request's model matches the
    /// longest key that is a prefix of it, so `claude-sonnet-4-5` covers dated variants.
    pub fn new(limit: u64, prices: HashMap<String, Price>) -> Self {
        Ledger {
            limit,
            prices,
            default_max_output: DEFAULT_MAX_OUTPUT_TOKENS,
            state: Mutex::new(State::default()),
        }
    }

    /// Sets the output cap assumed for a request without `max_tokens`.
    pub fn with_default_max_output(mut self, tokens: u64) -> Self {
        self.default_max_output = tokens;
        self
    }

    /// What has been spent.
    pub fn spent(&self) -> u64 {
        self.lock().spent
    }

    /// What in-flight calls hold.
    pub fn reserved(&self) -> u64 {
        self.lock().reserved
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn price(&self, model: &str) -> Option<Price> {
        self.prices
            .iter()
            .filter(|(k, _)| model.starts_with(k.as_str()))
            .max_by_key(|(k, _)| k.len())
            .map(|(_, p)| *p)
    }
}

impl Budget for Ledger {
    fn exhausted(&self) -> bool {
        let s = self.lock();
        s.spent.saturating_add(s.reserved) >= self.limit
    }

    fn record(&self, _usage: &Usage) {
        // spend is added in `settle`, at the price the call was reserved with
    }

    fn reserve(&self, call: &CallBound) -> Result<Hold, Refusal> {
        let price = call.model.as_deref().and_then(|m| self.price(m));
        let mut s = self.lock();
        let used = s.spent.saturating_add(s.reserved);
        if used >= self.limit {
            return Err(Refusal::Spent);
        }
        let Some(price) = price else {
            return Ok(Hold::none());
        };
        let max_output = call.max_output.unwrap_or(self.default_max_output);
        let worst = price.worst(call.input_tokens, max_output);
        if used.saturating_add(worst) > self.limit {
            return Err(Refusal::WouldExceed);
        }
        s.reserved += worst;
        Ok(Hold {
            amount: worst,
            price: Some(price),
        })
    }

    fn settle(&self, hold: Hold, usage: &Usage) {
        let mut s = self.lock();
        s.reserved = s.reserved.saturating_sub(hold.amount);
        if let Some(p) = hold.price {
            s.spent = s.spent.saturating_add(p.cost(usage));
        }
    }

    fn release(&self, hold: Hold) {
        let mut s = self.lock();
        s.reserved = s.reserved.saturating_sub(hold.amount);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Price = Price {
        input: 1,
        output: 5,
        cache_read: 0,
        cache_creation: 2,
    };

    fn ledger(limit: u64) -> Ledger {
        Ledger::new(limit, HashMap::from([("m".to_owned(), P)])).with_default_max_output(100)
    }

    fn call(max: Option<u64>) -> CallBound {
        CallBound {
            model: Some("m-2025".into()),
            input_tokens: 10,
            max_output: max,
        }
    }

    #[test]
    fn a_missing_max_tokens_uses_the_default_cap() {
        let l = ledger(1_000);
        // 10 input at the dearer input price (2) + 100 default output at 5
        let h = l.reserve(&call(None)).unwrap();
        assert_eq!(h.amount, 10 * 2 + 100 * 5);
        let h2 = l.reserve(&call(Some(20))).unwrap();
        assert_eq!(h2.amount, 10 * 2 + 20 * 5);
        assert_eq!(l.reserved(), h.amount + h2.amount);
    }

    #[test]
    fn the_check_and_the_reservation_share_a_step() {
        let l = ledger(300);
        // each call holds 20 + 50 = 70; four fit (280), the fifth would exceed
        let held: Vec<_> = (0..4)
            .map(|_| l.reserve(&call(Some(10))).unwrap())
            .collect();
        assert_eq!(l.reserve(&call(Some(10))), Err(Refusal::WouldExceed));
        l.release(held[0]);
        assert!(l.reserve(&call(Some(10))).is_ok());
    }

    #[test]
    fn settle_replaces_the_reservation_with_the_actual_cost() {
        let l = ledger(1_000);
        let h = l.reserve(&call(Some(10))).unwrap();
        l.settle(
            h,
            &Usage {
                input: 8,
                output: 3,
                requests: 1,
                ..Usage::default()
            },
        );
        assert_eq!((l.reserved(), l.spent()), (0, 8 + 15));
    }

    #[test]
    fn an_unpriced_model_reserves_nothing() {
        let l = ledger(1_000);
        let mut c = call(None);
        c.model = Some("other".into());
        assert_eq!(l.reserve(&c).unwrap(), Hold::none());
        assert_eq!(l.reserved(), 0);
    }

    #[test]
    fn a_dropped_hold_releases() {
        let l = Arc::new(ledger(1_000));
        let b: Arc<dyn Budget> = l.clone();
        let h = b.reserve(&call(None)).unwrap();
        assert!(l.reserved() > 0);
        drop(Held::new(b, h));
        assert_eq!(l.reserved(), 0);
    }
}
