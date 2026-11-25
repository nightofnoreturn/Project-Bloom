use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bloom_core::mock_device::{
    default_mock_firmware_dir, generate_dummy_firmware, mock_device_controller,
    MockDeviceMode, MockDevicePreset, MockDummyFirmwareKind, MockFaultPreset,
};
use bloom_core::{OdinFirmwareSlots, OdinSlotKind};
use egui::{
    vec2, Align, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, RichText,
    ScrollArea, Stroke, TextEdit, Ui, ViewportBuilder, ViewportId,
};
use egui_twoui::prelude::*;

use crate::state::{format_bytes, open_folder_in_os, BloomPage};
use crate::tasks::spawn_verify_odin_slot_md5;
use crate::widgets::labeled_text_field;
use crate::BloomApp;

enum DummyLoadRequest {
    None,
    LoadFolder(PathBuf),
    LoadSingle {
        slot: OdinSlotKind,
        file: PathBuf,
        verify_md5: bool,
    },
}

impl BloomApp {
    pub fn render_mock_device_window(&mut self, ctx: &egui::Context) {
        if !self.mock_window_open {
            return;
        }

        let is_flashing_in_bloom = self
            .shared
            .lock()
            .map(|g| g.flash.flashing)
            .unwrap_or(false);
        let ctrl = mock_device_controller();
        let snap = ctrl.snapshot();

        let builder = ViewportBuilder::default()
            .with_title("Virtual Galaxy — Mock Device")
            .with_decorations(false)
            .with_inner_size([480.0, 780.0])
            .with_min_inner_size([400.0, 520.0]);

        let mut close_requested = false;
        let mut load_request = DummyLoadRequest::None;
        let mut new_dummy_status = self.mock_dummy_status.clone();
        let verify_md5_enabled = self.flash_options.verify_md5;

        ctx.show_viewport_immediate(
            ViewportId::from_hash_of("bloom_mock_device_win"),
            builder,
            |ui, _class| {
                let theme = theme_from_ui(ui);
                let p = theme.palette.clone();
                apply_borderless_window_resize(ui.ctx(), &p);

                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    close_requested = true;
                }
                if is_flashing_in_bloom || snap.has_pending_transition {
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                }

                let subtitle = if !snap.enabled {
                    "Mock disabled (real USB)".to_owned()
                } else {
                    format!("{} • {}", snap.identity.model, snap.mode.label())
                };

                egui::CentralPanel::default()
                    .frame(Frame::NONE.fill(p.round_and_bg))
                    .show(ui, |ui| {
                        TwoUiScaffold::new("bloom_mock_device_scaffold", "Virtual Galaxy")
                            .subtitle(Some(subtitle.as_str()))
                            .nav_button(ScaffoldNavButton::None)
                            .window_controls(true)
                            .show(
                                ui,
                                |toolbar_ui| {
                                    if toolbar_ui
                                        .add(
                                            TwoUiIconButton::new(TwoUiIcon::Refresh)
                                                .size(34.0)
                                                .icon_size(16.0)
                                                .tint(p.text_primary)
                                                .tooltip("Reset telemetry & errors"),
                                        )
                                        .clicked()
                                    {
                                        ctrl.reset_session_telemetry();
                                    }
                                },
                                |ui| {
                                    render_mock_screen_card(ui, &snap, &p);

                                    SectionHeader::new("USB & Mode").show(ui);
                                    CardSection::new()
                                        .inner_margin(Margin::symmetric(0, 4))
                                        .show(ui, |ui| {
                                            let mut enabled_flag = snap.enabled;
                                            SwitchItem::new(
                                                &mut enabled_flag,
                                                "Mock device active",
                                            )
                                            .summary(
                                                "Intercepts Odin & ADB detection with this virtual phone",
                                            )
                                            .show(ui);
                                            if enabled_flag != snap.enabled {
                                                ctrl.set_enabled(enabled_flag);
                                            }

                                            paint_divider(ui, DividerInset::FullWidth);
                                            Frame::NONE
                                                .inner_margin(Margin::symmetric(16, 12))
                                                .show(ui, |ui| {
                                                    ui.spacing_mut().item_spacing.y = 10.0;
                                                    ui.horizontal_wrapped(|ui| {
                                                        ui.spacing_mut().item_spacing =
                                                            vec2(6.0, 6.0);
                                                        let modes = [
                                                            ("Download", MockDeviceMode::DownloadMode),
                                                            ("ADB", MockDeviceMode::NormalAdb),
                                                            ("Maint.", MockDeviceMode::MaintenanceMode),
                                                            ("Charging", MockDeviceMode::NormalChargingOnly),
                                                        ];
                                                        for (lbl, m) in modes {
                                                            let active = snap.mode == m;
                                                            if ui
                                                                .add(
                                                                    TwoUiButton::new(lbl)
                                                                        .style(if active {
                                                                            TwoUiButtonStyle::Primary
                                                                        } else {
                                                                            TwoUiButtonStyle::Default
                                                                        })
                                                                        .min_size(vec2(82.0, 34.0)),
                                                                )
                                                                .clicked()
                                                            {
                                                                ctrl.set_mode(m);
                                                            }
                                                        }
                                                    });

                                                    let is_disconnected =
                                                        snap.mode == MockDeviceMode::Disconnected;
                                                    if is_disconnected {
                                                        if ui
                                                            .add(
                                                                TwoUiButton::contained_primary(
                                                                    "Plug In USB (Download Mode)",
                                                                )
                                                                .icon(TwoUiIcon::Device)
                                                                .fill_width(true)
                                                                .min_size(vec2(180.0, 40.0)),
                                                            )
                                                            .clicked()
                                                        {
                                                            ctrl.set_mode(
                                                                MockDeviceMode::DownloadMode,
                                                            );
                                                        }
                                                    } else if ui
                                                        .add(
                                                            TwoUiButton::outline(
                                                                "Unplug USB Cable (Live)",
                                                            )
                                                            .icon(TwoUiIcon::DeviceMobileDisconnect)
                                                            .fill_width(true)
                                                            .min_size(vec2(180.0, 40.0)),
                                                        )
                                                        .clicked()
                                                    {
                                                        ctrl.set_mode(MockDeviceMode::Disconnected);
                                                    }
                                                });
                                        });

                                    SectionHeader::new("Fault Injector & Speed").show(ui);
                                    CardSection::new()
                                        .inner_margin(Margin::symmetric(0, 4))
                                        .show(ui, |ui| {
                                            Frame::NONE
                                                .inner_margin(Margin::symmetric(16, 12))
                                                .show(ui, |ui| {
                                                    ui.spacing_mut().item_spacing.y = 10.0;
                                                    let fault_labels: Vec<&'static str> =
                                                        MockFaultPreset::ALL
                                                            .iter()
                                                            .map(|f| f.label())
                                                            .collect();
                                                    let mut fault_idx = MockFaultPreset::ALL
                                                        .iter()
                                                        .position(|&f| f == snap.fault)
                                                        .unwrap_or(0);
                                                    ui.label(
                                                        RichText::new("Fault scenario")
                                                            .font(font_id(
                                                                ui,
                                                                13.0,
                                                                TwoUiFontWeight::Medium,
                                                            ))
                                                            .color(p.text_muted),
                                                    );
                                                    if TwoUiDropdown::new(
                                                        "mock_fault_dropdown",
                                                        &fault_labels,
                                                        &mut fault_idx,
                                                    )
                                                    .min_width(ui.available_width().max(200.0))
                                                    .show(ui)
                                                    .changed()
                                                    {
                                                        if let Some(&picked) =
                                                            MockFaultPreset::ALL.get(fault_idx)
                                                        {
                                                            ctrl.set_fault(picked);
                                                        }
                                                    }

                                                    if snap.fault.uses_percent() {
                                                        let mut pct_f = f32::from(snap.fail_at_percent);
                                                        if ui
                                                            .add(
                                                                TwoUiSeekBarPro::new(
                                                                    "Fail at progress",
                                                                    &mut pct_f,
                                                                    5.0..=95.0,
                                                                )
                                                                .step(5.0)
                                                                .unit("%")
                                                                .mode(SeekBarMode::Expand)
                                                                .end_labels("5%", "95%"),
                                                            )
                                                            .changed()
                                                        {
                                                            ctrl.set_fail_at_percent(
                                                                pct_f.round() as u8
                                                            );
                                                        }
                                                    }

                                                    let mut speed_f =
                                                        snap.transfer_speed_mbps.clamp(1.0, 50.0);
                                                    if ui
                                                        .add(
                                                            TwoUiSeekBarPro::new(
                                                                "USB speed",
                                                                &mut speed_f,
                                                                1.0..=50.0,
                                                            )
                                                            .step(1.0)
                                                            .unit(" MiB/s")
                                                            .mode(SeekBarMode::Expand)
                                                            .end_labels("1 MiB/s", "50 MiB/s"),
                                                        )
                                                        .changed()
                                                    {
                                                        ctrl.set_transfer_speed_mbps(
                                                            speed_f.round(),
                                                        );
                                                    }
                                                });
                                        });

                                    SectionHeader::new("Device Identity (DVIF & Super)").show(ui);
                                    CardSection::new()
                                        .inner_margin(Margin::symmetric(0, 4))
                                        .show(ui, |ui| {
                                            Frame::NONE
                                                .inner_margin(Margin::symmetric(16, 12))
                                                .show(ui, |ui| {
                                                    ui.spacing_mut().item_spacing.y = 10.0;
                                                    let preset_labels: Vec<&'static str> =
                                                        MockDevicePreset::ALL
                                                            .iter()
                                                            .map(|pr| pr.label)
                                                            .collect();
                                                    let mut preset_idx = snap.preset_idx;
                                                    ui.horizontal_wrapped(|ui| {
                                                        ui.label(
                                                            RichText::new("Preset")
                                                                .font(font_id(
                                                                    ui,
                                                                    14.0,
                                                                    TwoUiFontWeight::Medium,
                                                                ))
                                                                .color(p.text_primary),
                                                        );
                                                        ui.with_layout(
                                                            Layout::right_to_left(Align::Center),
                                                            |ui| {
                                                                if TwoUiDropdown::new(
                                                                    "mock_preset_dropdown",
                                                                    &preset_labels,
                                                                    &mut preset_idx,
                                                                )
                                                                .min_width(210.0)
                                                                .show(ui)
                                                                .changed()
                                                                {
                                                                    ctrl.apply_preset(preset_idx);
                                                                }
                                                            },
                                                        );
                                                    });

                                                    let mut model = snap.identity.model.clone();
                                                    let mut csc = snap.identity.sales_code.clone();
                                                    let mut bl =
                                                        snap.identity.bootloader_version.clone();
                                                    ui.columns(2, |cols| {
                                                        if labeled_text_field(
                                                            &mut cols[0],
                                                            "Model",
                                                            &mut model,
                                                            "SM-S921B",
                                                            None,
                                                            true,
                                                        )
                                                        .changed()
                                                        {
                                                            model.make_ascii_uppercase();
                                                            ctrl.update_identity(|id| {
                                                                id.model = model;
                                                            });
                                                        }
                                                        if labeled_text_field(
                                                            &mut cols[1],
                                                            "CSC",
                                                            &mut csc,
                                                            "XID",
                                                            None,
                                                            true,
                                                        )
                                                        .changed()
                                                        {
                                                            csc.make_ascii_uppercase();
                                                            ctrl.update_identity(|id| {
                                                                id.sales_code = csc;
                                                            });
                                                        }
                                                    });

                                                    if labeled_text_field(
                                                        ui,
                                                        "Bootloader / Binary Version",
                                                        &mut bl,
                                                        "S921BXXU4AYB7",
                                                        None,
                                                        true,
                                                    )
                                                    .changed()
                                                    {
                                                        bl.make_ascii_uppercase();
                                                        ctrl.update_identity(|id| {
                                                            id.bootloader_version = bl;
                                                        });
                                                    }

                                                    let mut temp_f =
                                                        snap.identity.temperature_c as f32;
                                                    if ui
                                                        .add(
                                                            TwoUiSeekBarPro::new(
                                                                "Temperature",
                                                                &mut temp_f,
                                                                20.0..=75.0,
                                                            )
                                                            .step(5.0)
                                                            .unit(" °C")
                                                            .mode(SeekBarMode::Expand)
                                                            .end_labels("20°C", "75°C"),
                                                        )
                                                        .changed()
                                                    {
                                                        ctrl.update_identity(|id| {
                                                            id.temperature_c = temp_f.round() as i32;
                                                        });
                                                    }

                                                    let mut super_f =
                                                        snap.identity.super_free_mb as f32;
                                                    if ui
                                                        .add(
                                                            TwoUiSeekBarPro::new(
                                                                "Super free space (Dummy AP uses 4096 MiB)",
                                                                &mut super_f,
                                                                512.0..=8192.0,
                                                            )
                                                            .step(512.0)
                                                            .unit(" MiB")
                                                            .mode(SeekBarMode::Expand)
                                                            .end_labels("512 MiB", "8192 MiB"),
                                                        )
                                                        .changed()
                                                    {
                                                        ctrl.update_identity(|id| {
                                                            id.super_free_mb =
                                                                super_f.round() as u32;
                                                        });
                                                    }
                                                });

                                            let mut supports_dvif = snap.identity.supports_dvif;
                                            SwitchItem::new(
                                                &mut supports_dvif,
                                                "Respond to DVIF diagnostics",
                                            )
                                            .summary("Disable to simulate legacy bootloader without preflight info")
                                            .top_divider(true)
                                            .show(ui);
                                            if supports_dvif != snap.identity.supports_dvif {
                                                ctrl.update_identity(|id| {
                                                    id.supports_dvif = supports_dvif;
                                                });
                                            }

                                            let mut auto_reboot_tr = snap.auto_reboot_transition;
                                            SwitchItem::new(
                                                &mut auto_reboot_tr,
                                                "Auto-reboot back to ADB after flash",
                                            )
                                            .summary("Virtual phone reboots into Android 1.5s after PASS")
                                            .top_divider(true)
                                            .show(ui);
                                            if auto_reboot_tr != snap.auto_reboot_transition {
                                                ctrl.set_auto_reboot_transition(auto_reboot_tr);
                                            }
                                        });

                                    SectionHeader::new("Dummy Firmware Generator").show(ui);
                                    CardSection::new()
                                        .inner_margin(Margin::symmetric(16, 12))
                                        .show(ui, |ui| {
                                            ui.spacing_mut().item_spacing.y = 8.0;
                                            ui.horizontal_wrapped(|ui| {
                                                ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                                                let buttons = [
                                                    ("Valid Full FW", MockDummyFirmwareKind::ValidFull),
                                                    ("Corrupt MD5", MockDummyFirmwareKind::CorruptMd5),
                                                    ("Corrupt TAR", MockDummyFirmwareKind::CorruptTar),
                                                    (
                                                        "Wrong Model / B1",
                                                        MockDummyFirmwareKind::WrongModelLowerBinary,
                                                    ),
                                                ];
                                                for (lbl, kind) in buttons {
                                                    if ui
                                                        .add(
                                                            TwoUiButton::outline(lbl)
                                                                .min_size(vec2(140.0, 36.0)),
                                                        )
                                                        .clicked()
                                                    {
                                                        match generate_dummy_firmware(
                                                            &default_mock_firmware_dir(),
                                                            &snap.identity,
                                                            kind,
                                                        ) {
                                                            Ok(res) => {
                                                                new_dummy_status = Some((
                                                                    res.description,
                                                                    res.folder,
                                                                    res.primary_file,
                                                                    kind,
                                                                ));
                                                            }
                                                            Err(err) => {
                                                                new_dummy_status = Some((
                                                                    format!("Failed: {err}"),
                                                                    default_mock_firmware_dir(),
                                                                    None,
                                                                    kind,
                                                                ));
                                                            }
                                                        }
                                                    }
                                                }
                                            });

                                            if let Some((desc, folder, primary_file, kind)) =
                                                new_dummy_status.as_ref()
                                            {
                                                ui.label(
                                                    RichText::new(desc)
                                                        .font(font_id(
                                                            ui,
                                                            12.5,
                                                            TwoUiFontWeight::Medium,
                                                        ))
                                                        .color(p.text_primary),
                                                );
                                                ui.label(
                                                    RichText::new(folder.display().to_string())
                                                        .font(FontId::new(
                                                            11.0,
                                                            FontFamily::Monospace,
                                                        ))
                                                        .color(p.text_muted),
                                                );
                                                ui.horizontal(|ui| {
                                                    if ui
                                                        .add(
                                                            TwoUiButton::contained_primary(
                                                                "Load in Bloom",
                                                            )
                                                            .icon(TwoUiIcon::Lightning)
                                                            .min_size(vec2(140.0, 36.0)),
                                                        )
                                                        .clicked()
                                                    {
                                                        load_request = match kind {
                                                            MockDummyFirmwareKind::ValidFull => {
                                                                DummyLoadRequest::LoadFolder(
                                                                    folder.clone(),
                                                                )
                                                            }
                                                            MockDummyFirmwareKind::CorruptMd5 => {
                                                                if let Some(f) = primary_file {
                                                                    DummyLoadRequest::LoadSingle {
                                                                        slot: OdinSlotKind::Bl,
                                                                        file: f.clone(),
                                                                        verify_md5: verify_md5_enabled,
                                                                    }
                                                                } else {
                                                                    DummyLoadRequest::None
                                                                }
                                                            }
                                                            MockDummyFirmwareKind::CorruptTar => {
                                                                if let Some(f) = primary_file {
                                                                    DummyLoadRequest::LoadSingle {
                                                                        slot: OdinSlotKind::Ap,
                                                                        file: f.clone(),
                                                                        verify_md5: false,
                                                                    }
                                                                } else {
                                                                    DummyLoadRequest::None
                                                                }
                                                            }
                                                            MockDummyFirmwareKind::WrongModelLowerBinary => {
                                                                if let Some(f) = primary_file {
                                                                    DummyLoadRequest::LoadSingle {
                                                                        slot: OdinSlotKind::Ap,
                                                                        file: f.clone(),
                                                                        verify_md5: verify_md5_enabled,
                                                                    }
                                                                } else {
                                                                    DummyLoadRequest::None
                                                                }
                                                            }
                                                        };
                                                    }
                                                    if ui
                                                        .add(
                                                            TwoUiButton::outline("Open Folder")
                                                                .icon(TwoUiIcon::FolderOutline)
                                                                .min_size(vec2(126.0, 36.0)),
                                                        )
                                                        .clicked()
                                                    {
                                                        let _ = open_folder_in_os(folder);
                                                    }
                                                });
                                            }
                                        });

                                    SectionHeader::new("Odin Wire Packet Log").show(ui);
                                    CardSection::new()
                                        .inner_margin(Margin::symmetric(14, 10))
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    RichText::new("Bootloader RX/TX trace")
                                                        .font(font_id(
                                                            ui,
                                                            12.5,
                                                            TwoUiFontWeight::Medium,
                                                        ))
                                                        .color(p.text_muted),
                                                );
                                                ui.with_layout(
                                                    Layout::right_to_left(Align::Center),
                                                    |ui| {
                                                        if ui
                                                            .add(
                                                                TwoUiButton::primary("Clear")
                                                                    .icon(TwoUiIcon::Delete)
                                                                    .min_size(vec2(86.0, 30.0)),
                                                            )
                                                            .clicked()
                                                        {
                                                            ctrl.clear_packet_log();
                                                        }
                                                    },
                                                );
                                            });
                                            ui.add_space(6.0);
                                            Frame::NONE
                                                .fill(p.round_and_bg)
                                                .stroke(Stroke::new(1.0, p.divider))
                                                .inner_margin(Margin::symmetric(8, 6))
                                                .show(ui, |ui| {
                                                    ui.set_width(ui.available_width());
                                                    ScrollArea::vertical()
                                                        .id_salt("bloom_mock_wire_log_scroll")
                                                        .max_height(150.0)
                                                        .stick_to_bottom(true)
                                                        .show(ui, |ui| {
                                                            let log_txt = if snap.packet_log.is_empty() {
                                                                "Waiting for Odin / ADB traffic..."
                                                                    .to_owned()
                                                            } else {
                                                                snap.packet_log.join("\n")
                                                            };
                                                            let mut view: &str = &log_txt;
                                                            ui.add(
                                                                TextEdit::multiline(&mut view)
                                                                    .frame(Frame::NONE)
                                                                    .margin(Margin::ZERO)
                                                                    .desired_width(ui.available_width())
                                                                    .font(FontId::new(
                                                                        11.5,
                                                                        FontFamily::Monospace,
                                                                    ))
                                                                    .text_color(p.text_secondary),
                                                            );
                                                        });
                                                });
                                        });

                                    ui.add_space(6.0);
                                    ui.label(
                                        RichText::new("Tip: Press Ctrl+Shift+M in Bloom to reopen this window if closed.")
                                            .font(font_id(ui, 11.5, TwoUiFontWeight::Regular))
                                            .color(p.text_muted),
                                    );
                                    ui.add_space(16.0);
                                },
                            );
                    });
            },
        );

        self.mock_dummy_status = new_dummy_status;
        if close_requested {
            self.mock_window_open = false;
        }

        if is_flashing_in_bloom {
            if !matches!(load_request, DummyLoadRequest::None) {
                self.toast
                    .show_message(ctx, "Cannot load firmware while flashing", 2.4);
            }
        } else {
            match load_request {
                DummyLoadRequest::None => {}
                DummyLoadRequest::LoadFolder(folder) => {
                    self.flash_entry_accepted = true;
                    self.page_id = BloomPage::Flash.id();
                    match OdinFirmwareSlots::scan_firmware_folder(&folder) {
                        Ok(scanned) => {
                            if scanned.can_toggle_csc_mode() {
                                self.flash_pending_folder_slots = Some(scanned);
                                self.flash_csc_choice_dialog_open = true;
                            } else {
                                self.apply_scanned_flash_slots(ctx, scanned);
                            }
                        }
                        Err(err) => {
                            self.toast.show_message(ctx, format!("{err}"), 2.4);
                        }
                    }
                    ctx.request_repaint();
                }
                DummyLoadRequest::LoadSingle {
                    slot,
                    file,
                    verify_md5,
                } => {
                    self.flash_entry_accepted = true;
                    self.page_id = BloomPage::Flash.id();
                    self.flash_slots = OdinFirmwareSlots::default();
                    if verify_md5 {
                        spawn_verify_odin_slot_md5(
                            &self.rt,
                            Arc::clone(&self.shared),
                            ctx.clone(),
                            slot,
                            file,
                        );
                    } else {
                        self.flash_slots.set_slot(slot, Some(file.clone()));
                        if let Ok(mut g) = self.shared.lock() {
                            let fname = file
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("package");
                            g.flash.logs.push(format!("Added {}: {fname}", slot.label()));
                        }
                    }
                    ctx.request_repaint();
                }
            }
        }
    }
}

