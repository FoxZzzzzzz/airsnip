// SPDX-License-Identifier: GPL-3.0-only
//! 反馈用的系统信息（SYS-8）：设置「关于」页一键复制运行环境，用户粘到 issue 里。
//!
//! 采集在 `sys`（注册表 / 显示器 / DXGI）、排版在 `text`（一段紧凑文本，用于反馈 issue 的
//! 「日志与补充信息」字段），本模块只负责串联与写剪贴板。

pub mod sys;
pub mod text;

/// 生成反馈文本（Markdown）。关于页按钮与调试都走这一个入口。
pub fn report(config: &crate::settings::Config) -> String {
    let info = sys::collect();
    // 界面缩放实际系数要按光标所在显示器解析（与覆盖层同一套规则，ADR 0016）
    let scale = crate::overlay::ui::Ui::resolve(config.ui_scale).scale();
    text::render(&info, config, scale)
}

/// 生成反馈文本并写入剪贴板（`CF_UNICODETEXT`）。
pub fn copy_report(config: &crate::settings::Config) -> Result<(), String> {
    crate::output::copy_text_to_clipboard(&report(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实环境下生成一份文本：三行结构完整，且不含路径 / 用户名之类的隐私项。
    #[test]
    fn report_is_compact_and_private() {
        let cfg = crate::settings::Config::default();
        let text = report(&cfg);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[0].contains(env!("CARGO_PKG_VERSION")), "{text}");
        assert!(lines[1].starts_with("显示器："), "{text}");
        assert!(lines[2].starts_with("开关："), "{text}");
        // APPDATA / 用户目录等路径不该出现在反馈文本里
        if let Some(dir) = std::env::var_os("USERPROFILE") {
            let dir = dir.to_string_lossy().to_string();
            assert!(!dir.is_empty() && !text.contains(&dir), "不应包含用户目录：{text}");
        }
    }
}
