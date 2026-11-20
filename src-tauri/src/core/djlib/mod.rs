//! DJ sync hub: a normalized DJ-library model plus one adapter per DJ program.
//! Spec: docs/superpowers/specs/2026-09-25-dj-sync-hub-design.md
pub mod capabilities;
pub mod convert;
pub mod endpoint;
pub mod model;
pub mod rekordbox;
pub mod serato;
pub mod serato_import;
