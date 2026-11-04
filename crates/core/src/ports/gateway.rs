//! A gateway as kelpie asks it: the models it serves, whether it takes the
//! key, and a lease on a model for a long call
//!
//! Kelpie reaches a gateway only through [`ModelGateway`]. Each
//! [`GatewayKind`](crate::settings::GatewayKind) has one adapter, and
//! `adapters::port` hands it out. Chat is
//! plain OpenAI and goes through the forwarder, so the port holds the rest.

use std::fmt;

use crate::forwarder::Upstream;
use crate::ports::{Ending, Metered};

/// What a gateway answers to kelpie's key
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCheck {
    /// The gateway takes it
    Taken,
    /// The gateway answered that it does not
    Refused,
    /// The gateway could not be asked, or its answer says neither, with why
    Unsure(String),
}

/// Where a take stands, told to whoever waits on it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stand {
    /// The take is about to be asked
    Asking,
    /// The gateway turned it away because the model's turn is another
    /// lease's: it waits in line, as for a lock
    Turn,
    /// The gateway turned it away for another reason, such as a load
    Busy,
    /// The lease is held
    Granted,
}

/// What a call or round needs to hold a lease on its model
#[derive(Clone, Copy)]
pub struct Taking<'a> {
    /// The model's name on the gateway
    pub model: &'a str,
    /// What the gateway shows beside the lease
    pub note: &'a str,
    /// Says when the wait for the grant should end
    pub give_up: &'a dyn Fn() -> bool,
    /// Told where the take stands, at each change
    pub tell: &'a dyn Fn(Stand),
    /// Asked to end the call if the lease is lost
    pub ending: &'a Ending,
}

impl fmt::Debug for Taking<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Taking")
            .field("model", &self.model)
            .field("note", &self.note)
            .finish_non_exhaustive()
    }
}

/// Why a call did not run under its lease
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unheld {
    /// `give_up` said to stop before the grant
    GaveUp,
    /// The gateway refused the take other than as busy, or could not be asked
    Refused(String),
    /// The gateway took the lease back while the call ran, with why
    Lost(String),
}

/// One kind of gateway, as kelpie asks it
pub trait ModelGateway: Send + Sync {
    /// The model names the gateway serves
    ///
    /// # Errors
    ///
    /// Why the gateway did not answer with its list.
    fn models(&self, gateway: &Upstream) -> Result<Vec<String>, String>;

    /// Whether the gateway takes the key `gateway` carries
    fn key(&self, gateway: &Upstream) -> KeyCheck;

    /// Runs `call` holding a lease on `taking.model`, taken before it and
    /// released after, and told the lease's id
    ///
    /// `None` when this kind offers no lease, and `call` has not run. `Some`
    /// when it was asked: `Err` when the lease was not taken or was lost,
    /// which is an error whatever `call` did, since it was asked to end. `Ok`
    /// holds what the release said the lease used, `None` when it said nothing.
    fn lease(
        &self,
        gateway: &Upstream,
        taking: &Taking<'_>,
        call: &mut dyn FnMut(&str),
    ) -> Option<Result<Option<Metered>, Unheld>>;

    /// Runs `call` holding the lease `id`, which an earlier image of the
    /// runner took and left held, and releases it after
    ///
    /// The lease is renewed at once and then as it was, so it is not lost
    /// between the images. As for [`ModelGateway::lease`]: `None` when this
    /// kind offers no lease, and `call` has not run, else `Err` when the lease
    /// was lost and `Ok` with what the release said it used.
    fn resume(
        &self,
        gateway: &Upstream,
        id: &str,
        ending: &Ending,
        call: &mut dyn FnMut(),
    ) -> Option<Result<Option<Metered>, Unheld>> {
        let _ = (gateway, id, ending, call);
        None
    }
}

