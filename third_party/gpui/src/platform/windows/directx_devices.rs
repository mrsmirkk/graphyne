use anyhow::{Context, Result};
use util::ResultExt;
use windows::Win32::{
    Foundation::HMODULE,
    Graphics::{
        Direct3D::{
            D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_1,
            D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
        },
        Direct3D11::{
            D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_DEBUG,
            D3D11_FEATURE_D3D10_X_HARDWARE_OPTIONS, D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS,
            D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
        },
        Dxgi::{
            CreateDXGIFactory2, DXGI_CREATE_FACTORY_DEBUG, DXGI_CREATE_FACTORY_FLAGS,
            DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE, DXGI_GPU_PREFERENCE_MINIMUM_POWER, IDXGIAdapter1,
            IDXGIFactory6,
        },
    },
};
use windows::core::Interface;

pub(crate) fn try_to_recover_from_device_lost<T>(
    mut f: impl FnMut() -> Result<T>,
    on_success: impl FnOnce(T),
    on_error: impl FnOnce(),
) {
    let result = (0..5).find_map(|i| {
        if i > 0 {
            // Add a small delay before retrying
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        f().log_err()
    });

    if let Some(result) = result {
        on_success(result);
    } else {
        on_error();
    }
}

#[derive(Clone)]
pub(crate) struct DirectXDevices {
    pub(crate) adapter: IDXGIAdapter1,
    pub(crate) dxgi_factory: IDXGIFactory6,
    pub(crate) device: ID3D11Device,
    pub(crate) device_context: ID3D11DeviceContext,
}

impl DirectXDevices {
    pub(crate) fn new() -> Result<Self> {
        let __t0 = std::time::Instant::now();
        let __trace = std::env::var_os("GRAPHYNE_STARTUP_TRACE").is_some();
        let __mark = |label: &str| {
            if __trace {
                eprintln!("  gpui: {label:<30} {:>7.1} ms", __t0.elapsed().as_secs_f64() * 1000.0);
            }
        };
        let debug_layer_available = check_debug_layer_available();
        let dxgi_factory =
            get_dxgi_factory(debug_layer_available).context("Creating DXGI factory")?;
        __mark("  dxgi factory");
        // Graphyne: the device created while probing the adapter is the one we keep
        // (upstream created it twice; each creation is slow on hybrid-GPU laptops).
        let (adapter, device, context, feature_level) =
            get_adapter(&dxgi_factory, debug_layer_available).context("Getting DXGI adapter")?;
        __mark("  adapter + d3d11 device");
        let (device, device_context) = {
            match feature_level {
                D3D_FEATURE_LEVEL_11_1 => {
                    log::info!("Created device with Direct3D 11.1 feature level.")
                }
                D3D_FEATURE_LEVEL_11_0 => {
                    log::info!("Created device with Direct3D 11.0 feature level.")
                }
                D3D_FEATURE_LEVEL_10_1 => {
                    log::info!("Created device with Direct3D 10.1 feature level.")
                }
                _ => unreachable!(),
            }
            (device, context)
        };

        Ok(Self {
            adapter,
            dxgi_factory,
            device,
            device_context,
        })
    }
}

#[inline]
fn check_debug_layer_available() -> bool {
    #[cfg(debug_assertions)]
    {
        use windows::Win32::Graphics::Dxgi::{DXGIGetDebugInterface1, IDXGIInfoQueue};

        unsafe { DXGIGetDebugInterface1::<IDXGIInfoQueue>(0) }
            .log_err()
            .is_some()
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

#[inline]
fn get_dxgi_factory(debug_layer_available: bool) -> Result<IDXGIFactory6> {
    let factory_flag = if debug_layer_available {
        DXGI_CREATE_FACTORY_DEBUG
    } else {
        #[cfg(debug_assertions)]
        log::warn!(
            "Failed to get DXGI debug interface. DirectX debugging features will be disabled."
        );
        DXGI_CREATE_FACTORY_FLAGS::default()
    };
    unsafe { Ok(CreateDXGIFactory2(factory_flag)?) }
}

/// Which GPU to render with. Graphyne: a 2D editor doesn't need the discrete GPU of a
/// hybrid laptop, and creating a device there is the slowest step of start-up (~250 ms to
/// wake the NVIDIA driver against ~110 ms on the integrated GPU), besides keeping that GPU
/// awake. So the minimum-power GPU goes first when a display is connected to it (a laptop's
/// own screen); on desktops, whose monitors hang on the discrete card, the order stays
/// Windows' own. The high-performance GPU is used when asked for in Windows Settings >
/// Display > Graphics, or with `GRAPHYNE_GPU=high` (`low` forces the integrated one,
/// `default` keeps Windows' order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GpuChoice {
    Default,
    MinimumPower,
    ForcedMinimumPower,
    HighPerformance,
}

fn gpu_choice() -> GpuChoice {
    match std::env::var("GRAPHYNE_GPU").unwrap_or_default().to_ascii_lowercase().as_str() {
        "high" => return GpuChoice::HighPerformance,
        "low" => return GpuChoice::ForcedMinimumPower,
        "default" => return GpuChoice::Default,
        _ => {}
    }
    // Windows Settings > Graphics keeps "GpuPreference=2;" (high performance), "=1;" (power
    // saving) or "=0;" (let Windows decide) under the program's full path.
    let preference = std::env::current_exe().ok().and_then(|exe| {
        let key = windows_registry::CURRENT_USER.open(r"Software\Microsoft\DirectX\UserGpuPreferences").ok()?;
        key.get_string(exe.to_string_lossy().as_ref()).ok()
    });
    match preference.as_deref().and_then(|p| p.split(';').find_map(|kv| kv.trim().strip_prefix("GpuPreference="))) {
        Some("2") => GpuChoice::HighPerformance,
        _ => GpuChoice::MinimumPower,
    }
}

fn adapter_name(adapter: &IDXGIAdapter1) -> String {
    unsafe { adapter.GetDesc1() }
        .map(|desc| String::from_utf16_lossy(&desc.Description).trim_matches(char::from(0)).to_string())
        .unwrap_or_default()
}

fn has_outputs(adapter: &IDXGIAdapter1) -> bool {
    unsafe { adapter.EnumOutputs(0) }.is_ok()
}

/// The adapters to try, best first.
fn candidate_adapters(dxgi_factory: &IDXGIFactory6) -> Vec<IDXGIAdapter1> {
    let first_by = |preference| unsafe { dxgi_factory.EnumAdapterByGpuPreference::<IDXGIAdapter1>(0, preference) }.ok();
    let mut adapters: Vec<IDXGIAdapter1> = match gpu_choice() {
        GpuChoice::Default => None,
        GpuChoice::HighPerformance => first_by(DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE),
        GpuChoice::ForcedMinimumPower => first_by(DXGI_GPU_PREFERENCE_MINIMUM_POWER),
        GpuChoice::MinimumPower => first_by(DXGI_GPU_PREFERENCE_MINIMUM_POWER).filter(has_outputs),
    }
    .into_iter()
    .collect();
    adapters.extend((0..).map_while(|i| unsafe { dxgi_factory.EnumAdapters(i) }.ok().and_then(|a| a.cast::<IDXGIAdapter1>().ok())));
    adapters
}

/// The first candidate adapter that supports Direct3D 11, with the device created on it.
#[inline]
fn get_adapter(
    dxgi_factory: &IDXGIFactory6,
    debug_layer_available: bool,
) -> Result<(IDXGIAdapter1, ID3D11Device, ID3D11DeviceContext, D3D_FEATURE_LEVEL)> {
    for adapter in candidate_adapters(dxgi_factory) {
        let mut context: Option<ID3D11DeviceContext> = None;
        let mut feature_level = D3D_FEATURE_LEVEL::default();
        if let Some(device) = get_device(
            &adapter,
            Some(&mut context),
            Some(&mut feature_level),
            debug_layer_available,
        )
        .log_err()
        {
            if let Some(context) = context {
                log::info!("Using GPU: {}", adapter_name(&adapter));
                return Ok((adapter, device, context, feature_level));
            }
        }
    }
    anyhow::bail!("no GPU supports Direct3D 11")
}

#[inline]
fn get_device(
    adapter: &IDXGIAdapter1,
    context: Option<*mut Option<ID3D11DeviceContext>>,
    feature_level: Option<*mut D3D_FEATURE_LEVEL>,
    debug_layer_available: bool,
) -> Result<ID3D11Device> {
    let mut device: Option<ID3D11Device> = None;
    let device_flags = if debug_layer_available {
        D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_DEBUG
    } else {
        D3D11_CREATE_DEVICE_BGRA_SUPPORT
    };
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            device_flags,
            // 4x MSAA is required for Direct3D Feature Level 10.1 or better
            Some(&[
                D3D_FEATURE_LEVEL_11_1,
                D3D_FEATURE_LEVEL_11_0,
                D3D_FEATURE_LEVEL_10_1,
            ]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            feature_level,
            context,
        )?;
    }
    let device = device.unwrap();
    let mut data = D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS::default();
    unsafe {
        device
            .CheckFeatureSupport(
                D3D11_FEATURE_D3D10_X_HARDWARE_OPTIONS,
                &mut data as *mut _ as _,
                std::mem::size_of::<D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS>() as u32,
            )
            .context("Checking GPU device feature support")?;
    }
    if data
        .ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x
        .as_bool()
    {
        Ok(device)
    } else {
        Err(anyhow::anyhow!(
            "Required feature StructuredBuffer is not supported by GPU/driver"
        ))
    }
}
