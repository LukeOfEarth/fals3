// fals3-core: pure-Rust S3 simulator over a local filesystem.

pub mod error;
pub mod meta;
pub mod paths;
pub mod store;

pub use error::{Fals3Error, Result};
pub use meta::ObjectMeta;
pub use store::{IfConditions, Store};

#[cfg(test)]
mod tests;
