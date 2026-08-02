//! Kestrel's control surface.
//!
//! One [`Shared`] holds the show, the live runtime facts and a revision
//! counter. The GUI, the render loop and the HTTP/WebSocket server all go
//! through it, so a crosspoint taken on a Stream Deck and one taken by dragging
//! in the UI are the same operation on the same state.

pub mod server;
pub mod state;

pub use server::{router, serve};
pub use state::{
    EnableBody, FormatView, InputView, OutputBody, OutputView, Reply, RoiBody, RoiView, RouteBody,
    Runtime, Shared, StateView,
};
