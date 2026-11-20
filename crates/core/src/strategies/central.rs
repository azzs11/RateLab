//! Central exact: every request is checked by the store.

use super::{CoordinationStrategy, Message, MessageId, Outbox, RequestId, StoreOp, StoreReply};
use crate::breaker::FailurePolicy;
use crate::{Cost, Nanos};
use std::collections::HashMap;
use std::time::Duration;

/// Asks the store for every request: exactly the global limit, at the price of one
/// round trip per decision. When the store does not answer, the [`FailurePolicy`]
/// decides.
///
/// ```
/// use ratelab::strategies::{CentralExact, CoordinationStrategy, Outbox, Store};
/// use ratelab::{Cost, FailurePolicy, Nanos, RateLimitConfig};
///
/// let mut store = Store::new(RateLimitConfig::per_second(1)?);
/// let mut node = CentralExact::new(FailurePolicy::FailClosed, false);
/// let mut out = Outbox::default();
/// node.on_request(Nanos::ZERO, 7, "k", Cost::ONE, &mut out);
/// let msg = out.messages.pop().unwrap();
/// let reply = store.handle(Nanos::ZERO, &msg.key, &msg.op);
/// node.on_reply(Nanos::ZERO, msg.id, reply, &mut out);
/// assert_eq!(out.decisions[0].0, 7);
/// assert!(out.decisions[0].1.allowed);
/// # Ok::<(), ratelab::ConfigError>(())
/// ```
#[derive(Debug)]
pub struct CentralExact {
    policy: FailurePolicy,
    node_clock: bool,
    next: MessageId,
    pending: HashMap<MessageId, RequestId>,
}

impl CentralExact {
    /// A node that sends every request to the store. With `node_clock`, it sends its
    /// own time (as an application-clock script would) instead of letting the store use
    /// its clock.
    pub fn new(policy: FailurePolicy, node_clock: bool) -> Self {
        Self {
            policy,
            node_clock,
            next: 0,
            pending: HashMap::new(),
        }
    }
}

impl CoordinationStrategy for CentralExact {
    fn name(&self) -> &'static str {
        if self.node_clock {
            "central_exact_node_clock"
        } else {
            "central_exact"
        }
    }

    fn on_request(&mut self, now: Nanos, id: RequestId, key: &str, cost: Cost, out: &mut Outbox) {
        let msg = self.next;
        self.next += 1;
        self.pending.insert(msg, id);
        out.messages.push(Message {
            id: msg,
            key: key.to_owned(),
            op: StoreOp::Check {
                cost,
                at: self.node_clock.then_some(now),
            },
        });
    }

    fn on_reply(&mut self, _now: Nanos, id: MessageId, reply: StoreReply, out: &mut Outbox) {
        if let (Some(request), StoreReply::Checked(decision)) = (self.pending.remove(&id), reply) {
            out.decisions.push((request, decision));
        }
    }

    fn on_failure(&mut self, _now: Nanos, id: MessageId, out: &mut Outbox) {
        if let Some(request) = self.pending.remove(&id) {
            out.decisions
                .push((request, self.policy.decide(Duration::from_secs(1))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_applies_the_policy() {
        for (policy, allowed) in [
            (FailurePolicy::FailOpen, true),
            (FailurePolicy::FailClosed, false),
        ] {
            let mut node = CentralExact::new(policy, false);
            let mut out = Outbox::default();
            node.on_request(Nanos::ZERO, 1, "k", Cost::ONE, &mut out);
            let id = out.messages[0].id;
            node.on_failure(Nanos::ZERO, id, &mut out);
            assert_eq!(out.decisions.len(), 1);
            assert_eq!(out.decisions[0].1.allowed, allowed);
            // A late reply after the failure is ignored.
            node.on_reply(
                Nanos::ZERO,
                id,
                StoreReply::Checked(crate::Decision::allow(crate::Tokens(0), Duration::ZERO)),
                &mut out,
            );
            assert_eq!(out.decisions.len(), 1);
        }
    }

    #[test]
    fn node_clock_mode_sends_the_time() {
        let mut node = CentralExact::new(FailurePolicy::FailOpen, true);
        let mut out = Outbox::default();
        node.on_request(Nanos(42), 1, "k", Cost::ONE, &mut out);
        assert_eq!(
            out.messages[0].op,
            StoreOp::Check {
                cost: Cost::ONE,
                at: Some(Nanos(42))
            }
        );
        assert_eq!(node.name(), "central_exact_node_clock");
    }
}
