//! Types shared across formats.
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Pcm { pub rate: u32, pub data: Vec<i16> }
