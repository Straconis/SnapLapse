//! Windows capture.
//!
//! The main method is Windows Graphics Capture (WGC), which reads a window's image straight from
//! the desktop compositor. It works while the window is covered, and the copy happens on the
//! graphics card. Windows delivers a new image only when the window changes. We keep a reference
//! to the newest one and copy it to memory only when a screenshot is due, so there's almost no
//! work between shots.
//!
//! PrintWindow is the fallback for systems where WGC isn't available.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{factory, IInspectable, Interface, HSTRING};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{BOOL, E_FAIL, HMODULE, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, MessageBoxW,
    GA_ROOT, GWL_EXSTYLE, MB_ICONERROR, WS_EX_TOOLWINDOW,
};

use super::{CaptureError, Capturer, Frame, Method, WindowId, WindowInfo};

const PIXEL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
/// Asks PrintWindow to include content drawn by the GPU, which browsers need.
const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(2);

fn hwnd(id: WindowId) -> HWND {
    HWND(id.0 as usize as *mut c_void)
}

fn window_title(hwnd: HWND) -> Option<String> {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(hwnd, &mut buf);
        (copied > 0).then(|| String::from_utf16_lossy(&buf[..copied as usize]))
    }
}

/// Matches the windows shown in the taskbar: visible, top-level, not tool windows, not hidden
/// on another virtual desktop, and not SnapLapse itself.
unsafe fn is_listed(hwnd: HWND) -> bool {
    if !IsWindowVisible(hwnd).as_bool() || GetAncestor(hwnd, GA_ROOT) != hwnd {
        return false;
    }
    if GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
        return false;
    }
    let mut cloaked = 0u32;
    let attr = DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut c_void,
        std::mem::size_of::<u32>() as u32,
    );
    if attr.is_ok() && cloaked != 0 {
        return false;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    pid != std::process::id()
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let windows = &mut *(lparam.0 as *mut Vec<WindowInfo>);
    if is_listed(hwnd) {
        if let Some(title) = window_title(hwnd) {
            let title = title.trim();
            if !title.is_empty() {
                windows.push(WindowInfo {
                    id: WindowId(hwnd.0 as usize as u64),
                    title: title.to_string(),
                });
            }
        }
    }
    TRUE
}

pub fn list_windows() -> Vec<WindowInfo> {
    let mut windows: Vec<WindowInfo> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(collect_window),
            LPARAM(&mut windows as *mut _ as isize),
        );
    }
    windows
}

fn check_window(hwnd: HWND) -> Result<(), CaptureError> {
    unsafe {
        if !IsWindow(hwnd).as_bool() {
            return Err(CaptureError::Gone);
        }
        if IsIconic(hwnd).as_bool() {
            return Err(CaptureError::Minimized);
        }
    }
    Ok(())
}

pub fn open(id: WindowId, method: Method) -> Result<Box<dyn Capturer>, CaptureError> {
    let hwnd = hwnd(id);
    unsafe {
        if !IsWindow(hwnd).as_bool() {
            return Err(CaptureError::Gone);
        }
    }
    match method {
        Method::PrintWindow => Ok(Box::new(PrintWindowCapturer { hwnd })),
        Method::GraphicsCapture => Wgc::open(hwnd)
            .map(|c| Box::new(c) as Box<dyn Capturer>)
            .map_err(|e| CaptureError::Failed(format!("Windows Graphics Capture failed: {e}"))),
        Method::Auto => match Wgc::open(hwnd) {
            Ok(c) => Ok(Box::new(c)),
            Err(_) => Ok(Box::new(PrintWindowCapturer { hwnd })),
        },
    }
}

/// Shows an error dialog, for failures before the app window exists.
pub fn show_error(message: &str) {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(message),
            &HSTRING::from("SnapLapse"),
            MB_ICONERROR,
        );
    }
}

/// A capture frame that can be handed between threads. Windows capture frames are free-threaded.
struct SharedFrame(Direct3D11CaptureFrame);
unsafe impl Send for SharedFrame {}

struct Wgc {
    hwnd: HWND,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    d3d: IDirect3DDevice,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    _item: GraphicsCaptureItem,
    latest: Arc<Mutex<Option<SharedFrame>>>,
    pool_size: SizeInt32,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
}

fn create_device() -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
    let drivers: [D3D_DRIVER_TYPE; 2] = [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP];
    for driver in drivers {
        let mut device = None;
        let mut context = None;
        let created = unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        };
        if created.is_ok() {
            if let (Some(device), Some(context)) = (device, context) {
                return Ok((device, context));
            }
        }
    }
    Err(E_FAIL.into())
}

