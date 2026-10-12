//! Portable source documents and exact copied-span mappings for Svelte tooling.
//!
//! This foundation does not yet parse, type-check or compile Svelte. It has no
//! filesystem, process, clock or TypeScript-core dependency. Native and WASM
//! interfaces use the same JSON protocol in [`protocol`].

pub mod compatibility;
pub mod document;
pub mod mapping;
pub mod protocol;

pub use document::{ByteRange, Document, Edit, Error, Position, Range, Snapshot};
pub use mapping::{Chunk, Mapping, Projection};
