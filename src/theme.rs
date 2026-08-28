use gtk::gdk;
use vte4::prelude::*;

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: &'static str,
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
            name: "Carbon",
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
        manager.prepend_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/themes"));
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

.debug-state-stale {{ opacity: 0.56; }}

.panel-header {{
    min-height: 25px;
    padding: 0 6px;
    background: @app_surface;
    border-bottom: 1px solid @app_border;
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

.sidebar-row {{
    min-height: 24px;
    padding: 1px 4px;
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
.stack-row:nth-child(odd),
.sidebar-row:nth-child(odd) {{
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
    box-shadow: inset 0 1px alpha(@app_accent, 0.48);
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

.disassembly-browser-row {{ min-height: 23px; }}

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
}}

.disassembly-browser button.inline-action:checked {{
    color: @app_accent_hover;
    background: alpha(@app_accent, 0.16);
}}

.disassembly-browser dropdown.disassembly-syntax > button {{
    min-height: 21px;
    padding: 0 7px;
    color: @app_fg;
    background: @app_raised;
    border: 0;
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

columnview.debug-table > header button {{
    min-height: 23px;
    padding: 0 4px;
    color: @app_muted;
    background: @app_raised;
    border: 0;
    border-right: 1px solid @app_border;
}}

columnview.debug-table > header button:hover {{
    color: @app_fg;
    background: alpha(@app_accent, 0.12);
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

.local-type {{ color: @app_warning; }}
.local-value {{ color: @app_fg; }}
.local-details {{ color: @app_muted; }}
.local-details-error {{ color: @app_danger; }}
.local-load-more {{
    color: @app_accent_hover;
    font-weight: 700;
}}

.locals-table treeexpander {{
    min-height: 28px;
    padding-left: 2px;
}}

.locals-table treeexpander > label.local-name {{
    min-height: 24px;
    padding: 2px 6px 2px 4px;
}}

.locals-table treeexpander > label.local-expandable:hover {{
    background: alpha(@app_accent, 0.10);
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
    min-width: 230px;
    padding: 5px;
    background: @app_surface;
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

.gutter-breakpoint-menu {{
    min-width: 172px;
    padding: 5px;
    background: @app_surface;
    border: 1px solid @app_border;
}}

.gutter-breakpoint-menu > label {{ padding: 2px 7px 5px; }}

.gutter-breakpoint-menu > button {{
    min-height: 25px;
    padding: 1px 8px;
    color: @app_fg;
    background: transparent;
    border-left: 2px solid transparent;
}}

.gutter-breakpoint-menu > button:hover,
.gutter-breakpoint-menu > button:focus-visible {{
    color: @app_accent_hover;
    background: @app_raised;
    border-left-color: @app_accent;
}}

.gutter-breakpoint-menu > separator {{
    min-height: 1px;
    margin: 3px 0;
    background: @app_border;
}}

.until-menu > button {{
    min-height: 23px;
    padding: 1px 5px;
    background: alpha(@app_fg, 0.025);
    border-left: 2px solid transparent;
}}

.until-menu > button:hover,
.until-menu > button:focus-visible {{
    color: @app_accent_hover;
    background: @app_raised;
    border-left-color: @app_accent;
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

.sidebar-row:hover,
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
    border: 0;
    outline: none;
    box-shadow: none;
}}

button:active,
button:checked {{
    color: @app_fg;
    background: alpha(@app_accent, 0.17);
    background-image: none;
    border: 0;
    outline: none;
    box-shadow: none;
}}

button:disabled {{
    color: alpha(@app_muted, 0.45);
    background: transparent;
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

.signal-disclosure grid {{ padding: 0 0 3px; }}

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
    padding: 0 7px;
    color: @app_muted;
    background: @app_bg;
    border-left: 1px solid @app_border;
}}

.status-detail {{
    min-height: 21px;
    padding: 0 7px;
    color: @app_muted;
    background: @app_bg;
    border-top: 1px solid @app_border;
    font-size: 10px;
}}

.status-ready {{ color: @app_success; }}
.status-running {{ color: @app_accent_hover; }}
.status-error {{ color: @app_danger; }}

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
    min-height: 26px;
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

.kernel-status {{
    font-size: 10px;
    padding: 0 3px;
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
