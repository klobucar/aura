// Include the generated protobuf code
// Package: aura.v1alpha1 -> aura.v1alpha1.rs
pub mod aura {
    pub mod v1alpha1 {
        include!(concat!(env!("OUT_DIR"), "/aura.v1alpha1.rs"));
    }
}

// Re-export v1alpha1 types to top-level for convenience/compat
pub use aura::v1alpha1::*;

pub mod fast_header;
pub use fast_header::*;

impl Position {
    pub fn distance(&self, other: &Position) -> f32 {
        ((self.x - other.x).powi(2) + 
         (self.y - other.y).powi(2) + 
         (self.z - other.z).powi(2)).sqrt()
    }
}
