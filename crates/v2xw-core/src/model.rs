//! The base trait every plug-in family extends.
//!
//! 03-interfaces.md's conventions say it in one line: "Every trait extends `Model`, which
//! supplies the model card (§12). A plug-in without a model card cannot be registered."
//! This module is that trait. `Propagation`, `Mobility`, `Detector`, `Attacker`,
//! `MessageCodec` and the rest — each declared in its own crate — are written
//! `pub trait Propagation: Model { … }`, so a registered model always has a card and the
//! card is always reachable from the live object.
//!
//! # Why the card must be reachable from the object, not only from the registry
//!
//! The registry can hold a [`ModelCard`] on its own, and until now that is all it held.
//! But every service that has to explain a number — the `why` panel
//! ([`crate::provenance`]), the generated documentation site (ADR 0007 §3), the
//! conformance tracer that checks invariant I-C3 ("every numeric parameter a plug-in reads
//! must be declared in its model card") — starts from the *model that produced the value*
//! and needs its card. Going through the registry means carrying a [`ModelRef`] alongside
//! every trait object and trusting that the two were paired correctly. `card()` on the
//! object removes that opportunity for error: the card and the code that implements it are
//! one value.
//!
//! [`ModelRef`]: crate::registry::ModelRef
//!
//! # Dyn-compatible on purpose
//!
//! In-process Rust plug-ins are trait objects (ADR 0007 §8), so `Model` must be usable as
//! `dyn Model`: no generic methods, no `Self`-typed arguments or returns, no associated
//! constants. Every method here is `&self` returning a borrow or a `Copy` scalar, and the
//! crate's own tests build a `Box<dyn Model>` and an `Arc<dyn Model + Send + Sync>` so a
//! future addition that broke object safety would fail to compile here rather than in the
//! nine crates downstream.
//!
//! `Send + Sync` is not required by the trait itself — a model is free to be neither — but
//! [`ModelHandle`], the form the registry stores, does require it, because the registry is
//! shared by reference across the phase-parallel maps of 02-architecture.md §6.4.

use crate::card::{Family, ModelCard, Tier};

/// The base trait of every plug-in family: something that carries a model card.
///
/// Implementors hold their card (usually built once in the constructor or by a `card()`
/// free function in the same module) and return a borrow of it. Everything else here is
/// defaulted in terms of the card, so a family implementation is one method:
///
/// ```
/// use v2xw_core::card::{Family, ModelCard, Tier};
/// use v2xw_core::model::Model;
///
/// struct FreeSpace { card: ModelCard }
///
/// impl Model for FreeSpace {
///     fn card(&self) -> &ModelCard { &self.card }
/// }
///
/// let mut card = ModelCard::new("radio/propagation/free-space", Family::Propagation,
///                               "1.0.0", "Friis free-space path loss.");
/// card.tier = vec![Tier::Abstract, Tier::Medium];
/// let m = FreeSpace { card };
/// assert_eq!(m.tiers(), &[Tier::Abstract, Tier::Medium]);
/// assert!(m.implements_tier(Tier::Medium));
/// assert_eq!(m.id(), "radio/propagation/free-space");
/// ```
pub trait Model {
    /// This model's card: what it computes, from which parameters, with which sources
    /// (03-interfaces.md §12).
    ///
    /// The returned card must be the one that was registered — the registry hashes it for
    /// the content hash the manifest pins, and a model that returned a different card at
    /// runtime would make that pin a lie. In practice it is a field, built once.
    fn card(&self) -> &ModelCard;

    /// The fidelity tiers this model implements, from its card.
    ///
    /// The scenario picks one tier per family; a model offering several (a propagation
    /// model with an abstract and a medium path, say) branches on the tier the engine
    /// selected. Never empty: [`ModelCard::validate`] rejects a card with no tier, and the
    /// registry validates before it registers.
    fn tiers(&self) -> &[Tier] {
        &self.card().tier
    }

    /// True if this model implements `tier`.
    fn implements_tier(&self, tier: Tier) -> bool {
        self.card().implements_tier(tier)
    }

    /// The model's stable id, e.g. `radio/propagation/log-distance-shadowing`.
    fn id(&self) -> &str {
        &self.card().id
    }

    /// The model's own version (semver).
    fn version(&self) -> &str {
        &self.card().version
    }

    /// The plug-in family this model belongs to — the seam it plugs into.
    fn family(&self) -> Family {
        self.card().family
    }

