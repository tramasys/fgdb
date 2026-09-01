use gtk::gdk;
use vte4::prelude::*;

#[derive(Clone, Debug)]
pub struct Theme {
    pub source_scheme: &'static str,
    pub colors: Colors,
}

#[derive(Clone, Debug)]
pub struct Colors {
    pub background: &'static str,
    pub surface: &'static str,
    pub raised: &'static str,
    pub border: &'static str,
    pub foreground: &'static str,
    pub muted: &'static str,
    pub accent: &'static str,
    pub accent_hover: &'static str,
    pub success: &'static str,
    pub warning: &'static str,
    pub danger: &'static str,
    pub terminal_background: &'static str,
}

impl Theme {
    pub const fn graphite() -> Self {
        Self {
            source_scheme: "carbon",
            colors: Colors {
                background: "#000000",
                surface: "#0e0e0e",
                raised: "#202020",
                border: "#2b2b2b",
                foreground: "#dedede",
                muted: "#8f8f8f",
                accent: "#84a9ce",
                accent_hover: "#a8c8e8",
                success: "#88b89a",
                warning: "#d0b56f",
                danger: "#cf7777",
                terminal_background: "#000000",
            },
        }
    }

    pub fn install(&self) {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&self.stylesheet());

        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    }

    pub fn style_terminal(&self, terminal: &vte4::Terminal) {
        let foreground = rgba(self.colors.foreground);
        let background = rgba(self.colors.terminal_background);
        let palette = [
            rgba("#111111"),
            rgba("#b86f6f"),
            rgba("#7dac8d"),
            rgba("#b8a06c"),
            rgba("#7f91ad"),
            rgba("#a17fa8"),
            rgba("#739da3"),
            rgba("#c5c5c5"),
            rgba("#5d5d5d"),
            rgba("#d18484"),
            rgba("#93c3a4"),
            rgba("#d0b77c"),
            rgba("#96a8c3"),
            rgba("#b996c0"),
            rgba("#8bb5bb"),
            rgba("#eeeeee"),
        ];
        let palette_refs = palette.each_ref();
        terminal.set_colors(Some(&foreground), Some(&background), &palette_refs);
    }

    pub fn source_style_scheme(&self) -> Option<sourceview5::StyleScheme> {
        let manager = sourceview5::StyleSchemeManager::default();
        manager.prepend_search_path(&format!("resource://{}/themes", crate::RESOURCE_PREFIX));
        manager.force_rescan();
        manager
            .scheme(self.source_scheme)
            .or_else(|| manager.scheme("Adwaita-dark"))
    }

    fn stylesheet(&self) -> String {
        let colors = &self.colors;
        format!(
            r#"
@define-color app_bg {background};
@define-color app_surface {surface};
@define-color app_raised {raised};
@define-color app_border {border};
@define-color app_fg {foreground};
@define-color app_muted {muted};
@define-color app_placeholder alpha(@app_muted, 0.74);
@define-color app_accent {accent};
@define-color app_accent_hover {accent_hover};
@define-color app_success {success};
@define-color app_warning {warning};
@define-color app_danger {danger};

* {{
    font-family: monospace;
    border-radius: 0;
    box-shadow: none;
    outline: none;
    outline-width: 0;
    text-shadow: none;
}}

window.fgdb-window,
window.fgdb-window:backdrop,
window.fgdb-window.csd,
window.fgdb-window.csd:backdrop,
window.fgdb-window.solid-csd,
window.fgdb-window.solid-csd:backdrop,
.debugger-root {{
    background: @app_bg;
    color: @app_fg;
    font-size: 12px;
    border: 0;
    box-shadow: none;
}}

entry > text > placeholder {{ color: @app_placeholder; }}

headerbar.topbar {{
    min-height: 34px;
    padding: 0;
    background: @app_bg;
    background-image: none;
    box-shadow: none;
    border-top: 0;
    border-left: 0;
    border-right: 0;
    border-bottom: 1px solid @app_border;
}}

headerbar.topbar .titlebar-identity {{ padding: 0 8px; }}

headerbar.topbar .titlebar-actions {{ min-height: 33px; }}

headerbar.topbar .window-controls {{
    margin: 0;
    padding: 0;
    border-spacing: 0;
}}

headerbar.topbar button.window-control {{
    min-width: 42px;
    min-height: 33px;
    margin: 0;
    padding: 0;
    color: @app_muted;
    background: transparent;
    border: 0;
    font-family: sans-serif;
    font-size: 15px;
    font-weight: 400;
}}

headerbar.topbar button.window-control:hover,
headerbar.topbar button.window-control:focus-visible {{
    color: @app_fg;
    background: @app_raised;
}}

headerbar.topbar button.window-control:active {{
    color: @app_fg;
    background: alpha(@app_fg, 0.18);
}}

headerbar.topbar button.window-control.close:hover,
headerbar.topbar button.window-control.close:active {{
    color: @app_bg;
    background: @app_danger;
}}

.app-title {{
    font-size: 13px;
    font-weight: 700;
}}

.target-label,
.muted {{ color: @app_muted; }}

.panel {{
    background: @app_surface;
    border: 0;
}}

.panel-header {{
    min-height: 25px;
    padding: 0 6px;
    background: @app_surface;
    border-bottom: 1px solid @app_border;
}}

.panel-header.terminal-header {{
    padding-right: 0;
    border-bottom: 0;
}}

.subpanel-header {{
    min-height: 23px;
    padding: 0 3px;
    background: @app_surface;
}}

.section-title {{
    color: @app_muted;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.6px;
}}

.sidebar {{
    padding: 4px;
    background: @app_surface;
}}

.stack-row {{
    min-height: 31px;
    padding: 3px 4px;
}}

.breakpoint-row {{
    min-height: 42px;
    background: alpha(@app_fg, 0.018);
}}

.stack-row.breakpoint-row:nth-child(odd) {{
    background: alpha(@app_fg, 0.055);
}}

.stack-row.breakpoint-row:nth-child(even) {{
    background: alpha(@app_fg, 0.018);
}}

.stack-row.breakpoint-row:hover {{
    background: alpha(@app_fg, 0.085);
}}

.breakpoint-row-disabled {{
    color: @app_muted;
}}

.breakpoint-row-pending {{
    border-left: 2px solid @app_warning;
}}

.breakpoint-location-row {{
    min-height: 34px;
    padding: 2px 4px 2px 22px;
    background: alpha(@app_fg, 0.025);
    border-left: 1px solid alpha(@app_accent, 0.34);
}}

.breakpoint-location-row:hover {{ background: alpha(@app_fg, 0.075); }}

button.breakpoint-location-badge {{
    min-width: 36px;
    font-size: 10px;
}}

button.breakpoint-badge {{
    min-width: 24px;
    min-height: 20px;
    padding: 1px 4px;
    font-weight: 700;
    border: 0;
}}

button.breakpoint-badge-enabled {{
    color: @app_surface;
    background: @app_success;
}}

button.breakpoint-badge-enabled:hover,
button.breakpoint-badge-enabled:focus,
button.breakpoint-badge-enabled:focus-visible,
button.breakpoint-badge-enabled:active,
button.breakpoint-badge-enabled:checked {{
    color: @app_bg;
    background: @app_success;
}}

button.breakpoint-badge-disabled {{
    color: @app_muted;
    background: @app_raised;
}}

button.breakpoint-badge-disabled:hover,
button.breakpoint-badge-disabled:focus,
button.breakpoint-badge-disabled:focus-visible,
button.breakpoint-badge-disabled:active,
button.breakpoint-badge-disabled:checked {{
    color: @app_muted;
    background: @app_raised;
}}

.breakpoint-gutter {{
    color: @app_fg;
    background: transparent;
    border: 0;
    font-size: 10px;
}}

.breakpoint-condition {{
    color: @app_warning;
    background: alpha(@app_warning, 0.055);
    font-weight: 700;
    padding: 1px 3px;
}}

.breakpoint-metadata {{
    color: @app_accent;
    font-size: 10px;
    padding: 1px 3px;
}}

.breakpoint-commands {{
    color: @app_success;
    padding: 1px 3px;
}}

.value-editor-validation {{
    color: @app_danger;
    font-weight: 700;
}}

.field-label {{
    color: @app_muted;
    font-weight: 700;
}}

.watchpoint-row {{ background: alpha(@app_accent, 0.06); }}

button.stack-frame {{
    min-height: 31px;
    padding: 3px 4px;
    background: transparent;
    border: 0;
}}

button.stack-frame:hover {{ background: @app_raised; }}

button.stack-frame:nth-child(odd),
.stack-row:nth-child(odd) {{
    background: alpha(@app_fg, 0.035);
}}

button.stack-frame.current-debug-item {{
    background: alpha(@app_accent, 0.16);
}}

button.stack-frame:hover {{ background: alpha(@app_fg, 0.11); }}

button.stack-frame.current-debug-item:hover {{
    background: alpha(@app_accent, 0.23);
}}

.thread-heading {{
    color: @app_accent;
    font-weight: 700;
}}

.thread-name {{ color: @app_fg; }}

.thread-detail {{
    color: @app_muted;
    font-size: 11px;
}}

.thread-workspace {{
    padding: 5px;
    background: @app_bg;
}}

.thread-workspace-summary {{
    min-height: 20px;
    color: @app_accent_hover;
    font-size: 10px;
    font-weight: 700;
}}

.thread-workspace entry,
.thread-workspace dropdown > button {{
    min-height: 24px;
    padding: 1px 5px;
    color: @app_fg;
    background: @app_surface;
    border: 1px solid @app_border;
}}

entry.thread-search > image:first-child {{
    margin-right: 6px;
}}

entry.stop-point-search > image:first-child {{
    margin-right: 6px;
}}

dropdown.thread-dropdown > button,
dropdown.thread-dropdown > button cellview {{
    color: @app_fg;
    background: @app_surface;
    background-image: none;
}}

dropdown.thread-dropdown > button:hover,
dropdown.thread-dropdown > button:focus,
dropdown.thread-dropdown > button:focus-visible,
dropdown.thread-dropdown > button:active,
dropdown.thread-dropdown > button:checked,
dropdown.thread-dropdown > button:hover cellview,
dropdown.thread-dropdown > button:focus cellview,
dropdown.thread-dropdown > button:active cellview,
dropdown.thread-dropdown > button:checked cellview {{
    color: @app_fg;
    background: alpha(@app_accent, 0.14);
    background-image: none;
    border-color: @app_accent;
    box-shadow: none;
}}

dropdown.thread-dropdown popover > contents,
dropdown.thread-dropdown popover listview,
dropdown.thread-dropdown popover listview > row {{
    color: @app_fg;
    background: @app_surface;
    background-image: none;
}}

dropdown.thread-dropdown popover > contents {{
    padding: 2px;
    border: 1px solid @app_border;
    box-shadow: none;
}}

dropdown.thread-dropdown popover listview > row:hover {{
    color: @app_fg;
    background: @app_raised;
}}

dropdown.thread-dropdown popover listview > row:selected {{
    color: @app_fg;
    background: alpha(@app_accent, 0.20);
}}

.thread-workspace button {{
    min-height: 25px;
    padding: 2px 5px;
    background: @app_surface;
    border: 1px solid @app_border;
}}

.thread-workspace button:hover,
.thread-workspace button:focus-visible {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.12);
    border-color: alpha(@app_accent, 0.45);
}}

