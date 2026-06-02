#![allow(clippy::doc_markdown)]

//! Controller layer. Wraps the transport with domain-specific behaviour
//! (path normalisation, JMESPath filtering, output formatting) while keeping
//! tool/CLI handlers as thin adapters.

pub mod api;
pub mod circleci;
pub mod clone;
pub mod postman;
pub mod slack;
pub mod zoom;

pub use api::{BitbucketContext, ControllerResponse, HandleContext, handle_request};
pub use circleci::CircleCiContext;
pub use clone::handle_clone;
pub use postman::PostmanContext;
pub use slack::SlackContext;
pub use zoom::ZoomContext;
