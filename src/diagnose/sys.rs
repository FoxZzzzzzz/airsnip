// SPDX-License-Identifier: GPL-3.0-only
//! 系统信息采集（SYS-8）：反馈 issue 时一键贴出运行环境。
//!
//! 只收集排查「截图 / 贴图 / 多屏」类问题用得上的项：Windows 版本、显示器
//! （分辨率 / 缩放 / HDR / 排布）、显卡、是否开机自启。
//! **不碰用户名、保存路径、截图内容**——这些对定位没用，只带来隐私风险。

use std::mem::size_of;

use windows::Win32::Foundation::{ERROR_SUCCESS, LPARAM, RECT};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows::core::{BOOL, PCWSTR};

use crate::capture::hdr_display::{DisplayColorState, state_for_device};

/// 注册表里的系统版本键。
const VERSION_KEY: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
/// Windows 11 的起始内核版本号（Win11 的 `ProductName` 仍写「Windows 10」）。
const WIN11_BUILD: u32 = 22_000;
/// 适配器枚举上限：正常机器 1-3 个，留足余量又不会死循环。
const MAX_ADAPTERS: u32 = 8;

/// 单台显示器（几何与缩放都是物理像素口径）。
#[derive(Debug, Clone)]
pub struct Monitor {
    /// GDI 设备名（`\\.\DISPLAY1`），HDR 状态按它匹配。
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// 有效 DPI（96 = 100%）。
    pub dpi: u32,
    pub primary: bool,
    pub hdr: bool,
}

impl Monitor {
    /// 缩放百分比（125% / 150% …）。
    pub fn scale_percent(&self) -> u32 {
        (self.dpi as f32 / 96.0 * 100.0).round() as u32
    }
}

/// 运行环境快照。
#[derive(Debug, Clone)]
pub struct SystemInfo {
    /// 如「Windows 11 专业版 24H2 26100.1742」，取不到时给中文兜底。
    pub os: String,
    pub monitors: Vec<Monitor>,
    pub gpus: Vec<String>,
    pub autostart: bool,
}

/// 采集当前运行环境（截图 / 贴图链路之外的一次性查询，不在待机路径上）。
pub fn collect() -> SystemInfo {
    let hdr = crate::capture::hdr_display::active_display_states();
    SystemInfo {
        os: os_version(),
        monitors: monitors(&hdr),
        gpus: gpu_names(),
        autostart: crate::app::autostart::autostart_enabled(),
    }
}

/// Windows 版本：`ProductName` + `DisplayVersion` + `CurrentBuildNumber` + `UBR`。
fn os_version() -> String {
    let Some(build_text) = reg_string("CurrentBuildNumber") else {
        return "Windows（版本未识别）".to_string();
    };
    let build: u32 = build_text.trim().parse().unwrap_or(0);
    let mut product = reg_string("ProductName").unwrap_or_else(|| "Windows".to_string());
    if build >= WIN11_BUILD {
        product = product.replace("Windows 10", "Windows 11");
    }
    let display = reg_string("DisplayVersion").unwrap_or_default();
    let ubr = reg_dword("UBR").unwrap_or(0);
    let mut out = product.trim().to_string();
    if !display.trim().is_empty() {
        out.push(' ');
        out.push_str(display.trim());
    }
    out.push(' ');
    out.push_str(build_text.trim());
    if ubr > 0 {
        out.push('.');
        out.push_str(&ubr.to_string());
    }
    out
}

/// 字符串值（REG_SZ）：键或值不存在时返回 None。
fn reg_string(name: &str) -> Option<String> {
    let key = wide(VERSION_KEY);
    let value = wide(name);
    let mut buf = vec![0u16; 256];
    let mut len = (buf.len() * size_of::<u16>()) as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if err != ERROR_SUCCESS {
        return None;
    }
    // len 是字节数，末尾含一个 NUL
    let n = (len as usize / size_of::<u16>()).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]))
}