.thread-workspace button.stack-frame {{
    min-height: 31px;
    padding: 3px 4px;
    background: transparent;
    border: 0;
}}

.thread-workspace button.stack-frame:hover,
.thread-workspace button.stack-frame:focus-visible {{
    color: @app_fg;
    background: alpha(@app_accent, 0.16);
}}

.thread-workspace button.stack-frame.current-debug-item {{
    background: alpha(@app_accent, 0.16);
}}

.thread-workspace button.stack-frame.current-debug-item:hover,
.thread-workspace button.stack-frame.current-debug-item:focus-visible {{
    background: alpha(@app_accent, 0.23);
}}

.thread-controls-disclosure > button.disclosure-header {{
    min-height: 23px;
    padding: 2px 4px;
    color: @app_muted;
    background: alpha(@app_fg, 0.025);
    border-top: 1px solid @app_border;
    border-bottom: 1px solid @app_border;
}}

.thread-controls-disclosure > button.disclosure-header:hover,
.thread-controls-disclosure > button.disclosure-header.disclosure-expanded {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.10);
}}

.thread-controls-disclosure > box:last-child {{ padding: 5px 0 2px; }}

window.thread-analysis-window,
window.thread-analysis-window > box,
window.thread-analysis-window notebook,
window.thread-analysis-window scrolledwindow,
window.thread-analysis-window viewport {{
    color: @app_fg;
    background: @app_bg;
}}

window.thread-analysis-window columnview.debug-table row {{
    color: @app_fg;
    background: @app_surface;
}}

window.thread-analysis-window columnview.debug-table row:nth-child(odd) {{
    background: alpha(@app_fg, 0.04);
}}

window.thread-analysis-window columnview.debug-table row:hover {{
    background: alpha(@app_accent, 0.10);
}}

.thread-backtrace-section {{
    padding: 7px;
    background: @app_surface;
    border: 1px solid @app_border;
}}

button.thread-backtrace-frame {{
    min-height: 34px;
    padding: 4px 7px;
    color: @app_fg;
    background: alpha(@app_fg, 0.025);
    border: 0;
    border-left: 2px solid @app_border;
}}

button.thread-backtrace-frame:hover {{
    background: alpha(@app_accent, 0.12);
    border-left-color: @app_accent;
}}

.thread-comparison-changed {{
    color: @app_warning;
    font-weight: 700;
}}

.lock-graph {{
    padding: 5px 0 0;
    background: @app_surface;
    border-top: 1px solid @app_border;
}}

.module-row {{
    padding: 3px 4px;
    border-bottom: 1px solid @app_border;
}}

.module-row:hover {{ background: @app_raised; }}
.module-name {{ color: @app_fg; }}
.module-range,
.module-path {{ color: @app_muted; font-size: 10px; }}
.module-symbol-state {{
    padding: 0 3px;
    font-size: 9px;
    font-weight: 700;
}}
.module-symbols-loaded {{ color: @app_success; }}
.module-symbols-missing {{ color: @app_warning; }}

.inferior-summary {{
    margin: 2px 4px 6px;
    padding: 6px;
    background: alpha(@app_fg, 0.018);
    border: 1px solid @app_border;
}}

.inferior-summary dropdown.inferior-selector {{
    min-height: 23px;
    padding: 0;
    background: transparent;
    border: 0;
    box-shadow: none;
}}

.inferior-summary dropdown.inferior-selector > button {{
    min-height: 27px;
    padding: 0 7px;
    color: @app_fg;
    background: alpha(@app_fg, 0.035);
    background-image: none;
    border: 1px solid @app_border;
    box-shadow: none;
}}

.inferior-summary dropdown.inferior-selector > button:hover,
.inferior-summary dropdown.inferior-selector > button:focus,
.inferior-summary dropdown.inferior-selector > button:focus-visible,
.inferior-summary dropdown.inferior-selector > button:checked {{
    color: @app_fg;
    background: alpha(@app_accent, 0.10);
    background-image: none;
    border-color: @app_accent;
    box-shadow: none;
}}

.inferior-summary dropdown.inferior-selector > button > box {{
    border-spacing: 5px;
}}

.inferior-summary dropdown.inferior-selector > button arrow {{
    min-width: 10px;
    min-height: 10px;
    margin: 0;
    padding: 0;
    color: @app_muted;
    opacity: 0.72;
    -gtk-icon-size: 10px;
}}

.inferior-selected-state,
.inferior-card-state {{
    padding: 0 3px;
    color: @app_muted;
    font-size: 9px;
    font-weight: 700;
}}

.inferior-selected-state {{
    min-height: 16px;
    padding: 1px 5px;
    background: alpha(@app_fg, 0.035);
}}

.inferior-stop-owner {{
    min-height: 18px;
    margin: 0;
    padding: 3px 6px;
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.055);
    border-left: 2px solid alpha(@app_accent, 0.7);
    font-size: 9px;
    font-weight: 700;
}}

.inferior-page {{ background: @app_bg; }}

.inferior-page-header {{
    min-height: 28px;
    margin: 4px 6px;
    padding: 2px 7px;
    background: alpha(@app_fg, 0.018);
    border: 1px solid @app_border;
}}

button.inferior-refresh {{
    min-width: 20px;
    min-height: 22px;
    padding: 0;
    color: @app_muted;
    background: transparent;
    background-image: none;
    border: 0;
    box-shadow: none;
}}

button.inferior-refresh:hover,
button.inferior-refresh:focus-visible {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.11);
    background-image: none;
    box-shadow: none;
}}

.inferior-navigation,
.inferior-policy {{
    margin: 0 6px 6px;
    padding: 7px;
    background: alpha(@app_fg, 0.018);
    border: 1px solid @app_border;
}}

.inferior-list-title {{
    margin: 0 6px;
    padding: 4px 7px 5px;
}}

.inferior-list-scroll {{ background: @app_bg; }}
.inferior-list {{ margin: 0 6px 6px; }}

button.inferior-inline-action,
button.inferior-policy-choice {{
    min-height: 29px;
    padding: 2px 7px;
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.06);
    border: 1px solid alpha(@app_border, 0.85);
}}

button.inferior-inline-action:hover,
button.inferior-policy-choice:hover,
button.inferior-policy-choice:checked {{
    color: @app_fg;
    background: alpha(@app_accent, 0.17);
}}

button.inferior-inline-action:disabled,
button.inferior-policy-choice:disabled {{
    color: alpha(@app_muted, 0.48);
    background: transparent;
}}

.inferior-detach-policy {{
    min-height: 26px;
    margin-top: 1px;
    color: @app_fg;
    font-size: 11px;
}}

.inferior-card {{
    margin-top: 6px;
    padding: 7px;
    background: alpha(@app_fg, 0.025);
    border: 1px solid @app_border;
    border-left: 2px solid transparent;
}}

.inferior-card:nth-child(odd) {{ background: alpha(@app_fg, 0.045); }}
.inferior-card-selected {{ border-left-color: @app_accent; }}
.inferior-card-stop-owner {{ background: alpha(@app_accent, 0.10); }}
.inferior-id {{ color: @app_accent_hover; font-weight: 700; }}
.inferior-name {{ color: @app_fg; }}
.inferior-facts,
.inferior-relationship {{ color: @app_muted; font-size: 10px; }}
.inferior-running {{ color: @app_success; }}
.inferior-stopped {{ color: @app_accent_hover; }}
.inferior-exited {{ color: @app_danger; }}
.inferior-unknown {{ color: @app_muted; }}

button.instruction-row {{
    min-height: 34px;
    padding: 2px 3px;
    background: transparent;
    border: 0;
}}

button.instruction-row:hover {{ background: @app_raised; }}

button.instruction-row.current-instruction {{
    background: alpha(@app_accent, 0.13);
}}

button.instruction-row.current-instruction:hover {{
    background: alpha(@app_accent, 0.20);
}}

.instruction-address {{
    color: @app_muted;
    font-size: 11px;
}}

.instruction-mnemonic {{
    color: @app_success;
    font-weight: 700;
}}

.instruction-opcodes {{
    color: @app_warning;
    font-size: 10px;
}}

.instruction-symbol {{
    color: @app_accent;
    font-size: 10px;
}}

.instruction-operands {{ color: @app_fg; }}

.instruction-source {{ color: @app_muted; }}

.function-boundary-cell {{
    box-shadow: inset 2px 0 alpha(@app_accent, 0.48);
}}

.disassembly-range {{
    padding-right: 4px;
    color: @app_muted;
    font-size: 10px;
}}

.disassembly-browser {{
    padding: 3px 4px;
    background: @app_surface;
    border-bottom: 1px solid @app_border;
}}

.disassembly-browser-row {{
    min-height: 23px;
}}

.disassembly-control-group {{
    min-height: 23px;
    background: @app_bg;
    border: 1px solid @app_border;
}}

.disassembly-control-label {{
    padding: 0 6px;
    color: @app_muted;
    font-size: 9px;
    font-weight: 700;
}}

.disassembly-browser entry {{
    min-height: 21px;
    padding: 0 6px;
    background: @app_bg;
    border: 1px solid @app_border;
}}

.disassembly-browser entry:focus {{ border-color: @app_accent; }}
.disassembly-browser entry.input-error {{ border-color: @app_danger; }}

.disassembly-browser button.inline-action {{
    min-height: 21px;
    padding: 0 7px;
    background: alpha(@app_raised, 0.72);
    border-left: 1px solid @app_border;
}}

.disassembly-browser button.inline-action:checked {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.16);
}}

.instruction-insight {{
    padding: 4px 5px;
    background: alpha(@app_fg, 0.025);
    border-top: 1px solid alpha(@app_fg, 0.05);
    border-bottom: 1px solid @app_border;
}}

.instruction-insight-line {{
    min-height: 24px;
    padding: 3px 7px;
    color: @app_muted;
    font-size: 11px;
    background: @app_surface;
    border-left: 2px solid @app_border;
}}

.instruction-flow-insight {{
    color: @app_accent_hover;
    font-weight: 700;
    background: alpha(@app_accent, 0.10);
    border-left-color: @app_accent;
}}

.instruction-arguments-insight {{
    color: @app_fg;
    background: alpha(@app_warning, 0.055);
    border-left-color: alpha(@app_warning, 0.70);
}}

.instruction-memory-insight {{
    color: @app_fg;
    background: alpha(@app_success, 0.055);
    border-left-color: alpha(@app_success, 0.70);
}}

.instruction-insight-line.branch-taken {{
    color: @app_success;
    font-weight: 700;
    background: alpha(@app_success, 0.10);
    border-left-color: @app_success;
}}
.instruction-insight-line.branch-not-taken {{
    color: @app_muted;
    background: alpha(@app_fg, 0.035);
    border-left-color: @app_muted;
}}

