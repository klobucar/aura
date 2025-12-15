// Include the generated protobuf code
include!(concat!(env!("OUT_DIR"), "/aura.rs"));

pub mod fast_header;
pub use fast_header::*;

impl Position {
    pub fn distance(&self, other: &Position) -> f32 {
        ((self.x - other.x).powi(2) + 
         (self.y - other.y).powi(2) + 
         (self.z - other.z).powi(2)).sqrt()
    }
}
