//! Zero-copy input for a hardware H.264 encoder: the captured BGRA desktop texture is scaled and converted to
//! NV12 (BT.709 limited range, as `bgra_to_nv12`) by the GPU's video processor, into a small ring of textures
//! the encoder reads directly. No pixel crosses to system memory.

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::core::{BOOL, Interface};

type Result<T> = windows::core::Result<T>;

/// Encoder input textures in flight: an async encoder may still read one while the next frames are converted.
const RING: usize = 4;

/// `D3D11_VIDEO_PROCESSOR_COLOR_SPACE` bits: YCbCr_Matrix (bit 2) = BT.709, Nominal_Range (bits 4-5) = 16-235.
const OUT_709_LIMITED: u32 = (1 << 2) | (1 << 4);
/// RGB_Range (bit 1) = 0: full-range 0-255 desktop pixels.
const IN_RGB_FULL: u32 = 0;

/// BGRA (`src_w` x `src_h`) to NV12 (`w` x `h`) on the GPU.
pub struct GpuConverter {
    device: ID3D11Device,
    vdev: ID3D11VideoDevice,
    vctx: ID3D11VideoContext,
    ctx: ID3D11DeviceContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    ring: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next: usize,
    /// Input view of the last source texture (the capturer reuses one texture).
    input: Option<(usize, ID3D11VideoProcessorInputView)>,
    w: u32,
    h: u32,
}

impl GpuConverter {
    pub fn new(device: &ID3D11Device, src_w: u32, src_h: u32, w: u32, h: u32, fps: u32) -> Result<Self> {
        unsafe {
            let vdev: ID3D11VideoDevice = device.cast()?;
            let ctx = device.GetImmediateContext()?;
            let vctx: ID3D11VideoContext = ctx.cast()?;
            let rate = DXGI_RATIONAL { Numerator: fps.max(1), Denominator: 1 };
            let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: rate,
                InputWidth: src_w,
                InputHeight: src_h,
                OutputFrameRate: rate,
                OutputWidth: w,
                OutputHeight: h,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let enumerator = vdev.CreateVideoProcessorEnumerator(&content)?;
            let processor = vdev.CreateVideoProcessor(&enumerator, 0)?;
            vctx.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            // No denoise, edge enhancement or other "improvements" a driver may switch on by default.
            vctx.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            vctx.VideoProcessorSetStreamColorSpace(
                &processor,
                0,
                &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: IN_RGB_FULL },
            );
            vctx.VideoProcessorSetOutputColorSpace(
                &processor,
                &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: OUT_709_LIMITED },
            );
            let mut me = Self {
                device: device.clone(),
                vdev,
                vctx,
                ctx,
                enumerator,
                processor,
                ring: Vec::with_capacity(RING),
                next: 0,
                input: None,
                w,
                h,
            };
            for _ in 0..RING {
                let t = me.nv12_texture()?;
                let desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
                };
                let mut view = None;
                me.vdev.CreateVideoProcessorOutputView(&t, &me.enumerator, &desc, Some(&mut view))?;
                me.ring.push((t, view.ok_or_else(super::mf::fail)?));
            }
            Ok(me)
        }
    }

    fn nv12_texture(&self) -> Result<ID3D11Texture2D> {
        let mut desc = D3D11_TEXTURE2D_DESC {
            Width: self.w,
            Height: self.h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_VIDEO_ENCODER.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut t = None;
        unsafe {
            if self.device.CreateTexture2D(&desc, None, Some(&mut t)).is_err() {
                // Drivers without the video-encoder bind flag still take a render target.
                desc.BindFlags = D3D11_BIND_RENDER_TARGET.0 as u32;
                self.device.CreateTexture2D(&desc, None, Some(&mut t))?;
            }
        }
        t.ok_or_else(super::mf::fail)
    }

    /// Converts `src` into the next ring texture and returns it.
    pub fn convert(&mut self, src: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        unsafe {
            let key = src.as_raw() as usize;
            if self.input.as_ref().is_none_or(|(k, _)| *k != key) {
                let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    FourCC: 0,
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
                    },
                };
                let mut view = None;
                self.vdev.CreateVideoProcessorInputView(src, &self.enumerator, &desc, Some(&mut view))?;
                self.input = Some((key, view.ok_or_else(super::mf::fail)?));
            }
            let input = self.input.as_ref().map(|(_, v)| v.clone());
            let (tex, out) = &self.ring[self.next];
            self.next = (self.next + 1) % RING;
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: BOOL(1),
                pInputSurface: std::mem::ManuallyDrop::new(input),
                ..Default::default()
            };
            let streams = [stream];
            let r = self.vctx.VideoProcessorBlt(&self.processor, out, 0, &streams);
            let [mut s] = streams;
            std::mem::ManuallyDrop::drop(&mut s.pInputSurface);
            r?;
            // Submit now: the encoder reads the texture from its own engine, and commands still waiting in this
            // context's buffer would leave it reading an unwritten (black) picture.
            self.ctx.Flush();
            Ok(tex.clone())
        }
    }
}