.instruction-cell {{
    min-height: 22px;
    padding: 2px 4px;
}}

columnview.debug-table {{
    color: @app_fg;
    background: @app_surface;
    border: 0;
}}

columnview.debug-table > header {{
    min-height: 23px;
    color: @app_muted;
    background: @app_raised;
    border: 0;
}}

columnview.debug-table.view > header > button {{
    min-height: 23px;
    padding: 0 4px;
    color: @app_muted;
    background: @app_raised;
    background-image: none;
    border-style: none;
    border-width: 0;
    border-color: transparent;
    border-right: 1px solid @app_border;
    border-bottom: 0;
    outline-style: none;
    outline-width: 0;
    outline-color: transparent;
    outline-offset: 0;
    box-shadow: none;
    transition: none;
}}

columnview.debug-table.view > header > button:hover,
columnview.debug-table.view > header > button:focus,
columnview.debug-table.view > header > button:focus-visible,
columnview.debug-table.view > header > button:active,
columnview.debug-table.view > header > button:checked {{
    color: @app_fg;
    background: alpha(@app_accent, 0.12);
    background-image: none;
    border-style: none;
    border-width: 0;
    border-color: transparent;
    border-right: 1px solid @app_border;
    border-bottom: 0;
    outline-style: none;
    outline-width: 0;
    outline-color: transparent;
    outline-offset: 0;
    box-shadow: none;
}}

columnview.debug-table.view > header > button sort-indicator:not(.ascending):not(.descending) {{
    min-width: 0;
    min-height: 0;
    opacity: 0;
}}

columnview.debug-table row {{
    min-height: 26px;
    background: transparent;
}}

columnview.debug-table row:nth-child(odd) {{
    background: alpha(@app_fg, 0.035);
}}

columnview.debug-table row:hover {{
    background: alpha(@app_fg, 0.11);
}}

columnview.debug-table row:selected {{
    color: @app_fg;
    background: alpha(@app_accent, 0.16);
}}

.debug-table-cell {{
    min-height: 22px;
    padding: 2px 4px;
}}

.local-name {{
    color: @app_accent;
    font-weight: 700;
}}

.local-type {{ color: mix(@app_warning, @app_fg, 0.24); }}
.local-value {{ color: @app_fg; }}
.local-details {{ color: @app_muted; }}
.local-details-error {{ color: @app_danger; }}
.local-changed-value {{ color: @app_warning; }}
.local-load-more {{
    color: @app_accent_hover;
    font-weight: 700;
}}

.locals-table treeexpander {{
    min-height: 25px;
    padding: 0;
}}

.local-name-cell {{
    min-height: 25px;
    padding: 0 5px 0 3px;
}}

.local-name-cell.local-expandable:hover .local-name {{
    color: @app_accent_hover;
}}

.local-disclosure {{
    min-width: 8px;
    color: @app_muted;
    font-size: 11px;
}}

.local-scope {{
    min-width: 31px;
    padding: 0 3px;
    color: @app_muted;
    font-size: 8px;
    font-weight: 700;
    background: alpha(@app_fg, 0.055);
    border: 1px solid alpha(@app_border, 0.8);
    border-radius: 2px;
}}

.local-changed-marker {{
    color: @app_warning;
    font-size: 7px;
}}

.locals-panel {{ background: @app_surface; }}

.locals-header {{
    min-height: 27px;
    padding: 0 5px;
    border-bottom: 1px solid alpha(@app_border, 0.65);
}}

.locals-summary {{
    color: @app_muted;
    font-size: 10px;
}}

.locals-toolbar {{
    min-height: 29px;
    padding: 3px 4px;
    background: @app_bg;
    border-bottom: 1px solid alpha(@app_border, 0.65);
}}

.locals-toolbar entry.locals-filter-entry {{
    min-height: 22px;
    padding-top: 0;
    padding-bottom: 0;
    color: @app_fg;
    background: @app_bg;
    border: 1px solid @app_border;
}}

.locals-toolbar entry.locals-filter-entry:focus-within {{ border-color: @app_accent; }}

button.locals-changed-filter {{
    min-height: 22px;
    color: @app_muted;
    background: @app_surface;
    border: 1px solid @app_border;
}}

button.locals-changed-filter:hover,
button.locals-changed-filter:focus-visible {{
    color: @app_fg;
    background: alpha(@app_fg, 0.08);
}}

button.locals-changed-filter:checked {{
    color: @app_warning;
    background: alpha(@app_warning, 0.12);
    border-color: alpha(@app_warning, 0.45);
}}

popover.local-variable-menu > contents {{
    min-width: 290px;
    padding: 4px;
    background: @app_surface;
    border: 1px solid @app_border;
}}

.local-variable-menu-summary {{
    padding: 4px 7px 5px;
    background: alpha(@app_fg, 0.025);
}}

.local-variable-menu-caption,
.local-variable-menu-section {{
    color: @app_muted;
    font-size: 10px;
    font-weight: 700;
}}

.local-variable-menu-section {{ padding: 1px 7px 2px; }}

.local-variable-menu-name {{
    color: @app_accent_hover;
    font-weight: 700;
}}

.local-variable-menu-type,
.local-variable-menu-value {{
    color: @app_muted;
    font-size: 10px;
}}

.local-variable-menu-content > separator {{
    min-height: 1px;
    margin: 2px 0;
    background: @app_border;
}}

button.local-variable-menu-action {{
    min-height: 31px;
    margin: 0;
    padding: 2px 7px;
    color: @app_fg;
    background: alpha(@app_fg, 0.025);
    border: 0;
    border-left: 2px solid transparent;
}}

button.local-variable-menu-action:hover,
button.local-variable-menu-action:focus-visible {{
    color: @app_fg;
    background: @app_raised;
    border-left-color: @app_accent;
}}

.local-variable-menu-action-label {{ font-weight: 700; }}

.local-variable-menu-action-detail {{
    color: @app_muted;
    font-size: 10px;
}}

button.local-variable-viewer-action .local-variable-menu-action-label {{
    color: @app_accent_hover;
}}

.variable-viewer-window {{
    color: @app_fg;
    background: @app_bg;
}}

.variable-viewer-identity {{
    padding: 8px 10px;
    background: @app_surface;
    border: 1px solid @app_border;
}}

.variable-viewer-name {{
    color: @app_accent_hover;
    font-weight: 700;
}}

.vector-lane-index {{
    min-width: 28px;
    color: @app_muted;
}}

window.vector-editor dropdown > button {{
    min-height: 26px;
    padding: 1px 5px;
    color: @app_fg;
    background: @app_bg;
    border: 1px solid @app_border;
}}

.register-group-panel {{
    background: @app_surface;
    border: 0;
}}

.register-disclosure > button.disclosure-header {{
    min-height: 24px;
    padding: 3px 6px 3px 5px;
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.10);
    border-top: 1px solid alpha(@app_accent, 0.22);
    border-bottom: 1px solid alpha(@app_accent, 0.22);
    border-left: 2px solid alpha(@app_accent, 0.48);
}}

.register-disclosure > button.disclosure-header .section-title {{
    color: @app_accent_hover;
    font-weight: 800;
}}

.register-disclosure > button.disclosure-header.disclosure-expanded {{
    background: alpha(@app_accent, 0.17);
    border-left-color: @app_accent_hover;
}}

.register-disclosure > button.disclosure-header:hover {{
    color: @app_fg;
    background: alpha(@app_accent, 0.24);
}}

.register-row {{
    min-height: 23px;
    padding: 1px 4px;
    background: transparent;
}}

.register-row:hover {{ background: @app_raised; }}

.register-name {{
    color: @app_accent;
    font-weight: 700;
}}

.register-value {{ color: @app_fg; }}
.register-details {{ color: @app_fg; }}
.register-zero {{ color: @app_muted; }}

.modified-register {{
    color: @app_warning;
}}

.stack-memory-row {{
    min-height: 42px;
    padding: 3px 4px;
    background: transparent;
}}

.stack-memory-row:hover {{ background: @app_raised; }}

.stack-register-marker {{
    color: @app_accent;
    font-weight: 700;
}}

.stack-position,
.stack-region {{ color: @app_muted; }}

.stack-references {{ color: @app_accent_hover; }}

.stack-word-inspector {{
    min-height: 112px;
    padding: 4px 5px 5px;
    background: @app_bg;
    border-top: 1px solid @app_border;
}}

.stack-word-inspector grid {{ margin-top: 2px; }}

.stack-inspector-key {{
    min-width: 92px;
    color: @app_muted;
    font-size: 10px;
}}

.stack-inspector-value {{
    min-height: 18px;
    color: @app_fg;
}}

.legend-swatch {{ font-size: 10px; }}
.legend-modified {{ color: @app_warning; }}
.memory-code {{ color: @app_success; }}
.memory-heap {{ color: @app_warning; }}
.memory-stack {{ color: @app_accent; }}
.memory-writable {{ color: @app_accent_hover; }}
.memory-readonly {{ color: @app_muted; }}
.memory-rwx {{ color: @app_danger; font-weight: 700; }}
.memory-string {{ color: @app_warning; }}
.memory-none {{ color: @app_fg; }}

.memory-watch-command {{
    padding: 5px 6px;
    background: @app_surface;
    border: 0;
    border-bottom: 1px solid @app_border;
}}

.memory-watch-options {{
    min-height: 26px;
}}

.memory-watch-command entry {{
    min-height: 27px;
    padding: 0 8px;
}}

.memory-watch-command spinbutton,
.memory-watch-command dropdown > button {{
    min-height: 24px;
    padding-top: 0;
    padding-bottom: 0;
}}

.memory-watch-command spinbutton {{ padding: 0; }}

.memory-watch-command spinbutton > text {{
    min-height: 22px;
    padding: 0 9px;
}}

.memory-watch-command spinbutton > button {{
    min-width: 27px;
    min-height: 22px;
    padding: 0 6px;
}}

.memory-watch-command dropdown > button {{
    padding-left: 10px;
    padding-right: 8px;
}}

.memory-inspector-section,
.memory-map-section,
.memory-watch-page {{
    background: @app_surface;
}}

.memory-inspector-section > .subpanel-header,
.memory-map-section > .subpanel-header {{
    min-height: 25px;
    padding: 0 6px;
    background: @app_raised;
    border-bottom: 1px solid @app_border;
}}

.memory-map-search {{
    min-height: 23px;
    margin: 1px 0;
}}

entry.memory-map-search > image:first-child {{
    margin-right: 5px;
}}

.memory-inspector-split > separator {{
    min-height: 3px;
    background: @app_border;
    border: 0;
}}

.memory-inspector-split > separator:hover {{ background: @app_accent; }}

notebook.memory-watch-notebook > header {{
    background: @app_bg;
    border-bottom: 1px solid @app_border;
}}

notebook.memory-watch-notebook > header > tabs {{
    padding: 0;
    margin: 0;
}}

notebook.memory-watch-notebook > header > tabs > tab {{
    min-height: 27px;
    padding: 0 5px 0 10px;
}}

notebook.memory-watch-notebook > header > tabs > tab:checked {{
    color: @app_accent_hover;
    background: @app_raised;
    box-shadow: inset 0 -2px @app_accent;
}}

