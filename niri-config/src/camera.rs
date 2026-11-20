//! arbor: the dock tree camera.

use crate::utils::MergeWith;
use crate::FloatOrInt;

/// Camera over the dock tree: the tree is laid out on a canvas that grows past the screen when
/// leaves would get smaller than the minimum size, and the camera shows part of it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// Smallest useful leaf width; the canvas grows to keep leaves at least this wide.
    pub min_leaf_width: f64,
    /// Smallest useful leaf height.
    pub min_leaf_height: f64,
    /// Leaves narrower than this on screen draw as icon tiles.
    pub icon_threshold: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            min_leaf_width: 480.,
            min_leaf_height: 320.,
            icon_threshold: 160.,
        }
    }
}

impl MergeWith<CameraPart> for Camera {
    fn merge_with(&mut self, part: &CameraPart) {
        if let Some(x) = part.min_leaf_width {
            self.min_leaf_width = x.0;
        }
        if let Some(x) = part.min_leaf_height {
            self.min_leaf_height = x.0;
        }
        if let Some(x) = part.icon_threshold {
            self.icon_threshold = x.0;
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct CameraPart {
    #[knuffel(child, unwrap(argument))]
    pub min_leaf_width: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub min_leaf_height: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub icon_threshold: Option<FloatOrInt<0, 65535>>,
}
