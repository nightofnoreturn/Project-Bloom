use std::sync::Arc;
use std::time::{Duration, Instant};

use bloom_core::{generate_adb_qr_session, AdbMdnsService, AdbTransport};
use egui::{Margin, RichText, Ui};
use egui_twoui::prelude::*;

use crate::tasks::{
    spawn_connect_wireless_device, spawn_pair_wireless_device, spawn_scan_mdns_services,
};
use crate::widgets::{labeled_text_field, pill_tag, status_banner};
use crate::BloomApp;

impl BloomApp {
    pub fn reset_pair_qr_session(&mut self) {
        let (svc, pwd, payload) = generate_adb_qr_session();
        self.pair_qr_service = svc;
        self.pair_qr_password = pwd;
        self.pair_qr_payload = payload;
        self.pair_qr_triggered_addr = None;
        if let Ok(mut guard) = self.shared.lock() {
            guard.wireless_pair.last_error = None;
            guard.wireless_pair.last_success = None;
        }
    }

    pub fn render_pair_device_page(&mut self, ui: &mut Ui) {
        if self.last_mdns_scan.elapsed() >= Duration::from_millis(1500) {
            self.last_mdns_scan = Instant::now();
            spawn_scan_mdns_services(&self.rt, Arc::clone(&self.shared), ui.ctx().clone());
        }
        ui.ctx()
            .request_repaint_after(Duration::from_millis(1500));

        let (wireless_pair, connected_devices) = self
            .shared
            .lock()
            .map(|g| (g.wireless_pair.clone(), g.connected_devices.clone()))
            .unwrap_or_default();

        if wireless_pair.pairing || wireless_pair.connecting {
            ui.ctx().request_repaint();
        }

        // Auto-trigger ADB TLS pairing as soon as the phone scans our QR code and advertises mDNS.
        if !wireless_pair.pairing {
            if let Some(qr_match) = wireless_pair.mdns_services.iter().find(|svc| {
                svc.service_type == "_adb-tls-pairing._tcp"
                    && svc.instance.eq_ignore_ascii_case(&self.pair_qr_service)
            }) {
                if self.pair_qr_triggered_addr.as_deref() != Some(qr_match.address.as_str()) {
                    self.pair_qr_triggered_addr = Some(qr_match.address.clone());
                    spawn_pair_wireless_device(
                        &self.rt,
                        Arc::clone(&self.shared),
                        ui.ctx().clone(),
                        qr_match.address.clone(),
                        self.pair_qr_password.clone(),
                    );
                }
            }
        }

        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        SubTabBar::new(
            "bloom_pair_tabs",
            &["QR code", "Pairing code"],
            &mut self.pair_tab_idx,
        )
        .show(ui);
        ui.add_space(14.0);

        if self.pair_tab_idx == 0 {
            self.render_qr_pair_tab(ui, &wireless_pair);
        } else {
            self.render_otp_pair_tab(ui, &wireless_pair);
        }

        // Show already-paired Wi-Fi endpoints only when one is actively discovered & not yet attached.
        let available_connect: Vec<&AdbMdnsService> = wireless_pair
            .mdns_services
            .iter()
            .filter(|svc| {
                svc.service_type == "_adb-tls-connect._tcp"
                    && !connected_devices.iter().any(|d| {
                        d.is_adb_ready()
                            && d.transport == AdbTransport::Wifi
                            && d.device_id.contains(&svc.instance)
                    })
            })
            .collect();

        if !available_connect.is_empty() {
            SectionHeader::new("Available on Wi-Fi").show(ui);
            let mut connect_addr: Option<String> = None;
            CardSection::new()
                .inner_margin(Margin::symmetric(0, 6))
                .show(ui, |ui| {
                    for (idx, svc) in available_connect.iter().enumerate() {
                        let badge = if wireless_pair.connecting {
                            "Connecting"
                        } else {
                            "Connect"
                        };
                        let resp = ui.add(
                            CardItem::new(&svc.address)
                                .icon(TwoUiIcon::Wifi)
                                .summary(&svc.instance)
                                .badge(badge)
                                .top_divider(idx > 0)
                                .enabled(!wireless_pair.connecting),
                        );
                        if resp.clicked() && !wireless_pair.connecting {
                            connect_addr = Some(svc.address.clone());
                        }
                    }
                });
            if let Some(addr) = connect_addr {
                spawn_connect_wireless_device(&self.rt, Arc::clone(&self.shared), ui.ctx().clone(), addr);
            }
        }

        if let Some(ref err) = wireless_pair.last_error {
            ui.add_space(12.0);
            status_banner(ui, TwoUiIcon::Info, p.functional_red, "Pairing failed", Some(err));
        } else if let Some(ref msg) = wireless_pair.last_success {
            ui.add_space(12.0);
            status_banner(ui, TwoUiIcon::Check, p.functional_green, "Paired", Some(msg));
        }
    }