.memory-watch-tab-close {{
    min-width: 19px;
    min-height: 19px;
    padding: 0;
    margin: 2px 0 2px 2px;
    color: @app_muted;
    background: transparent;
    border: 0;
    box-shadow: none;
}}

.memory-watch-tab-close:hover {{
    color: @app_fg;
    background: @app_hover;
}}

.memory-watch-toolbar {{
    min-height: 30px;
    padding: 3px 5px;
    background: @app_bg;
    border-bottom: 1px solid @app_border;
}}

.memory-watch-expression {{
    color: @app_accent_hover;
    font-weight: 700;
}}

.memory-watch-offset {{
    min-width: 54px;
    padding: 0 5px;
    color: @app_warning;
    background: alpha(@app_warning, 0.08);
}}

.memory-watch-summary {{
    min-height: 27px;
    padding: 2px 7px;
    color: @app_fg;
    background: alpha(@app_fg, 0.025);
    border-bottom: 1px solid @app_border;
}}

.memory-watch-range {{ color: @app_muted; }}
.memory-watch-error {{ color: @app_danger; font-weight: 700; }}

columnview.memory-watch-table row,
columnview.memory-map-table row {{ min-height: 28px; }}

.memory-watch-cell {{
    min-height: 24px;
    padding: 3px 6px;
    font-size: 12px;
}}

.memory-row-changed {{
    color: @app_warning;
    background: alpha(@app_warning, 0.10);
}}

.until-menu {{
    min-width: 330px;
    padding: 4px;
    color: @app_fg;
    background: @app_surface;
    border: 1px solid @app_border;
}}

.until-summary {{
    padding: 4px 7px 5px;
    background: alpha(@app_fg, 0.025);
}}

.until-menu > separator {{
    min-height: 1px;
    margin: 2px 0;
    background: @app_border;
}}

.session-menu {{
    min-width: 320px;
    padding: 5px;
    color: @app_fg;
    background: @app_surface;
    border: 1px solid @app_border;
}}

.session-summary {{
    padding: 6px 8px 7px;
    background: alpha(@app_fg, 0.025);
}}

.session-caption {{
    color: @app_muted;
    font-size: 10px;
    font-weight: 700;
}}

.session-kind {{
    color: @app_accent_hover;
    font-weight: 700;
}}

.session-target {{
    color: @app_fg;
    font-size: 10px;
}}

.session-menu > separator {{
    min-height: 1px;
    margin: 4px 0 3px;
    background: @app_border;
}}

.session-menu > button.session-action {{
    min-height: 29px;
    margin: 0;
    padding: 2px 8px;
    color: @app_fg;
    background: alpha(@app_fg, 0.025);
    border: 0;
    border-left: 2px solid transparent;
}}

.session-menu > button.session-action:hover,
.session-menu > button.session-action:focus-visible {{
    color: @app_fg;
    background: @app_raised;
    border-left-color: @app_accent;
}}

.session-menu > button.session-action:disabled {{
    color: @app_muted;
    background: transparent;
}}

.session-menu > button.session-utility-action {{
    margin-top: 2px;
}}

.session-capabilities {{
    margin: 7px 8px;
    color: @app_muted;
}}

.session-action-label {{ font-weight: 700; }}

.session-action-detail {{
    color: @app_muted;
    font-size: 10px;
}}

.session-menu > button.session-primary-action .session-action-label {{
    color: @app_accent_hover;
}}

.session-menu > button.session-action.danger-action .session-action-label {{
    color: @app_danger;
}}

.session-menu > button.session-action.configuration-warning .session-action-label,
.session-menu > button.session-action.configuration-warning .session-action-detail {{
    color: @app_danger;
}}

.configuration-dialog {{
    color: @app_fg;
    background: @app_bg;
}}

.configuration-ok {{ color: @app_success; }}
.configuration-error {{ color: @app_danger; }}

.configuration-files,
.configuration-issues,
.configuration-grid {{
    padding: 6px;
    background: @app_surface;
    border: 1px solid @app_border;
}}

.configuration-fact {{
    min-height: 24px;
    padding: 1px 5px;
}}

.configuration-fact-name {{
    min-width: 120px;
    color: @app_muted;
    font-weight: 700;
}}

.configuration-fact-value,
.configuration-value {{
    color: @app_fg;
    font-family: monospace;
}}

.configuration-issue {{
    padding: 6px 8px;
    background: alpha(@app_danger, 0.06);
    border-left: 2px solid @app_danger;
}}

.configuration-issue-location {{
    color: @app_danger;
    font-family: monospace;
    font-size: 10px;
}}

.configuration-grid > label {{
    min-height: 23px;
    padding: 3px 6px;
}}

.configuration-grid-heading {{
    color: @app_muted;
    background: @app_raised;
    font-size: 10px;
    font-weight: 700;
}}

.configuration-setting {{
    min-width: 145px;
    color: @app_accent_hover;
    font-family: monospace;
}}

.debug-data-window {{
    color: @app_fg;
    background: @app_bg;
}}

.debug-data-heading {{
    min-height: 32px;
    padding: 5px 7px;
    background: alpha(@app_fg, 0.018);
    border: 1px solid @app_border;
}}

.debug-data-title {{ color: @app_fg; }}

.debug-data-window notebook {{
    background: @app_bg;
    border: 1px solid @app_border;
}}

.debug-data-window notebook > header {{
    background: @app_surface;
    border-bottom: 1px solid @app_border;
}}

.debug-data-window notebook > header > tabs {{
    padding: 2px;
}}

.debug-data-window notebook > header > tabs > tab {{
    min-height: 29px;
    margin: 0 1px;
    padding: 3px 9px;
    color: @app_muted;
    background: transparent;
    border: 0;
    box-shadow: none;
}}

.debug-data-window notebook > header > tabs > tab:hover {{
    color: @app_fg;
    background: alpha(@app_fg, 0.055);
}}

.debug-data-window notebook > header > tabs > tab:checked {{
    color: @app_fg;
    background: alpha(@app_accent, 0.08);
    border: 0;
    box-shadow: none;
}}

.debug-data-window notebook > stack {{
    background: @app_bg;
}}

.debug-data-section {{
    min-height: 20px;
    padding: 4px 7px 5px;
    color: @app_muted;
    background: alpha(@app_fg, 0.018);
    border: 1px solid @app_border;
    font-size: 10px;
    font-weight: 700;
}}

.debug-data-fact {{
    min-height: 29px;
    padding: 5px 7px;
    background: alpha(@app_fg, 0.025);
    border: 1px solid @app_border;
}}

.debug-data-fact-name {{
    min-width: 135px;
    color: @app_muted;
    font-weight: 700;
}}

.debug-data-control-card {{
    min-height: 31px;
    padding: 6px 7px;
    background: alpha(@app_fg, 0.025);
    border: 1px solid @app_border;
}}

.debug-data-row {{
    padding: 8px;
    background: alpha(@app_fg, 0.025);
    border: 1px solid @app_border;
    border-left: 2px solid transparent;
}}

.debug-data-row:nth-child(odd) {{ background: alpha(@app_fg, 0.045); }}

.debug-data-row .debug-data-fact {{
    min-height: 22px;
    padding: 2px 0;
    background: transparent;
    border: 0;
    border-top: 1px solid alpha(@app_border, 0.58);
}}

.debug-data-window button.inline-action {{
    min-height: 29px;
    margin: 0;
    padding: 2px 8px;
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.06);
    border: 1px solid alpha(@app_border, 0.85);
}}

.debug-data-window button.inline-action:hover,
.debug-data-window button.inline-action:focus-visible {{
    color: @app_fg;
    background: alpha(@app_accent, 0.17);
    border-color: alpha(@app_accent, 0.68);
}}

.debug-data-window button.inline-action:disabled {{
    color: alpha(@app_muted, 0.48);
    background: transparent;
    border-color: alpha(@app_border, 0.65);
}}

.debug-data-window entry {{
    min-height: 29px;
    padding: 2px 7px;
    color: @app_fg;
    background: @app_bg;
    border: 1px solid @app_border;
}}

.debug-data-window entry:focus-within {{ border-color: @app_accent; }}

.debug-data-window entry.debug-data-search image.left {{ margin-right: 7px; }}

.debug-data-source,
.debug-data-activity {{
    min-height: 25px;
    padding: 5px 7px;
    color: @app_fg;
    background: alpha(@app_fg, 0.025);
    border: 1px solid @app_border;
}}

.debug-data-printer-summary {{
    min-height: 29px;
    padding: 5px 8px;
    background: alpha(@app_fg, 0.025);
    border: 1px solid @app_border;
}}

.debug-data-printer-summary-count,
.debug-data-printer-scope-name,
.debug-data-printer-provider-name {{
    color: @app_fg;
    font-weight: 700;
}}

.debug-data-printer-match-count {{
    margin: 0 3px;
}}

.debug-data-printer-scope {{
    padding: 8px;
    background: alpha(@app_fg, 0.025);
    border: 1px solid @app_border;
    border-left: 2px solid alpha(@app_accent, 0.72);
}}

.debug-data-printer-scope-header {{
    min-height: 25px;
}}

.debug-data-printer-kind {{
    min-width: 52px;
    padding: 2px 6px;
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.12);
    border: 1px solid alpha(@app_accent, 0.35);
    font-size: 10px;
    font-weight: 700;
}}

.debug-data-printer-count {{
    color: @app_muted;
    font-size: 11px;
}}

.debug-data-printer-path {{
    min-height: 23px;
    padding: 3px 6px;
    color: @app_muted;
    background: alpha(@app_bg, 0.42);
    border: 1px solid alpha(@app_border, 0.65);
}}

.debug-data-printer-provider {{
    min-height: 24px;
    padding: 3px 5px;
    background: alpha(@app_fg, 0.018);
    border-bottom: 1px solid alpha(@app_border, 0.72);
}}

.debug-data-printer-grid {{
    margin: 0;
}}

.debug-data-printer {{
    min-height: 25px;
    padding: 4px 7px;
    color: @app_fg;
    background: alpha(@app_bg, 0.5);
    border: 1px solid alpha(@app_border, 0.72);
    font-family: monospace;
}}

.gef-tools-menu {{
    min-width: 300px;
    padding: 1px;
    background: @app_surface;
}}

.gef-tools-menu > button {{
    min-height: 18px;
    padding: 0 3px;
}}

.gef-tools-menu > separator {{ margin: 0; }}

.gef-tools-menu entry {{
    min-height: 19px;
    padding: 0 3px;
    color: @app_fg;
    background: @app_bg;
    border: 1px solid @app_border;
}}

.gef-tools-menu entry:focus {{ border-color: @app_accent; }}

.gef-tools-tabs > header {{
    min-height: 20px;
    background: @app_bg;
    border-bottom: 1px solid @app_border;
}}

.gef-tools-tabs > header > tabs > tab {{
    min-height: 19px;
    padding: 0 5px;
    border: 0;
    border-bottom: 1px solid transparent;
}}

