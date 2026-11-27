//! Platforms without a TLS backend (macOS for now, WebAssembly).

use std::io::{self, Read, Write};

use std::sync::Arc;

use crate::{Config, Error, ServerConfig, Version};

const UNSUPPORTED: &str = "no TLS backend for this platform";

/// Uninhabited: `new` always fails, so no server credential ever exists.
pub(crate) enum ServerCred {}

impl ServerCred {
    pub(crate) fn new(_cfg: &ServerConfig) -> Result<ServerCred, Error> {
        Err(Error::Unsupported(UNSUPPORTED))
    }
}

/// Uninhabited: `connect` always fails and `accept` needs a credential, so no session
/// ever exists.
pub(crate) enum Session {}

impl Session {
    pub(crate) fn connect<S: Read + Write>(_cfg: &Config, _s: &mut S) -> Result<Session, Error> {
        Err(Error::Unsupported(UNSUPPORTED))
    }

    pub(crate) fn accept<S: Read + Write>(
        cred: &Arc<ServerCred>,
        _s: &mut S,
        _recv: Vec<u8>,
        _alpn: &[Vec<u8>],
    ) -> Result<Session, Error> {
        match **cred {}
    }

    pub(crate) fn read<S: Read + Write>(&mut self, _s: &mut S, _b: &mut [u8]) -> io::Result<usize> {
        match *self {}
    }

    pub(crate) fn write<S: Read + Write>(&mut self, _s: &mut S, _b: &[u8]) -> io::Result<usize> {
        match *self {}
    }

    pub(crate) fn shutdown<S: Read + Write>(&mut self, _s: &mut S) -> io::Result<()> {
        match *self {}
    }

    pub(crate) fn take_leftover(&mut self) -> Vec<u8> {
        match *self {}
    }

    pub(crate) fn version(&self) -> Option<Version> {
        match *self {}
    }

    pub(crate) fn alpn_protocol(&self) -> Option<&[u8]> {
        match *self {}
    }

    pub(crate) fn peer_certificate_der(&self) -> Option<Vec<u8>> {
        match *self {}
    }
}