/// 双字值（REG_DWORD），取不到时返回 None。
fn reg_dword(name: &str) -> Option<u32> {
    let key = wide(VERSION_KEY);
    let value = wide(name);
    let mut out: u32 = 0;
    let mut len = size_of::<u32>() as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut out as *mut u32).cast()),
            Some(&mut len),
        )
    };
    (err == ERROR_SUCCESS).then_some(out)
}

/// 枚举显示器期间的收集上下文（回调只带一个 LPARAM）。
struct Walk<'a> {
    out: Vec<Monitor>,
    hdr: &'a [(String, DisplayColorState)],
}

/// 枚举全部显示器：几何 + 有效 DPI + HDR 状态。
fn monitors(hdr: &[(String, DisplayColorState)]) -> Vec<Monitor> {
    let mut walk = Walk { out: Vec::new(), hdr };
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor),
            LPARAM((&mut walk as *mut Walk) as isize),
        );
    }
    walk.out
}

unsafe extern "system" fn enum_monitor(hmon: HMONITOR, _hdc: HDC, _clip: *mut RECT, data: LPARAM) -> BOOL {
    unsafe {
        let walk = &mut *(data.0 as *mut Walk);
        if let Some(m) = read_monitor(hmon, walk.hdr) {
            walk.out.push(m);
        }
    }
    // 返回 TRUE 继续枚举下一台
    BOOL(1)
}

/// 读单台显示器的几何、DPI 与 HDR 状态。
fn read_monitor(hmon: HMONITOR, hdr: &[(String, DisplayColorState)]) -> Option<Monitor> {
    unsafe {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(hmon, &mut info.monitorInfo).as_bool() {
            return None;
        }
        let name = String::from_utf16_lossy(
            &info
                .szDevice
                .iter()
                .take_while(|c| **c != 0)
                .copied()
                .collect::<Vec<_>>(),
        );
        let (mut dx, mut dy) = (96u32, 96u32);
        if GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).is_err() {
            dx = 96; // 查询失败按 100% 处理，宁可少写也不写错
        }
        let r = info.monitorInfo.rcMonitor;
        Some(Monitor {
            hdr: state_for_device(hdr, &name).hdr_enabled,
            name,
            x: r.left,
            y: r.top,
            width: r.right - r.left,
            height: r.bottom - r.top,
            dpi: dx,
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        })
    }
}

/// 显卡名（跳过 Microsoft Basic Render Driver 这类软件适配器，去重）。
fn gpu_names() -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return out;
        };
        for i in 0..MAX_ADAPTERS {
            let Ok(adapter) = factory.EnumAdapters1(i) else {
                break;
            };
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let name = String::from_utf16_lossy(
                &desc
                    .Description
                    .iter()
                    .take_while(|c| **c != 0)
                    .copied()
                    .collect::<Vec<_>>(),
            );
            if !name.trim().is_empty() && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// 窄字符串 → 以 NUL 结尾的宽字符串（注册表 API 要 `PCWSTR`）。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 查询本身不应 panic；单屏 / 无 HDR 环境下的返回值也必须是自洽的。
    #[test]
    fn collect_is_safe_and_consistent() {
        let info = collect();
        assert!(!info.monitors.is_empty(), "至少应枚举到一块显示器");
        for m in &info.monitors {
            assert!(m.width > 0 && m.height > 0, "{m:?}");
            assert!(m.scale_percent() >= 100, "缩放不应低于 100%：{m:?}");
        }
        assert!(info.monitors.iter().any(|m| m.primary), "应有一块主屏");
        assert!(!info.os.is_empty());
        for g in &info.gpus {
            assert!(!g.trim().is_empty());
        }
    }

    /// 缩放百分比换算（96 → 100%、120 → 125%、144 → 150%）。
    #[test]
    fn scale_percent_follows_dpi() {
        let m = |dpi| Monitor {
            name: "\\\\.\\DISPLAY1".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            dpi,
            primary: true,
            hdr: false,
        };
        assert_eq!(m(96).scale_percent(), 100);
        assert_eq!(m(120).scale_percent(), 125);
        assert_eq!(m(144).scale_percent(), 150);
        assert_eq!(m(192).scale_percent(), 200);
    }
}
