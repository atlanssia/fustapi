//! Provider trait and adapter registry.
//!
//! Defines the `Provider` trait that all adapters implement, along with
//! unified types like `UnifiedRequest`, `Message`, and error types.

pub mod omlx;
pub mod lmstudio;
pub mod sglang;

pub mod cloud;
