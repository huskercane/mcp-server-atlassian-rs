#![allow(clippy::doc_markdown)]

//! Controller layer. Wraps the transport with domain-specific behaviour
//! (path normalisation, JMESPath filtering, output formatting) while keeping
//! tool/CLI handlers as thin adapters.

pub mod api;
pub mod clone;
pub mod zoom;

pub use api::{BitbucketContext, ControllerResponse, HandleContext, handle_request};
pub use clone::handle_clone;
pub use zoom::ZoomContext;