impl dyn ModelGateway + '_ {
    /// Runs `call` under a lease on `taking.model`, or at once when this kind
    /// offers none
    ///
    /// # Errors
    ///
    /// [`Unheld`] when the lease was asked and not taken, or was lost.
    pub fn around<T>(
        &self,
        gateway: &Upstream,
        taking: &Taking<'_>,
        call: impl FnOnce() -> T,
    ) -> Result<T, Unheld> {
        self.around_leased(gateway, taking, |_| call())
            .map(|(done, _)| done)
    }

    /// Runs `call` under a lease on `taking.model`, told the lease's id, or at
    /// once with `None` when this kind offers none, and returns its value with
    /// what the lease used
    ///
    /// # Errors
    ///
    /// [`Unheld`] when the lease was asked and not taken, or was lost.
    pub fn around_leased<T>(
        &self,
        gateway: &Upstream,
        taking: &Taking<'_>,
        call: impl FnOnce(Option<&str>) -> T,
    ) -> Result<(T, Option<Metered>), Unheld> {
        let mut call = Some(call);
        let mut done = None;
        let mut run = |id: &str| done = call.take().map(|call| call(Some(id)));
        let metered = match self.lease(gateway, taking, &mut run) {
            Some(Err(unheld)) => return Err(unheld),
            Some(Ok(metered)) => metered,
            None => None,
        };
        // A kind with no lease left the call unrun.
        done.or_else(|| call.map(|call| call(None)))
            .map(|done| (done, metered))
            .ok_or_else(|| Unheld::Refused("the lease ran no call".into()))
    }

    /// Runs `call` under the lease `id` an earlier image took, or at once
    /// when this kind offers none, and returns its value with what the lease
    /// used
    ///
    /// # Errors
    ///
    /// [`Unheld`] when the lease was lost.
    pub fn around_resumed<T>(
        &self,
        gateway: &Upstream,
        id: &str,
        ending: &Ending,
        call: impl FnOnce() -> T,
    ) -> Result<(T, Option<Metered>), Unheld> {
        let mut call = Some(call);
        let mut done = None;
        let mut run = || done = call.take().map(|call| call());
        let metered = match self.resume(gateway, id, ending, &mut run) {
            Some(Err(unheld)) => return Err(unheld),
            Some(Ok(metered)) => metered,
            None => None,
        };
        done.or_else(|| call.map(|call| call()))
            .map(|done| (done, metered))
            .ok_or_else(|| Unheld::Refused("the lease ran no call".into()))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::settings::EndpointUrl;

    // A kind that serves nothing, takes any key and offers no lease.
    struct Leaseless;

    impl ModelGateway for Leaseless {
        fn models(&self, _: &Upstream) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }

        fn key(&self, _: &Upstream) -> KeyCheck {
            KeyCheck::Taken
        }

        fn lease(
            &self,
            _: &Upstream,
            _: &Taking<'_>,
            _: &mut dyn FnMut(&str),
        ) -> Option<Result<Option<Metered>, Unheld>> {
            None
        }
    }

    #[test]
    fn a_call_on_a_kind_with_no_lease_runs_at_once_and_is_told_of_nothing() {
        let base = EndpointUrl::try_from("http://gpu:8700/v1".to_owned()).unwrap();
        let upstream = Upstream::new(&base).unwrap();
        let told = Cell::new(false);
        let ending = Ending::default();
        let taking = Taking {
            model: "qwen",
            note: "n",
            give_up: &|| true,
            tell: &|_| told.set(true),
            ending: &ending,
        };
        let port: &dyn ModelGateway = &Leaseless;
        assert_eq!(port.around(&upstream, &taking, || 7), Ok(7));
        assert!(!told.get() && !ending.asked());
    }

    #[test]
    fn a_resumed_call_on_a_kind_with_no_lease_runs_at_once() {
        let base = EndpointUrl::try_from("http://gpu:8700/v1".to_owned()).unwrap();
        let upstream = Upstream::new(&base).unwrap();
        let ending = Ending::default();
        let port: &dyn ModelGateway = &Leaseless;
        let resumed = port.around_resumed(&upstream, "L1", &ending, || 7);
        assert_eq!(resumed, Ok((7, None)));
        assert!(!ending.asked());
    }
}