impl Wgc {
    fn open(hwnd: HWND) -> windows::core::Result<Self> {
        unsafe {
            // Fails harmlessly if this thread is already set up.
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let (device, context) = create_device()?;
        let dxgi: IDXGIDevice = device.cast()?;
        let d3d: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;

        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd)? };
        let size = item.Size()?;

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&d3d, PIXEL_FORMAT, 2, size)?;
        let session = pool.CreateCaptureSession(&item)?;
        // These settings only exist on newer Windows versions. On Windows 10, the yellow border
        // can't be turned off.
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(false);

        let latest: Arc<Mutex<Option<SharedFrame>>> = Arc::new(Mutex::new(None));
        let sink = latest.clone();
        pool.FrameArrived(
            &TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |pool, _| {
                if let Some(pool) = pool {
                    if let Ok(frame) = pool.TryGetNextFrame() {
                        // Replacing the old frame hands its buffer back to Windows.
                        *sink.lock().unwrap() = Some(SharedFrame(frame));
                    }
                }
                Ok(())
            }),
        )?;
        session.StartCapture()?;

        Ok(Wgc {
            hwnd,
            device,
            context,
            d3d,
            pool,
            session,
            _item: item,
            latest,
            pool_size: size,
            staging: None,
        })
    }

    /// Copies a frame from the graphics card into memory.
    fn copy_frame(&mut self, frame: &Direct3D11CaptureFrame) -> windows::core::Result<Frame> {
        let content = frame.ContentSize()?;
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };

        let reusable =
            matches!(&self.staging, Some((_, w, h)) if *w == desc.Width && *h == desc.Height);
        if !reusable {
            let mut staging_desc = desc;
            staging_desc.Usage = D3D11_USAGE_STAGING;
            staging_desc.BindFlags = 0;
            staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            staging_desc.MiscFlags = 0;
            let mut staging = None;
            unsafe {
                self.device
                    .CreateTexture2D(&staging_desc, None, Some(&mut staging))?
            };
            let staging = staging.ok_or_else(|| windows::core::Error::from(E_FAIL))?;
            self.staging = Some((staging, desc.Width, desc.Height));
        }
        let staging = &self
            .staging
            .as_ref()
            .expect("staging texture was just created")
            .0;
        unsafe { self.context.CopyResource(staging, &texture) };

        let width = (content.Width.max(1) as u32).min(desc.Width);
        let height = (content.Height.max(1) as u32).min(desc.Height);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?
        };
        let row_bytes = width as usize * 4;
        let mut bgra = Vec::with_capacity(row_bytes * height as usize);
        for y in 0..height as usize {
            let row = unsafe {
                std::slice::from_raw_parts(
                    (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                    row_bytes,
                )
            };
            bgra.extend_from_slice(row);
        }
        unsafe { self.context.Unmap(staging, 0) };

        // The window was resized, so resize the buffers for the next frames.
        if content.Width != self.pool_size.Width || content.Height != self.pool_size.Height {
            self.pool.Recreate(&self.d3d, PIXEL_FORMAT, 2, content)?;
            self.pool_size = content;
        }
        Ok(Frame {
            width,
            height,
            bgra,
        })
    }
}

impl Capturer for Wgc {
    fn grab(&mut self) -> Result<Frame, CaptureError> {
        check_window(self.hwnd)?;
        // Windows sends the first image right after capture starts, so this wait is brief.
        let deadline = Instant::now() + Duration::from_secs(2);
        let frame = loop {
            if let Some(shared) = self.latest.lock().unwrap().as_ref() {
                break shared.0.clone();
            }
            if Instant::now() > deadline {
                return Err(CaptureError::Failed(
                    "The window didn't send an image".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        self.copy_frame(&frame)
            .map_err(|e| CaptureError::Failed(format!("Copying the image failed: {e}")))
    }

    fn title(&self) -> Option<String> {
        window_title(self.hwnd)
    }
}

impl Drop for Wgc {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

struct PrintWindowCapturer {
    hwnd: HWND,
}

impl Capturer for PrintWindowCapturer {
    fn grab(&mut self) -> Result<Frame, CaptureError> {
        check_window(self.hwnd)?;
        unsafe { print_window(self.hwnd) }
            .ok_or_else(|| CaptureError::Failed("PrintWindow couldn't capture this window".into()))
    }

    fn title(&self) -> Option<String> {
        window_title(self.hwnd)
    }
}

unsafe fn print_window(hwnd: HWND) -> Option<Frame> {
    let mut outer = RECT::default();
    GetWindowRect(hwnd, &mut outer).ok()?;
    // The visible frame, without the invisible resize borders Windows 10 and 11 add.
    let mut visible = outer;
    let _ = DwmGetWindowAttribute(
        hwnd,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        &mut visible as *mut RECT as *mut c_void,
        std::mem::size_of::<RECT>() as u32,
    );
    let (w, h) = (outer.right - outer.left, outer.bottom - outer.top);
    if w <= 0 || h <= 0 {
        return None;
    }

    let screen = GetDC(None);
    let memory = CreateCompatibleDC(screen);
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // negative means rows run top to bottom
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    let result = match CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
        Ok(bitmap) => {
            let previous = SelectObject(memory, bitmap);
            let printed = PrintWindow(hwnd, memory, PW_RENDERFULLCONTENT).as_bool();
            let frame = (printed && !bits.is_null()).then(|| {
                let all = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
                crop(all, w, h, &outer, &visible)
            });
            SelectObject(memory, previous);
            let _ = DeleteObject(bitmap);
            frame
        }
        Err(_) => None,
    };
    let _ = DeleteDC(memory);
    ReleaseDC(None, screen);
    result
}

/// Cuts the visible part of the window out of the full image and makes every pixel opaque.
fn crop(all: &[u8], w: i32, h: i32, outer: &RECT, visible: &RECT) -> Frame {
    let left = (visible.left - outer.left).clamp(0, w);
    let top = (visible.top - outer.top).clamp(0, h);
    let right = (visible.right - outer.left).clamp(left, w);
    let bottom = (visible.bottom - outer.top).clamp(top, h);
    let (cw, ch) = if right > left && bottom > top {
        (right - left, bottom - top)
    } else {
        (w, h)
    };
    let (left, top) = if cw == w && ch == h {
        (0, 0)
    } else {
        (left, top)
    };
    let mut bgra = Vec::with_capacity((cw * ch * 4) as usize);
    for y in top..top + ch {
        let start = ((y * w + left) * 4) as usize;
        bgra.extend_from_slice(&all[start..start + (cw * 4) as usize]);
    }
    for alpha in bgra.iter_mut().skip(3).step_by(4) {
        *alpha = 255;
    }
    Frame {
        width: cw as u32,
        height: ch as u32,
        bgra,
    }
}
