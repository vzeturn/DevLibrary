//! Stand-ins for platforms without COM: every call fails with [`Error::Unsupported`].

use crate::Error;
use crate::com::{InvokeKind, Variant};

// Never constructed: every function that would return one fails first.
#[allow(dead_code)]
pub(crate) struct Init(());

// Never constructed, as above.
#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct Dispatch(());

impl Dispatch {
    pub(crate) fn addr(&self) -> usize {
        0
    }
}

pub(crate) fn init() -> Result<Init, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn is_multithreaded(_i: &Init) -> bool {
    false
}

pub(crate) fn create(_prog_id: &[u16], _name: &str) -> Result<Dispatch, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn dispid(_d: &Dispatch, _wide: &[u16], _name: &str) -> Result<i32, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn invoke(
    _d: &Dispatch,
    _id: i32,
    _name: &str,
    _kind: InvokeKind,
    _args: &[Variant],
) -> Result<Variant, Error> {
    Err(Error::Unsupported)
}