.gef-tools-tabs > header > tabs > tab:checked {{
    color: @app_fg;
    background: @app_raised;
    border-bottom-color: @app_accent;
}}

.gef-tools-tabs > stack > box > button {{
    min-height: 18px;
    padding: 0 3px;
}}

.gef-command {{
    color: @app_muted;
    font-size: 10px;
}}

.gutter-breakpoint-menu,
.terminal-context-menu {{
    min-width: 172px;
    padding: 5px;
    background: @app_surface;
    border: 1px solid @app_border;
}}

.gutter-breakpoint-menu > label,
.terminal-context-menu > label {{ padding: 2px 7px 5px; }}

.gutter-breakpoint-menu > button,
.terminal-context-menu > button {{
    min-height: 25px;
    padding: 1px 8px;
    color: @app_fg;
    background: transparent;
    border-left: 2px solid transparent;
}}

.gutter-breakpoint-menu > button:hover,
.gutter-breakpoint-menu > button:focus-visible,
.terminal-context-menu > button:hover,
.terminal-context-menu > button:focus-visible {{
    color: @app_accent_hover;
    background: @app_raised;
    border-left-color: @app_accent;
}}

.gutter-breakpoint-menu > separator,
.terminal-context-menu > separator {{
    min-height: 1px;
    margin: 3px 0;
    background: @app_border;
}}

.until-menu > button.until-action {{
    min-height: 23px;
    margin: 0;
    padding: 0 7px;
    color: @app_fg;
    background: alpha(@app_fg, 0.025);
    border: 0;
    border-left: 2px solid transparent;
}}

.until-menu > button.until-action:hover,
.until-menu > button.until-action:focus-visible {{
    color: @app_fg;
    background: @app_raised;
    border-left-color: @app_accent;
}}

.until-menu > button.until-action:hover .session-action-label,
.until-menu > button.until-action:focus-visible .session-action-label {{
    color: @app_accent_hover;
}}

.until-condition {{ padding: 2px 0 0; }}

.until-condition > button {{
    min-width: 45px;
    min-height: 25px;
    padding: 1px 7px;
}}

.until-menu entry,
.sidebar entry,
.sidebar spinbutton,
.sidebar dropdown > button {{
    min-height: 23px;
    padding: 1px 4px;
    color: @app_fg;
    background: @app_bg;
    border: 1px solid @app_border;
}}

.until-menu entry:focus,
.sidebar entry:focus,
.sidebar spinbutton:focus-within,
.sidebar dropdown > button:focus {{
    border-color: @app_accent;
}}

.stack-row:hover {{ background: @app_raised; }}

button {{
    min-height: 23px;
    padding: 1px 7px;
    color: @app_fg;
    background: transparent;
    background-image: none;
    border: 0;
    outline: none;
    box-shadow: none;
}}

button:hover,
button:focus,
button:focus-visible {{
    background: alpha(@app_fg, 0.11);
    background-image: none;
    outline: none;
    box-shadow: none;
}}

button:active,
button:checked {{
    color: @app_fg;
    background: alpha(@app_accent, 0.17);
    background-image: none;
    outline: none;
    box-shadow: none;
}}

button:disabled {{
    color: alpha(@app_muted, 0.45);
    background: transparent;
}}

/* GTK paints the selected value inside a separate cell. Keep that internal
   layer transparent so it cannot introduce the system theme's bright blue
   selection color over fgdb's darker dropdown button. */
dropdown > button cellview,
dropdown > button > box > stack,
dropdown > button > box > stack > * {{
    color: @app_fg;
    background: transparent;
    background-image: none;
}}

dropdown popover > contents,
dropdown popover listview,
dropdown popover listview > row {{
    color: @app_fg;
    background: @app_surface;
    background-image: none;
}}

dropdown popover > contents {{
    border: 1px solid @app_border;
    box-shadow: none;
}}

dropdown popover listview > row:hover {{
    color: @app_fg;
    background: @app_raised;
}}

dropdown popover listview > row:selected {{
    color: @app_fg;
    background: alpha(@app_accent, 0.18);
}}

button.pause-availability-pending,
button.pause-availability-pending:hover,
button.pause-availability-pending:focus,
button.pause-availability-pending:focus-visible,
button.pause-availability-pending:active {{
    color: alpha(@app_muted, 0.45);
    background: transparent;
}}

/* Stop-only controls remain visually stable while execution interlocks them.
   They are still insensitive; this only avoids repainting the entire debugger
   chrome for every single step. */
.execution-interlocked:disabled {{
    opacity: 1;
}}

.execution-interlocked:disabled * {{
    opacity: 1;
}}

button.execution-interlocked:disabled,
.execution-interlocked:disabled button:disabled {{
    color: @app_fg;
}}

button.execution-interlocked:disabled:checked,
.execution-interlocked:disabled button:disabled:checked {{
    color: @app_fg;
    background: alpha(@app_accent, 0.17);
}}

button.primary-control.execution-interlocked:disabled,
button.inline-action.execution-interlocked:disabled,
.execution-interlocked:disabled button.primary-control:disabled,
.execution-interlocked:disabled button.inline-action:disabled {{
    color: @app_accent_hover;
    background: @app_raised;
}}

button.heap-inspector-action.execution-interlocked:disabled,
.execution-interlocked:disabled button.heap-inspector-action:disabled {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.07);
    border-left-color: alpha(@app_accent, 0.52);
}}

button.inline-action.danger-action.execution-interlocked:disabled {{
    color: @app_danger;
}}

.session-menu > button.session-action.execution-interlocked:disabled {{
    color: @app_fg;
    background: alpha(@app_fg, 0.025);
}}

button.signal-action.execution-interlocked:disabled {{
    color: @app_muted;
    background: @app_raised;
}}

button.signal-action.signal-caught.execution-interlocked:disabled {{
    color: @app_success;
    background: alpha(@app_success, 0.13);
}}

button.primary-control {{
    color: @app_accent_hover;
    background: @app_raised;
}}

button.primary-control:hover,
button.primary-control:focus-visible {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.20);
}}

button.until-control {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.055);
    box-shadow: inset 0 -1px alpha(@app_accent, 0.42);
}}

button.until-control:hover,
button.until-control:focus,
button.until-control:focus-visible {{
    color: @app_fg;
    background: alpha(@app_accent, 0.12);
    box-shadow: inset 0 -1px alpha(@app_accent, 0.7);
}}

button.until-control:active,
button.until-control:checked {{
    color: @app_fg;
    background: alpha(@app_accent, 0.17);
    box-shadow: inset 0 -2px @app_accent;
}}

button.until-control:disabled {{
    color: alpha(@app_accent_hover, 0.42);
    background: alpha(@app_accent, 0.025);
    box-shadow: inset 0 -1px alpha(@app_accent, 0.16);
}}

button.until-control.execution-interlocked:disabled {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.055);
    box-shadow: inset 0 -1px alpha(@app_accent, 0.42);
}}

button.toolbar-action {{
    padding-left: 8px;
    padding-right: 8px;
}}

button.toolbar-toggle:checked {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.14);
}}

button.inline-action {{
    min-height: 19px;
    padding: 0 6px;
    color: @app_accent_hover;
    background: @app_raised;
}}

button.inline-action.danger-action {{ color: @app_danger; }}

button.kernel-signal-filter {{
    min-height: 21px;
    padding: 0 7px;
    color: @app_muted;
    background: transparent;
    box-shadow: inset 2px 0 transparent;
}}

button.kernel-signal-filter:hover,
button.kernel-signal-filter:focus-visible {{
    color: @app_fg;
    background: alpha(@app_fg, 0.08);
}}

button.kernel-signal-filter:checked {{
    color: @app_success;
    background: alpha(@app_success, 0.17);
    box-shadow: inset 2px 0 @app_success;
}}

button.kernel-signal-filter:checked:hover,
button.kernel-signal-filter:checked:focus-visible {{
    color: @app_success;
    background: alpha(@app_success, 0.24);
}}

button.signal-action {{
    min-height: 21px;
    padding: 0 4px;
    color: @app_muted;
    background: @app_raised;
    font-size: 10px;
}}

button.signal-action:hover {{ color: @app_fg; }}

button.signal-action.signal-caught {{
    color: @app_success;
    background: alpha(@app_success, 0.13);
}}

.breakpoint-tool-section,
.signal-tool-section {{
    padding: 5px;
    background: alpha(@app_fg, 0.018);
    border: 1px solid @app_border;
}}

.watchpoint-controls entry,
.watchpoint-controls dropdown > button,
.watchpoint-controls button.watchpoint-add-action {{
    min-height: 27px;
}}

.watchpoint-controls entry {{ padding: 1px 7px; }}

.watchpoint-controls dropdown.watchpoint-access > button {{
    min-width: 92px;
    padding: 1px 7px;
    color: @app_fg;
    background: @app_bg;
    border: 1px solid @app_border;
}}

.watchpoint-controls dropdown.watchpoint-access > button:hover,
.watchpoint-controls dropdown.watchpoint-access > button:focus,
.watchpoint-controls dropdown.watchpoint-access > button:focus-visible,
.watchpoint-controls dropdown.watchpoint-access > button:checked {{
    color: @app_fg;
    background: @app_raised;
    border-color: @app_accent;
}}

.watchpoint-controls button.watchpoint-add-action {{
    min-width: 48px;
    padding: 1px 10px;
    border: 1px solid @app_border;
}}

button.catchpoint-action {{
    min-height: 25px;
    padding: 1px 6px;
    color: @app_muted;
    background: @app_raised;
    border: 1px solid alpha(@app_border, 0.85);
}}

button.catchpoint-action:hover,
button.catchpoint-action:focus-visible {{
    color: @app_fg;
    background: alpha(@app_accent, 0.12);
    border-color: alpha(@app_accent, 0.55);
}}

button.catchpoint-action.signal-caught {{
    color: @app_success;
    background: alpha(@app_success, 0.13);
    border-color: alpha(@app_success, 0.42);
}}

.signal-detail {{
    min-height: 27px;
    padding: 2px 5px;
    color: @app_muted;
    background: @app_bg;
    border-left: 2px solid @app_border;
}}

.signal-detail.signal-active {{
    color: @app_warning;
    border-left-color: @app_warning;
}}

.signal-disclosure > button.disclosure-header {{
    min-height: 20px;
    padding: 0;
}}

.signal-disclosure > button.disclosure-header:hover,
.signal-disclosure > button.disclosure-header.disclosure-expanded {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.10);
}}

.signal-disclosure > box:last-child {{ padding-top: 4px; }}
.signal-disclosure grid {{ padding: 0; }}

button.signal-clear-action {{
    min-height: 23px;
    padding: 1px 8px;
    border: 1px solid @app_border;
}}

.custom-signal-controls entry,
.custom-signal-controls button.signal-toggle-action {{
    min-height: 27px;
}}

.custom-signal-controls entry {{ padding: 1px 7px; }}

.custom-signal-controls button.signal-toggle-action {{
    min-width: 105px;
    padding: 1px 10px;
    border: 1px solid @app_border;
}}

window.value-editor,
window.value-editor > box {{
    color: @app_fg;
    background: @app_surface;
}}

