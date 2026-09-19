//! Routing — 04-models.md §2.4.
//!
//! * [`dijkstra`] — `mobility/routing/dijkstra`: shortest path on the lane graph with edge
//!   cost = length / speed limit, honouring turn restrictions and lane-class masks.
//! * [`dynamic`] — `mobility/routing/dynamic-reroute`: the same search, re-run at the next
//!   junction when a closure or a travel-time update changes the best route, plus the
//!   [`crate::views::EdgeCost`] implementations that carry closures and measured times.

pub mod dijkstra;
pub mod dynamic;

pub use dijkstra::{Dijkstra, DijkstraParams, FreeFlowCost};
pub use dynamic::{DynamicCost, DynamicReroute};
