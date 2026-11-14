//! Zero-copy input for a hardware H.264 encoder: the captured BGRA desktop texture is converted to NV12 (BT.709
//! limited range, as `bgra_to_nv12`) by the GPU's video processor, into a small ring of textures the encoder
//! reads directly. No pixel crosses to system memory.
//!
//! A smaller encode size is scaled first by [`AreaScaler`], a compute shader with the CPU scaler's area average:
//! the video processor's own filter is several dB softer on text.

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_RATIONAL,
    DXGI_SAMPLE_DESC,
};
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
    /// Downscale ahead of the video processor when the encode size is smaller than the desktop.
    scaler: Option<AreaScaler>,
    w: u32,
    h: u32,
}

impl GpuConverter {
    pub fn new(device: &ID3D11Device, src_w: u32, src_h: u32, w: u32, h: u32, fps: u32) -> Result<Self> {
        let scaler = if (src_w, src_h) != (w, h) { Some(AreaScaler::new(device, src_w, src_h, w, h)?) } else { None };
        // The video processor then only converts colour, at the encode size.
        let (src_w, src_h) = if scaler.is_some() { (w, h) } else { (src_w, src_h) };
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
                scaler,
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
        let scaled;
        let src = match &mut self.scaler {
            Some(scaler) => {
                scaled = scaler.run(src)?;
                &scaled
            }
            None => src,
        };
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

/// Compiled `shaders/area_scale.hlsl` (cs_5_0).
const AREA_SCALE_CS: &[u8] = include_bytes!("shaders/area_scale.cso");

/// Area-average downscale on the GPU (`shaders/area_scale.hlsl`): the desktop texture into a texture of the
/// encode size, matching the CPU scaler of DXGI capture.
pub struct AreaScaler {
    ctx: ID3D11DeviceContext,
    device: ID3D11Device,
    shader: ID3D11ComputeShader,
    params: ID3D11Buffer,
    out: ID3D11Texture2D,
    uav: ID3D11UnorderedAccessView,
    /// Shader view of the last source texture (the capturer reuses one texture).
    input: Option<(usize, ID3D11ShaderResourceView)>,
    w: u32,
    h: u32,
}

impl AreaScaler {
    pub fn new(device: &ID3D11Device, src_w: u32, src_h: u32, w: u32, h: u32) -> Result<Self> {
        if w == 0 || h == 0 || w > src_w || h > src_h {
            return Err(super::mf::fail());
        }
        unsafe {
            let ctx = device.GetImmediateContext()?;
            let mut shader = None;
            device.CreateComputeShader(AREA_SCALE_CS, None, Some(&mut shader))?;
            let shader = shader.ok_or_else(super::mf::fail)?;
            // Scale (2 x f32), destination and source size (2 x u32 each), padding: 32 bytes.
            let mut cb = [0u8; 32];
            cb[0..4].copy_from_slice(&(src_w as f32 / w as f32).to_le_bytes());
            cb[4..8].copy_from_slice(&(src_h as f32 / h as f32).to_le_bytes());
            for (i, v) in [w, h, src_w, src_h].into_iter().enumerate() {
                cb[8 + i * 4..12 + i * 4].copy_from_slice(&v.to_le_bytes());
            }
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: cb.len() as u32,
                Usage: D3D11_USAGE_IMMUTABLE,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let init = D3D11_SUBRESOURCE_DATA { pSysMem: cb.as_ptr().cast(), ..Default::default() };
            let mut params = None;
            device.CreateBuffer(&desc, Some(&init), Some(&mut params))?;
            let out = Self::output(device, w, h)?;
            let mut uav = None;
            device.CreateUnorderedAccessView(&out, None, Some(&mut uav))?;
            Ok(Self {
                ctx,
                device: device.clone(),
                shader,
                params: params.ok_or_else(super::mf::fail)?,
                out,
                uav: uav.ok_or_else(super::mf::fail)?,
                input: None,
                w,
                h,
            })
        }
    }

    /// BGRA when the GPU can store to it from a shader (no channel swap for the video processor), else RGBA.
    fn output(device: &ID3D11Device, w: u32, h: u32) -> Result<ID3D11Texture2D> {
        let format = if Self::typed_uav(device, DXGI_FORMAT_B8G8R8A8_UNORM) {
            DXGI_FORMAT_B8G8R8A8_UNORM
        } else {
            DXGI_FORMAT_R8G8B8A8_UNORM
        };
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_UNORDERED_ACCESS.0 | D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0)
                as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut t = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut t))? };
        t.ok_or_else(super::mf::fail)
    }

    fn typed_uav(device: &ID3D11Device, format: DXGI_FORMAT) -> bool {
        let mut support = D3D11_FEATURE_DATA_FORMAT_SUPPORT2 { InFormat: format, OutFormatSupport2: 0 };
        let ok = unsafe {
            device.CheckFeatureSupport(
                D3D11_FEATURE_FORMAT_SUPPORT2,
                (&mut support as *mut D3D11_FEATURE_DATA_FORMAT_SUPPORT2).cast(),
                std::mem::size_of::<D3D11_FEATURE_DATA_FORMAT_SUPPORT2>() as u32,
            )
        };
        ok.is_ok() && support.OutFormatSupport2 & D3D11_FORMAT_SUPPORT2_UAV_TYPED_STORE.0 as u32 != 0
    }

    /// Scales `src` (the desktop, BGRA with a shader-resource binding) into the output texture and returns it.
    pub fn run(&mut self, src: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        unsafe {
            let key = src.as_raw() as usize;
            if self.input.as_ref().is_none_or(|(k, _)| *k != key) {
                let mut view = None;
                self.device.CreateShaderResourceView(src, None, Some(&mut view))?;
                self.input = Some((key, view.ok_or_else(super::mf::fail)?));
            }
            let srv = self.input.as_ref().map(|(_, v)| v.clone());
            self.ctx.CSSetShader(&self.shader, None);
            self.ctx.CSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
            self.ctx.CSSetShaderResources(0, Some(&[srv]));
            self.ctx.CSSetUnorderedAccessViews(0, 1, Some(&Some(self.uav.clone())), None);
            self.ctx.Dispatch(self.w.div_ceil(8), self.h.div_ceil(8), 1);
            // Unbound, so the video processor may read the texture and the capturer may write the source.
            self.ctx.CSSetUnorderedAccessViews(0, 1, Some(&None), None);
            self.ctx.CSSetShaderResources(0, Some(&[None]));
            self.ctx.CSSetShader(None, None);
        }
        Ok(self.out.clone())
    }
}