window.session-editor,
window.session-editor > box {{
    color: @app_fg;
    background: @app_surface;
}}

window.session-editor button {{
    min-height: 27px;
    padding: 1px 9px;
}}

notebook.session-tabs > header > tabs {{ padding: 1px 4px; }}

notebook.session-tabs > header > tabs > tab {{
    min-height: 27px;
    margin: 0 1px;
    padding: 0 9px;
}}

window.session-editor entry,
window.session-editor spinbutton,
window.session-editor dropdown > button {{
    min-height: 27px;
    padding: 1px 6px;
    color: @app_fg;
    background: @app_bg;
    border: 1px solid @app_border;
}}

window.session-editor entry:focus,
window.session-editor spinbutton:focus-within,
window.session-editor dropdown > button:focus {{ border-color: @app_accent; }}

window.session-editor textview {{
    padding: 4px;
    color: @app_fg;
    background: @app_bg;
}}

window.session-editor scrolledwindow {{ border: 1px solid @app_border; }}

button.suggested-action {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.16);
}}

button.suggested-action:hover,
button.suggested-action:focus-visible {{
    color: @app_fg;
    background: alpha(@app_accent, 0.25);
}}

button.danger-action {{ color: @app_danger; }}

window.value-editor entry {{
    min-height: 28px;
    padding: 2px 6px;
    color: @app_fg;
    background: @app_bg;
    border: 1px solid @app_border;
}}

window.value-editor entry:focus {{ border-color: @app_accent; }}

window.value-editor entry selection {{
    color: @app_fg;
    background: alpha(@app_fg, 0.22);
}}

window.breakpoint-editor textview {{
    border: 1px solid @app_border;
}}

window.breakpoint-editor textview:focus-within {{ border-color: @app_accent; }}

window.breakpoint-editor checkbutton {{
    min-height: 23px;
    margin: 0;
    padding: 0;
    background: transparent;
    border: 0;
}}

window.breakpoint-editor checkbutton:hover,
window.breakpoint-editor checkbutton:focus,
window.breakpoint-editor checkbutton:focus-visible {{
    background: transparent;
    border: 0;
}}

window.breakpoint-editor checkbutton > check {{
    min-width: 14px;
    min-height: 14px;
    margin: 0 5px 0 0;
    padding: 0;
    border: 1px solid @app_border;
    box-shadow: none;
}}

window.breakpoint-editor checkbutton:hover > check,
window.breakpoint-editor checkbutton:focus > check,
window.breakpoint-editor checkbutton:focus-visible > check,
window.breakpoint-editor checkbutton:checked > check {{
    min-width: 14px;
    min-height: 14px;
    margin: 0 5px 0 0;
    padding: 0;
    border-width: 1px;
    box-shadow: none;
}}

window.breakpoint-editor spinbutton {{
    min-height: 32px;
    padding: 0;
    color: @app_fg;
    background: @app_bg;
    border: 1px solid @app_border;
}}

window.breakpoint-editor spinbutton:hover {{ border-color: @app_border; }}
window.breakpoint-editor spinbutton:focus-within {{ border-color: @app_accent; }}

window.breakpoint-editor spinbutton > button {{
    min-width: 28px;
    min-height: 30px;
    margin: 0;
    padding: 0;
    border: 0;
}}

window.breakpoint-editor > box > box:last-child > button,
window.breakpoint-editor > box > box:last-child > button:hover,
window.breakpoint-editor > box > box:last-child > button:focus,
window.breakpoint-editor > box > box:last-child > button:focus-visible,
window.breakpoint-editor > box > box:last-child > button:active {{
    min-height: 23px;
    margin: 0;
    padding: 1px 7px;
    border: 0;
}}

window.value-editor dropdown.value-editor-select > button {{
    min-height: 28px;
    padding: 1px 6px;
    color: @app_fg;
    background: @app_bg;
    background-image: none;
    border: 1px solid @app_border;
    box-shadow: none;
}}

window.value-editor dropdown.value-editor-select > button:hover,
window.value-editor dropdown.value-editor-select > button:focus,
window.value-editor dropdown.value-editor-select > button:focus-visible,
window.value-editor dropdown.value-editor-select > button:checked {{
    color: @app_fg;
    background: @app_raised;
    background-image: none;
    border: 1px solid @app_accent;
    box-shadow: none;
}}

window.value-editor dropdown.value-editor-select > button > box {{
    border-spacing: 6px;
}}

window.value-editor dropdown.value-editor-select > button arrow {{
    min-width: 12px;
    min-height: 12px;
    margin: 0;
    padding: 0;
    color: @app_muted;
    -gtk-icon-size: 12px;
}}

window.value-editor dropdown.value-editor-select popover > contents {{
    padding: 2px;
    color: @app_fg;
    background: @app_surface;
    border: 1px solid @app_border;
    box-shadow: none;
}}

window.value-editor dropdown.value-editor-select popover listview {{
    color: @app_fg;
    background: @app_surface;
}}

window.value-editor dropdown.value-editor-select popover listview > row {{
    min-height: 24px;
    padding: 1px 6px;
}}

window.value-editor dropdown.value-editor-select popover listview > row:hover {{
    background: @app_raised;
}}

window.value-editor dropdown.value-editor-select popover listview > row:selected {{
    color: @app_fg;
    background: alpha(@app_accent, 0.18);
}}

.status-readout {{
    min-height: 23px;
    margin: 0 6px 0 0;
    padding: 0 10px;
    color: @app_muted;
    background: alpha(@app_fg, 0.025);
    border-right: 1px solid @app_border;
}}

headerbar.topbar .execution-controls {{
    border-spacing: 1px;
}}

.status-detail {{
    min-height: 21px;
    padding: 0 7px;
    color: @app_muted;
    background: @app_bg;
    border-top: 1px solid @app_border;
    font-size: 10px;
}}

.workspace-footer {{
    min-height: 22px;
    background: @app_bg;
    border-top: 1px solid @app_border;
}}

.workspace-footer .status-detail {{ border-top: 0; }}

button.terminal-pane-toggle {{
    min-height: 21px;
    padding: 0 10px;
    color: @app_muted;
    background: @app_surface;
    border: 0;
    border-right: 1px solid @app_border;
}}

button.terminal-pane-toggle:hover {{
    color: @app_fg;
    background: @app_raised;
}}

button.terminal-pane-toggle:checked {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.14);
    box-shadow: inset 0 2px @app_accent;
}}

.status-ready {{ color: @app_success; }}
.status-running {{ color: @app_accent_hover; }}
.status-error {{ color: @app_danger; }}

.source-navigation-toolbar {{
    min-height: 25px;
    padding: 0;
    background: @app_surface;
    border-bottom: 1px solid @app_border;
}}

button.source-navigation-action,
menubutton.source-navigation-menu-button > button {{
    min-height: 24px;
    padding: 0 8px;
    color: @app_muted;
    background: transparent;
    border: 0;
    border-right: 1px solid alpha(@app_border, 0.75);
}}

button.source-navigation-action:hover,
button.source-navigation-action:focus-visible,
menubutton.source-navigation-menu-button > button:hover,
menubutton.source-navigation-menu-button > button:focus-visible {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.11);
}}

button.source-navigation-action:disabled {{
    color: alpha(@app_muted, 0.40);
    background: transparent;
}}

popover > contents .source-navigation-menu {{
    min-width: 310px;
    padding: 4px;
    background: @app_surface;
}}

button.source-navigation-menu-action {{
    min-height: 27px;
    padding: 1px 7px;
    color: @app_fg;
    background: transparent;
    border: 0;
}}

button.source-navigation-menu-action:hover,
button.source-navigation-menu-action:focus-visible {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.12);
}}

.source-find-bar {{
    min-height: 29px;
    padding: 3px 5px;
    background: @app_surface;
    border-bottom: 1px solid @app_border;
}}

.source-find-bar entry.source-search-entry {{ min-height: 21px; }}
.source-find-count {{
    min-width: 82px;
    color: @app_muted;
}}

window.source-palette,
window.source-palette > box {{
    color: @app_fg;
    background: @app_bg;
}}

window.source-palette entry.source-search-entry {{
    min-height: 29px;
    color: @app_fg;
    background: @app_surface;
    border: 1px solid @app_border;
}}

window.source-palette entry.source-search-entry:focus-within {{ border-color: @app_accent; }}

.source-search-entry image.left {{ margin-right: 6px; }}

.source-palette-results {{
    padding: 1px;
    background: @app_surface;
}}

button.source-palette-result {{
    min-height: 43px;
    padding: 4px 7px;
    color: @app_fg;
    background: transparent;
    border: 0;
    border-bottom: 1px solid alpha(@app_border, 0.65);
}}

button.source-palette-result:hover,
button.source-palette-result:focus-visible {{
    background: alpha(@app_accent, 0.13);
}}

.source-palette-primary {{ color: @app_accent_hover; }}
.source-palette-kind {{ color: @app_muted; font-size: 10px; }}
.source-palette-secondary {{ color: @app_muted; font-size: 11px; }}

.source-tree-panel {{ background: @app_surface; }}

.source-tree-toolbar {{
    min-height: 27px;
    padding: 3px 0 3px 3px;
    background: @app_bg;
    border-bottom: 1px solid alpha(@app_border, 0.75);
}}

.source-tree-toolbar entry.source-search-entry {{
    min-height: 22px;
    padding-top: 0;
    padding-bottom: 0;
    color: @app_fg;
    background: @app_bg;
    border: 1px solid @app_border;
}}

.source-tree-toolbar entry.source-search-entry:focus-within {{ border-color: @app_accent; }}

button.source-tree-refresh {{
    min-width: 18px;
    min-height: 24px;
    padding: 0;
    margin: 0;
    color: @app_muted;
    background: transparent;
    background-image: none;
    border: 0;
    box-shadow: none;
}}

button.source-tree-refresh:hover,
button.source-tree-refresh:focus-visible {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.11);
    background-image: none;
    box-shadow: none;
}}

.source-tree-status {{
    min-height: 20px;
    padding: 0 5px;
    color: @app_muted;
    font-size: 10px;
    background: @app_bg;
    border-bottom: 1px solid alpha(@app_border, 0.55);
}}

listview.source-tree-view {{
    color: @app_fg;
    background: @app_surface;
}}

listview.source-tree-view row {{
    min-height: 23px;
    padding: 0 3px;
    background: transparent;
}}

listview.source-tree-view row:hover {{ background: alpha(@app_accent, 0.08); }}
listview.source-tree-view row:selected {{ background: alpha(@app_accent, 0.19); }}

.source-tree-row {{ min-height: 22px; }}
.source-tree-disclosure {{
    min-width: 8px;
    color: @app_muted;
    font-size: 11px;
}}
.source-tree-icon {{ color: @app_muted; -gtk-icon-size: 13px; }}
.source-tree-name {{ color: @app_fg; }}
.source-tree-loaded {{ color: @app_success; font-size: 8px; }}

popover.source-tree-menu > contents {{
    min-width: 190px;
    padding: 4px;
    background: @app_surface;
    border: 1px solid @app_border;
}}

