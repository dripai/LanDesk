use anyhow::{Context, Result, ensure};
use windows::{
    Win32::{
        Foundation::{HMODULE, POINT},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::*,
            Dxgi::{Common::*, *},
            Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint},
        },
    },
    core::Interface,
};

#[derive(Debug)]
pub struct NoOutput;
impl std::fmt::Display for NoOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Windows 当前会话没有已连接的显示输出")
    }
}
impl std::error::Error for NoOutput {}

pub struct Capture {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    texture_width: u32,
    texture_height: u32,
    rotation: DXGI_MODE_ROTATION,
    pub width: u32,
    pub height: u32,
}
impl Capture {
    pub fn new() -> Result<Self> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let primary = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut selected = None;
            let mut attached = 0;
            let mut adapters = 0;
            loop {
                let adapter = match factory.EnumAdapters1(adapters) {
                    Ok(value) => value,
                    Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(error) => return Err(error.into()),
                };
                adapters += 1;
                let mut outputs = 0;
                loop {
                    let output = match adapter.EnumOutputs(outputs) {
                        Ok(value) => value,
                        Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                        Err(error) => return Err(error.into()),
                    };
                    outputs += 1;
                    let desc = output.GetDesc()?;
                    if !desc.AttachedToDesktop.as_bool() {
                        continue;
                    }
                    attached += 1;
                    if desc.Monitor != primary {
                        continue;
                    }
                    selected = Some((adapter.clone(), output, desc));
                }
            }
            if attached == 0 {
                return Err(NoOutput.into());
            }
            let (adapter, output, output_desc) = selected
                .ok_or_else(|| {
                    windows::core::Error::from_hresult(DXGI_ERROR_NOT_CURRENTLY_AVAILABLE)
                })
                .context("Windows 尚未提供系统主屏的 DXGI 输出")?;
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device = device.context("创建 D3D11 设备失败")?;
            let context = context.context("创建 D3D11 上下文失败")?;
            let duplication = output.cast::<IDXGIOutput1>()?.DuplicateOutput(&device)?;
            let desc = duplication.GetDesc();
            let (width, height) = (desc.ModeDesc.Width, desc.ModeDesc.Height);
            ensure!(
                width > 0 && height > 0 && u64::from(width) * u64::from(height) <= 16_000_000,
                "显示器尺寸超过限制"
            );
            ensure!(
                desc.ModeDesc.Format == DXGI_FORMAT_B8G8R8A8_UNORM,
                "桌面像素格式不受支持"
            );
            let mut staging = None;
            device.CreateTexture2D(
                &D3D11_TEXTURE2D_DESC {
                    Width: width,
                    Height: height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_STAGING,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut staging),
            )?;
            Ok(Self {
                device,
                context,
                duplication,
                staging: staging.context("创建桌面像素缓冲失败")?,
                texture_width: width,
                texture_height: height,
                rotation: output_desc.Rotation,
                width: (output_desc.DesktopCoordinates.right - output_desc.DesktopCoordinates.left)
                    as u32,
                height: (output_desc.DesktopCoordinates.bottom - output_desc.DesktopCoordinates.top)
                    as u32,
            })
        }
    }
    pub fn frame(&self) -> Result<Option<image::RgbImage>> {
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource = None;
            match self
                .duplication
                .AcquireNextFrame(50, &mut info, &mut resource)
            {
                Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
                Err(error) => return Err(error.into()),
                Ok(()) => {}
            }
            let result = (|| -> Result<_> {
                let source: ID3D11Texture2D = resource.context("桌面帧资源为空")?.cast()?;
                self.context.CopyResource(&self.staging, &source);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                self.context
                    .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                let result = (|| -> Result<_> {
                    ensure!(
                        !mapped.pData.is_null() && mapped.RowPitch >= self.texture_width * 4,
                        "桌面像素步长无效"
                    );
                    let data = std::slice::from_raw_parts(
                        mapped.pData.cast::<u8>(),
                        mapped.RowPitch as usize * self.texture_height as usize,
                    );
                    let mut rgb = image::RgbImage::new(self.texture_width, self.texture_height);
                    for (source, target) in data.chunks_exact(mapped.RowPitch as usize).zip(
                        rgb.as_mut()
                            .chunks_exact_mut(self.texture_width as usize * 3),
                    ) {
                        for (pixel, out) in source[..self.texture_width as usize * 4]
                            .chunks_exact(4)
                            .zip(target.chunks_exact_mut(3))
                        {
                            out.copy_from_slice(&[pixel[2], pixel[1], pixel[0]]);
                        }
                    }
                    let rgb = match self.rotation {
                        DXGI_MODE_ROTATION_ROTATE90 => image::imageops::rotate90(&rgb),
                        DXGI_MODE_ROTATION_ROTATE180 => image::imageops::rotate180(&rgb),
                        DXGI_MODE_ROTATION_ROTATE270 => image::imageops::rotate270(&rgb),
                        _ => rgb,
                    };
                    Ok(Some(rgb))
                })();
                self.context.Unmap(&self.staging, 0);
                result
            })();
            let released = self.duplication.ReleaseFrame();
            let frame = result?;
            released?;
            Ok(frame)
        }
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            self.context.ClearState();
            self.context.Flush();
        }
        let _ = &self.device;
    }
}