    /// `id@version`, the form the scenario and the manifest name a model by.
    fn id_at_version(&self) -> String {
        self.card().id_at_version()
    }
}

/// How the registry stores a live model: a shared, thread-safe trait object.
///
/// `Arc` because the same model instance is referenced by the registry, by the engine
/// component that calls it and by anything that resolved it from a scenario, and none of
/// them owns it exclusively. `Send + Sync` because the registry is read from inside the
/// phase-parallel maps of 02-architecture.md §6.4, which requires the whole registry to be
/// `Sync`, which requires what it holds to be `Sync`.
///
/// The bound also has a determinism value: a model that captured an `Rc`, a `Cell` or a
/// thread-local — the shapes that make behaviour depend on where the code ran — cannot be
/// stored here at all.
///
/// A family trait object (`Arc<dyn Propagation>`) is *not* this type and cannot be coerced
/// to it, because Rust has no trait-object upcasting across a supertrait boundary for
/// `Arc`. A crate that wants both keeps both handles to the same `Arc`; the registry's copy
/// is the one that answers "what is this model, and what does its card say".
pub type ModelHandle = std::sync::Arc<dyn Model + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::{Family, ModelCard, Tier};

    /// A model as a plug-in author writes one: the card is a field, everything else is
    /// defaulted.
    struct Shadowing {
        card: ModelCard,
    }

    impl Shadowing {
        fn new() -> Self {
            let mut card = ModelCard::new(
                "radio/propagation/log-distance-shadowing",
                Family::Propagation,
                "1.2.0",
                "Log-distance path loss with log-normal shadowing.",
            );
            card.tier = vec![Tier::Abstract, Tier::Medium];
            Self { card }
        }
    }

    impl Model for Shadowing {
        fn card(&self) -> &ModelCard {
            &self.card
        }
    }

    #[test]
    fn the_defaults_all_come_from_the_card() {
        let m = Shadowing::new();
        assert_eq!(m.id(), "radio/propagation/log-distance-shadowing");
        assert_eq!(m.version(), "1.2.0");
        assert_eq!(m.family(), Family::Propagation);
        assert_eq!(
            m.id_at_version(),
            "radio/propagation/log-distance-shadowing@1.2.0"
        );
        assert_eq!(m.tiers(), &[Tier::Abstract, Tier::Medium]);
        assert!(m.implements_tier(Tier::Abstract));
        assert!(!m.implements_tier(Tier::High));
        assert_eq!(m.card().purpose, m.card.purpose);
    }

    /// Object safety is the property the whole plug-in system rests on (ADR 0007 §8): if
    /// this stops compiling, every family trait that extends `Model` stops being usable as
    /// a trait object.
    #[test]
    fn model_is_usable_as_a_trait_object() {
        let boxed: Box<dyn Model> = Box::new(Shadowing::new());
        assert_eq!(boxed.tiers().len(), 2);
        assert!(boxed.implements_tier(Tier::Medium));

        let handle: ModelHandle = std::sync::Arc::new(Shadowing::new());
        assert_eq!(handle.id(), "radio/propagation/log-distance-shadowing");

        // …and the handle really is shareable across threads, which is what the
        // phase-parallel maps need of anything reachable from the registry.
        let cloned = ModelHandle::clone(&handle);
        let ids = std::thread::spawn(move || cloned.id_at_version())
            .join()
            .unwrap();
        assert_eq!(ids, handle.id_at_version());

        fn takes_a_ref(m: &dyn Model) -> Family {
            m.family()
        }
        assert_eq!(takes_a_ref(&*handle), Family::Propagation);
    }

    /// A family trait extends `Model`, and a value behind such a trait object still
    /// answers `card()` — the shape every crate downstream will write.
    #[test]
    fn a_family_trait_can_extend_model() {
        trait Propagation: Model {
            fn loss_db(&self, d_m: f64) -> f64;
        }

        impl Propagation for Shadowing {
            fn loss_db(&self, d_m: f64) -> f64 {
                20.0 * crate::math::log10(d_m)
            }
        }

        let p: Box<dyn Propagation> = Box::new(Shadowing::new());
        assert_eq!(p.loss_db(10.0), 20.0);
        assert_eq!(p.family(), Family::Propagation);
        assert_eq!(p.tiers(), &[Tier::Abstract, Tier::Medium]);
    }
}