    fn render_qr_pair_tab(
        &mut self,
        ui: &mut Ui,
        wireless_pair: &crate::state::WirelessPairState,
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        CardSection::new()
            .inner_margin(Margin::symmetric(24, 24))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.spacing_mut().item_spacing.y = 14.0;
                    ui.add(
                        TwoUiQrCode::new(&self.pair_qr_payload)
                            .size(216.0)
                            .icon(TwoUiIcon::Wifi),
                    );

                    if wireless_pair.pairing {
                        ui.add(TwoUiSpinner::indeterminate(SpinnerSize::Small));
                        ui.label(
                            RichText::new("Pairing...")
                                .font(font_id(ui, 14.0, TwoUiFontWeight::Medium))
                                .color(p.primary_high),
                        );
                    } else if wireless_pair.last_success.is_some() {
                        ui.label(
                            RichText::new("Paired")
                                .font(font_id(ui, 14.5, TwoUiFontWeight::SemiBold))
                                .color(p.functional_green),
                        );
                    } else {
                        ui.label(
                            RichText::new("Waiting for scan")
                                .font(font_id(ui, 14.0, TwoUiFontWeight::Medium))
                                .color(p.text_secondary),
                        );
                    }

                    ui.label(
                        RichText::new("Developer options → Wireless debugging → Pair device with QR code")
                            .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                            .color(p.text_muted),
                    );
                });
            });

        ui.add_space(12.0);
        let mut refresh_qr = false;
        ui.vertical_centered(|ui| {
            if ui
                .add(TwoUiButton::outline("New QR").icon(TwoUiIcon::Refresh))
                .clicked()
            {
                refresh_qr = true;
            }
        });
        if refresh_qr {
            self.reset_pair_qr_session();
        }
    }

    fn render_otp_pair_tab(
        &mut self,
        ui: &mut Ui,
        wireless_pair: &crate::state::WirelessPairState,
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        let detected_pairing = wireless_pair
            .mdns_services
            .iter()
            .find(|svc| {
                svc.service_type == "_adb-tls-pairing._tcp"
                    && !svc.instance.eq_ignore_ascii_case(&self.pair_qr_service)
            })
            .map(|svc| svc.address.clone());

        if let Some(ref addr) = detected_pairing {
            if self.pair_otp_address.trim().is_empty()
                || self.pair_otp_address == self.pair_otp_last_autofill
            {
                self.pair_otp_address = addr.clone();
                self.pair_otp_last_autofill = addr.clone();
            }
        }

        CardSection::new()
            .inner_margin(Margin::symmetric(20, 18))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 14.0;

                if let Some(ref addr) = detected_pairing {
                    ui.horizontal(|ui| {
                        pill_tag(
                            ui,
                            &format!("Detected {addr}"),
                            p.selected_bg,
                            p.primary_high,
                        );
                    });
                }

                labeled_text_field(
                    ui,
                    "IP address & port",
                    &mut self.pair_otp_address,
                    "192.168.1.4:45699",
                    None,
                    !wireless_pair.pairing,
                );

                labeled_text_field(
                    ui,
                    "Pairing code",
                    &mut self.pair_otp_code,
                    "6-digit code",
                    None,
                    !wireless_pair.pairing,
                );

                ui.label(
                    RichText::new("Developer options → Wireless debugging → Pair device with pairing code")
                        .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                        .color(p.text_muted),
                );

                if ui
                    .add(
                        TwoUiButton::contained_primary("Pair")
                            .icon(TwoUiIcon::Wifi)
                            .loading(wireless_pair.pairing)
                            .fill_width(true),
                    )
                    .clicked()
                    && !wireless_pair.pairing
                {
                    let addr = self.pair_otp_address.clone();
                    let code = self.pair_otp_code.clone();
                    spawn_pair_wireless_device(
                        &self.rt,
                        Arc::clone(&self.shared),
                        ui.ctx().clone(),
                        addr,
                        code,
                    );
                }
            });
    }
}