button.source-tree-menu-action {{
    min-height: 25px;
    padding: 1px 7px;
    color: @app_fg;
    background: transparent;
    border: 0;
}}

button.source-tree-menu-action:hover,
button.source-tree-menu-action:focus-visible {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.12);
}}

.source-tab {{
    padding: 0 2px;
    color: @app_fg;
}}

.executing-source-tab {{ color: @app_accent_hover; }}

button.source-tab-close {{
    min-width: 17px;
    min-height: 17px;
    padding: 0;
    color: @app_muted;
    background: transparent;
    border: 0;
}}

button.source-tab-close:hover {{
    color: @app_fg;
    background: alpha(@app_fg, 0.10);
}}

popover.source-tab-menu > contents {{
    min-width: 206px;
    padding: 4px;
    background: @app_surface;
    border: 1px solid @app_border;
}}

.source-tab-menu-action {{
    min-height: 25px;
    padding: 1px 8px;
    color: @app_fg;
    background: transparent;
    border: 0;
}}

.source-tab-menu-action:hover,
.source-tab-menu-action:focus-visible {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.12);
}}

.source-tab-menu-action:disabled {{ color: alpha(@app_muted, 0.55); }}

.source-tab-menu-separator {{ margin: 3px 5px; }}

textview,
textview text {{
    background: @app_bg;
    color: @app_fg;
}}

notebook,
notebook > stack {{
    background: @app_surface;
    border: 0;
    box-shadow: none;
}}

notebook > header {{
    background: @app_surface;
    border: 0;
    box-shadow: none;
}}

notebook > header > tabs > tab {{
    min-height: 24px;
    padding: 0 6px;
    color: @app_muted;
    background: transparent;
    background-image: none;
    border: 0;
    outline: none;
    box-shadow: none;
}}

notebook > header > tabs > tab:checked {{
    color: @app_fg;
    background: @app_raised;
    background-image: none;
    border: 0;
    outline: none;
    box-shadow: none;
}}

notebook > header > tabs > tab:hover {{
    color: @app_fg;
    background: @app_raised;
}}

/* Keep debugger category tabs distinct without making the data rows roomy. */
notebook.panel > header > tabs,
notebook.sidebar-tabs > header > tabs,
notebook.kernel-tabs > header > tabs,
notebook.gef-tools-tabs > header > tabs {{
    padding: 1px 2px;
}}

notebook.panel > header > tabs > tab,
notebook.sidebar-tabs > header > tabs > tab,
notebook.kernel-tabs > header > tabs > tab,
notebook.gef-tools-tabs > header > tabs > tab {{
    margin: 1px 0;
}}

notebook.panel > header > tabs > tab,
notebook.sidebar-tabs > header > tabs > tab,
notebook.kernel-tabs > header > tabs > tab {{
    padding-left: 8px;
    padding-right: 8px;
}}

notebook.gef-tools-tabs > header > tabs > tab {{
    padding-left: 6px;
    padding-right: 6px;
}}

notebook.panel > header > tabs > tab > label,
notebook.sidebar-tabs > header > tabs > tab > label,
notebook.kernel-tabs > header > tabs > tab > label,
notebook.gef-tools-tabs > header > tabs > tab > label {{
    margin: 0;
    padding: 0;
}}

.kernel-page,
stackswitcher.kernel-tabs {{ background: @app_surface; }}

.kernel-tab-navigation {{
    min-height: 26px;
    background: @app_surface;
}}

button.kernel-tab-nav-button {{
    min-width: 24px;
    min-height: 24px;
    margin: 1px 0;
    padding: 0;
    color: @app_muted;
    background: transparent;
    border: 0;
}}

button.kernel-tab-nav-button:hover {{
    color: @app_fg;
    background: @app_raised;
}}

button.kernel-tab-nav-button:disabled {{
    color: alpha(@app_muted, 0.25);
    background: transparent;
}}

stackswitcher.kernel-tabs {{
    padding: 1px 2px;
    border: 0;
    outline: 0;
    box-shadow: none;
    background-image: none;
}}

scrolledwindow.kernel-tabs-scroll,
scrolledwindow.kernel-tabs-scroll > viewport {{
    min-width: 0;
    min-height: 26px;
    background: @app_surface;
    border: 0;
    outline: 0;
    box-shadow: none;
}}

scrolledwindow.inspector-page-viewport,
scrolledwindow.inspector-page-viewport > viewport {{
    min-width: 0;
    background: @app_surface;
    border: 0;
    outline: 0;
    box-shadow: none;
}}

stackswitcher.kernel-tabs button {{
    min-height: 22px;
    margin: 1px 0;
    padding: 1px 6px;
    color: @app_muted;
    background: transparent;
    background-image: none;
    border: 0;
    outline: 0;
    box-shadow: none;
}}

stackswitcher.kernel-tabs button > label {{
    margin: 0;
    padding: 0;
}}

stackswitcher.kernel-tabs button:hover {{
    color: @app_fg;
    background: @app_raised;
}}

stackswitcher.kernel-tabs button:checked {{
    color: @app_fg;
    background: alpha(@app_fg, 0.08);
}}

.kernel-page.inspector-compact stackswitcher.kernel-tabs button {{
    padding-left: 5px;
    padding-right: 5px;
}}

.kernel-page.inspector-compact .subpanel-header {{
    padding-left: 2px;
    padding-right: 2px;
}}

.misc-startup-controls {{
    padding: 4px 5px 5px;
    background: @app_surface;
    border-bottom: 1px solid @app_border;
}}

.misc-startup-summary-cell {{
    min-height: 22px;
    padding: 2px 6px;
    background: @app_raised;
}}

.misc-startup-summary-key {{
    color: @app_muted;
    font-size: 10px;
    font-weight: bold;
}}

.misc-startup-summary-value {{
    min-width: 0;
    color: @app_fg;
}}

.misc-startup-warning {{
    padding: 4px 6px;
    color: @app_danger;
    background: alpha(@app_danger, 0.08);
    border-bottom: 1px solid alpha(@app_danger, 0.28);
}}

.misc-vector-section > .section-title {{
    min-height: 22px;
    padding: 2px 6px;
    color: @app_accent;
    background: @app_raised;
    border-bottom: 1px solid @app_border;
}}

.misc-vector-name {{
    color: @app_accent;
    font-weight: 700;
}}

.misc-vector-table label.debug-table-cell {{ min-height: 23px; }}

paned.misc-startup-split > separator {{
    min-height: 1px;
    margin: 0;
    padding: 0;
    background: @app_surface;
    background-image: none;
    border: 0;
}}

paned.misc-startup-split > separator:hover {{
    background: alpha(@app_accent, 0.35);
}}

.misc-data-summary {{
    min-height: 24px;
    padding: 3px 7px;
    color: @app_fg;
    background: @app_raised;
    border-bottom: 1px solid @app_border;
}}

.misc-data-note {{
    padding: 3px 7px;
}}

stackswitcher.allocator-view-tabs {{
    min-height: 25px;
    padding: 1px 3px;
    background: @app_surface;
    border-bottom: 1px solid @app_border;
}}

stackswitcher.allocator-view-tabs button {{
    min-height: 22px;
    margin: 0;
    padding: 1px 8px;
    color: @app_muted;
    background: transparent;
}}

stackswitcher.allocator-view-tabs button:hover {{
    color: @app_fg;
    background: alpha(@app_fg, 0.06);
}}

stackswitcher.allocator-view-tabs button:checked {{
    color: @app_accent_hover;
    font-weight: 700;
    background: alpha(@app_accent, 0.10);
}}

.allocator-detection-card {{
    padding: 7px 8px 6px;
    background: @app_raised;
    border-bottom: 1px solid @app_border;
}}

.allocator-detection-caption,
.allocator-detail-key,
.allocator-metric-key {{
    color: @app_muted;
    font-size: 10px;
    font-weight: 700;
}}

.allocator-detection-identity {{
    color: @app_accent;
    font-size: 15px;
    font-weight: 800;
}}

.allocator-detection-basis {{
    margin-top: 1px;
    padding: 2px 5px;
    color: @app_accent;
    background: alpha(@app_accent, 0.10);
    border-left: 2px solid @app_accent;
    font-size: 10px;
    font-weight: 700;
}}

.allocator-detection-basis.allocator-detection-warning {{
    color: @app_warning;
    background: alpha(@app_warning, 0.10);
    border-left-color: @app_warning;
}}

.allocator-detection-basis.allocator-detection-error {{
    color: @app_danger;
    background: alpha(@app_danger, 0.10);
    border-left-color: @app_danger;
}}

.allocator-detail {{
    margin-top: 3px;
    padding-top: 3px;
    border-top: 1px solid alpha(@app_border, 0.72);
}}

.allocator-detail-value {{
    color: @app_fg;
}}

.allocator-evidence-value {{
    color: @app_muted;
}}

.allocator-frontend-value {{
    color: @app_accent_hover;
}}

.allocator-detection-safety {{
    margin-top: 4px;
    padding: 3px 5px;
    color: @app_success;
    background: alpha(@app_success, 0.07);
    border-left: 2px solid alpha(@app_success, 0.72);
    font-size: 10px;
    font-weight: 700;
}}

.allocator-detection-safety.allocator-safety-warning {{
    color: @app_warning;
    background: alpha(@app_warning, 0.08);
    border-left-color: alpha(@app_warning, 0.72);
}}

flowbox.allocator-metrics {{
    background: @app_border;
}}

flowbox.allocator-metrics > flowboxchild {{
    min-width: 0;
    padding: 0;
    background: transparent;
}}

.allocator-metric-cell {{
    min-height: 35px;
    padding: 3px 7px;
    background: @app_surface;
}}

.allocator-metric-value {{
    color: @app_fg;
    font-weight: 700;
}}

.heap-inspector-controls {{
    padding: 6px 7px 7px;
    background: @app_surface;
    border-bottom: 1px solid @app_border;
}}

.heap-inspector-note {{
    padding-bottom: 2px;
    color: @app_muted;
}}

.heap-inspector-group-title {{
    color: @app_muted;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.4px;
}}

flowbox.heap-inspector-actions {{
    background: transparent;
}}

flowbox.heap-inspector-actions > flowboxchild {{
    min-width: 0;
    padding: 0;
    background: transparent;
}}

button.heap-inspector-action {{
    min-height: 24px;
    padding: 1px 8px;
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.07);
    border-left: 2px solid alpha(@app_accent, 0.52);
}}

button.heap-inspector-action:hover,
button.heap-inspector-action:focus-visible {{
    color: @app_fg;
    background: alpha(@app_accent, 0.15);
    border-left-color: @app_accent;
}}

button.heap-inspector-action:disabled {{
    color: alpha(@app_muted, 0.45);
    background: alpha(@app_fg, 0.025);
    border-left-color: alpha(@app_muted, 0.24);
}}

entry.heap-inspector-expression {{
    margin: 1px 0 2px;
}}

.heap-inspector-result-header {{
    min-height: 38px;
    padding: 4px 7px;
    background: @app_raised;
    border-bottom: 1px solid @app_border;
}}