fn render_mock_screen_card(
    ui: &mut Ui,
    snap: &bloom_core::mock_device::MockDeviceSnapshot,
    p: &TwoUiPalette,
) {
    let screen_bg = Color32::from_rgb(0x10, 0x16, 0x22);
    let accent = match snap.mode {
        MockDeviceMode::DownloadMode => Color32::from_rgb(0x38, 0x9E, 0xFF),
        MockDeviceMode::NormalAdb | MockDeviceMode::MaintenanceMode => p.functional_green,
        MockDeviceMode::NormalChargingOnly => Color32::from_rgb(0xE8, 0xA8, 0x38),
        MockDeviceMode::Disconnected => p.functional_red,
    };

    Frame::NONE
        .fill(screen_bg)
        .corner_radius(CornerRadius::same(18))
        .stroke(Stroke::new(1.2, accent.gamma_multiply(0.55)))
        .inner_margin(Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;

            ui.horizontal(|ui| {
                let badge_txt = match snap.mode {
                    MockDeviceMode::DownloadMode => "ODIN MODE",
                    MockDeviceMode::NormalAdb => "ANDROID (ADB)",
                    MockDeviceMode::MaintenanceMode => "MAINTENANCE MODE",
                    MockDeviceMode::NormalChargingOnly => "USB CHARGING",
                    MockDeviceMode::Disconnected => "DISCONNECTED",
                };
                Frame::NONE
                    .fill(accent.gamma_multiply(0.22))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(8, 3))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(badge_txt)
                                .font(FontId::new(11.0, FontFamily::Monospace))
                                .color(accent),
                        );
                    });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} | {} | {}°C",
                            snap.identity.model,
                            snap.identity.sales_code,
                            snap.identity.temperature_c
                        ))
                        .font(FontId::new(11.5, FontFamily::Monospace))
                        .color(Color32::from_rgb(0xB8, 0xC4, 0xD8)),
                    );
                });
            });

            ui.label(
                RichText::new(format!(
                    "BL: {}  •  Super free: {} MiB",
                    snap.identity.bootloader_version, snap.identity.super_free_mb
                ))
                .font(FontId::new(11.5, FontFamily::Monospace))
                .color(Color32::from_rgb(0x8D, 0x9C, 0xB3)),
            );

            ui.label(
                RichText::new(&snap.screen_status)
                    .font(font_id(ui, 13.5, TwoUiFontWeight::SemiBold))
                    .color(Color32::WHITE),
            );

            if let Some(ref banner) = snap.last_error_banner {
                Frame::NONE
                    .fill(p.functional_red.gamma_multiply(0.25))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(8, 5))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (icon_rect, _) =
                                ui.allocate_exact_size(vec2(14.0, 14.0), egui::Sense::hover());
                            TwoUiIcon::ErrorFilled.paint(ui, icon_rect, p.functional_red);
                            ui.label(
                                RichText::new(banner)
                                    .font(FontId::new(11.5, FontFamily::Monospace))
                                    .color(Color32::from_rgb(0xFF, 0x98, 0x98)),
                            );
                        });
                    });
            }

            let frac = if snap.session_total_bytes > 0 {
                (snap.session_received_bytes as f32 / snap.session_total_bytes as f32)
                    .clamp(0.0, 1.0)
            } else {
                0.0
            };
            ui.add(
                TwoUiProgressBar::determinate(frac)
                    .height(6.0)
                    .gradient(snap.active_partition.is_some()),
            );

            ui.horizontal(|ui| {
                let left = if let Some(ref part) = snap.active_partition {
                    format!("Writing {part}")
                } else if !snap.flashed_partitions.is_empty() {
                    format!("Committed: {}", snap.flashed_partitions.join(", "))
                } else {
                    "Idle — ready for Odin session".to_owned()
                };
                ui.label(
                    RichText::new(left)
                        .font(FontId::new(11.0, FontFamily::Monospace))
                        .color(Color32::from_rgb(0x9C, 0xAA, 0xBF)),
                );
                if snap.session_total_bytes > 0 {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{:.0}% ({} / {})",
                                frac * 100.0,
                                format_bytes(snap.session_received_bytes),
                                format_bytes(snap.session_total_bytes)
                            ))
                            .font(FontId::new(11.0, FontFamily::Monospace))
                            .color(Color32::from_rgb(0xC5, 0xD2, 0xE6)),
                        );
                    });
                }
            });
        });
}
