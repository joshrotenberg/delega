//! Tests for the process layer, against small fake children (`/bin/sh`
//! scripts). No provider program is involved, and every test checks that no
//! process it started is left running.

mod cleanup;
mod endings;
mod fixtures;
mod gaps;
mod io;
mod ported;