.heap-table-controls {{
    min-height: 28px;
    padding: 3px 5px;
    background: @app_surface;
    border-bottom: 1px solid @app_border;
}}

.heap-table-controls button.inline-action {{
    min-height: 22px;
    padding-left: 8px;
    padding-right: 8px;
}}

.heap-inspector-command {{
    color: @app_accent;
    font-weight: 700;
}}

.heap-inspector-status {{
    color: @app_muted;
}}

.heap-inspector-status.heap-inspector-error {{
    color: @app_danger;
}}

.heap-inspector-status.heap-inspector-warning {{
    color: @app_warning;
}}

.heap-inspector-table label.debug-table-cell {{
    min-height: 23px;
}}

.heap-inspector-cell {{
    color: @app_fg;
}}

.heap-inspector-structure-cell {{
    font-weight: 700;
}}

.heap-inspector-location-cell {{
    color: @app_accent_hover;
}}

.heap-inspector-metric-cell,
.heap-inspector-details-cell {{
    color: @app_muted;
}}

.heap-inspector-state-cell {{
    margin: 3px 4px;
    min-height: 18px;
    padding: 0 5px;
    color: @app_muted;
    background: alpha(@app_fg, 0.045);
}}

.heap-inspector-state-cell.heap-state-active {{
    color: @app_success;
    background: alpha(@app_success, 0.10);
}}

.heap-inspector-state-cell.heap-state-idle {{
    color: @app_muted;
    background: alpha(@app_fg, 0.035);
}}

.heap-inspector-state-cell.heap-state-free {{
    color: @app_warning;
    background: alpha(@app_warning, 0.09);
}}

.heap-inspector-state-cell.heap-state-special {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.10);
}}

.heap-inspector-cell.heap-inspector-section-cell {{
    color: @app_accent;
    font-weight: 700;
}}

.heap-inspector-cell.heap-inspector-error-cell {{
    color: @app_danger;
    font-weight: 700;
}}

.heap-inspector-cell.heap-inspector-warning-cell {{
    color: @app_warning;
}}

.call-abi-context {{
    min-height: 24px;
    padding: 4px 7px;
    color: @app_accent;
    background: alpha(@app_accent, 0.08);
    font-weight: 700;
}}

.misc-data-table label.debug-table-cell {{ min-height: 23px; }}

paned.misc-data-split > separator {{
    min-height: 1px;
    margin: 0;
    padding: 0;
    background: @app_surface;
    background-image: none;
    border: 0;
}}

paned.misc-data-split > separator:hover {{
    background: alpha(@app_accent, 0.35);
}}

.kernel-table-controls {{ padding: 2px 3px; }}
.kernel-table-summary {{ padding: 3px 4px; }}

.kernel-change-controls {{
    min-height: 28px;
    padding: 2px 6px;
    background: @app_raised;
    border-top: 1px solid @app_border;
    border-bottom: 1px solid @app_border;
}}

.kernel-change-search {{
    min-height: 22px;
    color: @app_fg;
    background: @app_bg;
    border: 1px solid @app_border;
    outline: 0;
    box-shadow: none;
}}

.kernel-change-search:focus-within {{ border-color: @app_accent; }}

entry.kernel-table-search > image:first-child {{
    margin-right: 5px;
}}

entry.kernel-table-search {{
    min-width: 92px;
}}

.kernel-change-empty {{
    padding: 8px 12px;
    background: alpha(@app_surface, 0.88);
}}

.kernel-memory-summary {{
    padding: 4px 5px 6px;
    color: @app_fg;
    background: alpha(@app_fg, 0.018);
}}

.kernel-memory-meta {{ padding: 1px 3px 4px; }}

.kernel-memory-unit-grid {{
    border-top: 1px solid @app_border;
    border-left: 1px solid @app_border;
}}

.kernel-memory-unit-cell {{
    min-height: 21px;
    padding: 1px 5px;
    color: @app_fg;
    border-right: 1px solid @app_border;
    border-bottom: 1px solid @app_border;
}}

.kernel-memory-unit-header {{
    color: @app_muted;
    font-size: 10px;
    font-weight: 700;
    background: @app_raised;
}}

.kernel-memory-unit-even {{ background: alpha(@app_fg, 0.018); }}
.kernel-memory-unit-odd {{ background: alpha(@app_fg, 0.042); }}

.kernel-memory-explanation {{
    padding: 3px 7px 5px 7px;
    border-bottom: 1px solid @app_border;
}}

.kernel-private-summary-grid {{ background: @app_border; }}

.kernel-private-summary-cell {{
    min-height: 0;
    padding: 2px 7px 3px;
    background: alpha(@app_success, 0.045);
}}

.kernel-private-summary-value {{
    color: @app_success;
    font-weight: 700;
}}

.kernel-memory-subtitle {{
    min-height: 22px;
    padding: 2px 7px;
    background: @app_raised;
    border-top: 1px solid @app_border;
    border-bottom: 1px solid @app_border;
}}

.kernel-memory-category {{
    color: @app_accent_hover;
    font-weight: 700;
}}

.kernel-memory-exclusive {{ color: @app_success; }}

.kernel-memory-table label.debug-table-cell {{ min-height: 23px; }}

.kernel-change-table label.debug-table-cell {{ min-height: 23px; }}
.kernel-change-growth {{ color: @app_accent_hover; }}
.kernel-change-release {{ color: @app_success; }}
.kernel-change-idle {{ color: @app_muted; }}
.kernel-change-new {{ color: @app_success; font-weight: 700; }}
.kernel-change-removed {{ color: @app_danger; font-weight: 700; }}
.kernel-change-protection {{ color: @app_warning; font-weight: 700; }}
.kernel-change-modified {{ color: @app_accent_hover; font-weight: 700; }}

.kernel-fact-row {{
    min-height: 23px;
    padding: 1px 4px;
}}

.kernel-fact-row:nth-child(odd) {{ background: alpha(@app_fg, 0.035); }}

listview.kernel-overview-list > row {{
    color: @app_fg;
    background: transparent;
}}

listview.kernel-overview-list > row:hover {{
    color: @app_fg;
    background: @app_raised;
}}

listview.kernel-overview-list > row:selected {{
    color: @app_fg;
    background: alpha(@app_accent, 0.16);
}}

listview.kernel-overview-list > row:hover .muted,
listview.kernel-overview-list > row:selected .muted {{ color: @app_fg; }}

.kernel-page label selection {{
    color: @app_fg;
    background: alpha(@app_accent, 0.38);
}}

.kernel-section-heading {{
    min-height: 24px;
    padding: 3px 6px 3px 5px;
    border-top: 1px solid alpha(@app_accent, 0.22);
    border-bottom: 1px solid alpha(@app_accent, 0.22);
    border-left: 2px solid alpha(@app_accent, 0.48);
    background: alpha(@app_accent, 0.10);
}}

.kernel-section-heading .section-title {{
    color: @app_accent_hover;
    font-weight: 800;
}}

.kernel-section-heading.kernel-section-expanded {{
    border-left-color: @app_accent_hover;
    background: alpha(@app_accent, 0.17);
}}

listview.kernel-overview-list > row:hover .kernel-section-heading {{
    background: alpha(@app_accent, 0.24);
}}

.kernel-warnings {{
    padding: 3px 5px;
    background: alpha(@app_warning, 0.08);
    border-bottom: 1px solid alpha(@app_warning, 0.24);
}}

.kernel-warning {{
    color: @app_warning;
    font-size: 10px;
}}

.kernel-state-active {{
    color: @app_success;
    font-weight: 700;
}}

.kernel-state-warning {{
    color: @app_warning;
    font-weight: 700;
}}

.kernel-tls-table label.debug-table-cell {{
    min-height: 22px;
}}

.kernel-tls-metadata-header {{
    min-height: 27px;
    padding: 1px 4px 1px 6px;
    background: @app_raised;
    border-bottom: 1px solid @app_border;
}}

stackswitcher.kernel-tls-tabs {{
    padding: 0;
    background: transparent;
}}

stackswitcher.kernel-tls-tabs button {{
    min-height: 23px;
    margin: 1px;
    padding: 1px 8px;
    color: @app_muted;
    background: transparent;
    border: 0;
    outline: 0;
    box-shadow: none;
}}

stackswitcher.kernel-tls-tabs button:hover {{
    color: @app_fg;
    background: alpha(@app_fg, 0.06);
}}

stackswitcher.kernel-tls-tabs button:checked {{
    color: @app_fg;
    font-weight: 700;
    background: alpha(@app_fg, 0.10);
}}

.kernel-tls-empty {{
    padding: 18px;
    color: @app_muted;
}}

.kernel-process-target {{
    color: @app_accent;
    font-weight: 700;
}}

.kernel-numeric {{ color: @app_accent_hover; }}

.kernel-signals-table label.debug-table-cell,
.kernel-process-table label.debug-table-cell {{
    min-height: 22px;
}}

notebook > header > tabs > arrow {{
    background: transparent;
    border: 0;
    outline: none;
    box-shadow: none;
}}

button.disclosure-header {{
    min-height: 24px;
    padding: 0 2px;
    color: @app_muted;
    background: transparent;
    border: 0;
    outline: none;
    box-shadow: none;
}}

button.disclosure-header:hover {{
    color: @app_fg;
    background: @app_raised;
}}

.context-legend grid {{ padding: 2px 4px 4px; }}

.disclosure-arrow {{
    color: @app_muted;
    font-size: 15px;
}}

paned > separator {{
    min-width: 1px;
    min-height: 1px;
    background: alpha(@app_fg, 0.10);
}}

paned.workspace-columns > separator:hover {{ background: @app_accent; }}

/* Keep the inspector resize handle functional without drawing a seam through
   both the source area and the terminal below it. */
paned.workspace-inspector-split > separator {{
    min-width: 1px;
    margin: 0;
    padding: 0;
    background: @app_surface;
    background-image: none;
    border: 0;
}}

paned.workspace-inspector-split > separator:hover {{ background: @app_accent; }}

paned.context-split > separator {{
    min-height: 3px;
    background: alpha(@app_fg, 0.06);
    border-top: 1px solid alpha(@app_fg, 0.10);
}}

paned.context-split > separator:hover {{
    background: alpha(@app_accent, 0.20);
    border-top-color: @app_accent;
}}

scrollbar {{
    background: @app_surface;
}}

scrollbar slider {{
    min-width: 6px;
    min-height: 6px;
    margin: 2px;
    padding: 0;
    background: alpha(@app_fg, 0.38);
    border: 0;
}}

scrollbar slider:hover {{ background: alpha(@app_fg, 0.58); }}
"#,
            background = colors.background,
            surface = colors.surface,
            raised = colors.raised,
            border = colors.border,
            foreground = colors.foreground,
            muted = colors.muted,
            accent = colors.accent,
            accent_hover = colors.accent_hover,
            success = colors.success,
            warning = colors.warning,
            danger = colors.danger,
        )
    }
}

fn rgba(value: &str) -> gdk::RGBA {
    gdk::RGBA::parse(value).expect("built-in theme colors must be valid")
}
