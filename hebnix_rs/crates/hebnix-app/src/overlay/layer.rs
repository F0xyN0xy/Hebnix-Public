#[cfg(target_os = "windows")]
use std::mem::ManuallyDrop;
use windows::Win32::Graphics::Direct2D::{
    Common::D2D_RECT_F, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_LAYER_OPTIONS1_NONE,
    D2D1_LAYER_PARAMETERS1, ID2D1Geometry,
};

/// Windows structs store interface fields in ManuallyDrop. Own and release the
/// mask explicitly; dropping D2D1_LAYER_PARAMETERS1 alone leaks its COM reference.
pub(crate) struct ImageLayerParameters(pub D2D1_LAYER_PARAMETERS1);

impl ImageLayerParameters {
    pub(crate) fn new(bounds: D2D_RECT_F, mask: ID2D1Geometry) -> Self {
        Self(D2D1_LAYER_PARAMETERS1 {
            contentBounds: bounds,
            geometricMask: ManuallyDrop::new(Some(mask)),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: windows_numerics::Matrix3x2 {
                M11: 1.0,
                M22: 1.0,
                ..Default::default()
            },
            opacity: 1.0,
            opacityBrush: ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS1_NONE,
        })
    }
}

impl Drop for ImageLayerParameters {
    fn drop(&mut self) {
        unsafe {
            ManuallyDrop::drop(&mut self.0.geometricMask);
            ManuallyDrop::drop(&mut self.0.opacityBrush);
        }
    }
}