#[cfg(test)]
mod tests {
    /// Regenerates `shaders/area_scale.cso` from the HLSL (needs d3dcompiler_47.dll, part of Windows 10+):
    /// `cargo test --lib compile_area_scale_shader -- --ignored`
    #[test]
    #[ignore]
    fn compile_area_scale_shader() {
        use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
        use windows::Win32::Graphics::Direct3D::ID3DBlob;
        use windows::core::s;
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/codec/h264/shaders");
        let src = std::fs::read(dir.join("area_scale.hlsl")).unwrap();
        let (mut code, mut errors): (Option<ID3DBlob>, Option<ID3DBlob>) = (None, None);
        let r = unsafe {
            D3DCompile(
                src.as_ptr().cast(),
                src.len(),
                s!("area_scale.hlsl"),
                None,
                None,
                s!("main"),
                s!("cs_5_0"),
                D3DCOMPILE_OPTIMIZATION_LEVEL3,
                0,
                &mut code,
                Some(&mut errors),
            )
        };
        if let Some(e) = errors {
            let msg = unsafe { std::slice::from_raw_parts(e.GetBufferPointer() as *const u8, e.GetBufferSize()) };
            println!("{}", String::from_utf8_lossy(msg));
        }
        r.unwrap();
        let code = code.unwrap();
        let bytes = unsafe { std::slice::from_raw_parts(code.GetBufferPointer() as *const u8, code.GetBufferSize()) };
        std::fs::write(dir.join("area_scale.cso"), bytes).unwrap();
        println!("area_scale.cso: {} bytes", bytes.len());
    }
}
