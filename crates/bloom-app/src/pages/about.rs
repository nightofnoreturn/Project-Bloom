use egui::{Margin, Ui};
use egui_oneui::prelude::*;

use crate::widgets::key_value_row;
use crate::BloomApp;

impl BloomApp {
    pub fn render_about_page(&mut self, ui: &mut Ui) {
        let (_update_clicked, link_idx) = AppInfoView::new("Project Bloom", env!("CARGO_PKG_VERSION"))
            .update_state(AppUpdateState::LatestInstalled)
            .links(&["Open GitHub Repository"])
            .show(ui);

        if link_idx == Some(0) {
            ui.ctx().open_url(egui::OpenUrl::new_tab(
                "https://github.com/nightofnoreturn/Project-Bloom",
            ));
        }

        SectionHeader::new("Engine & Toolkit").show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                key_value_row(
                    ui,
                    "FUS Protocol",
                    "Modern + Legacy Auth + SmartHistory",
                    false,
                );
                paint_divider(ui, DividerInset::Standard);
                key_value_row(
                    ui,
                    "Resumable Transfer",
                    "Segmented HTTP Range + 401 Nonce Recovery",
                    false,
                );
                paint_divider(ui, DividerInset::Standard);
                key_value_row(
                    ui,
                    "Decryption Engine",
                    "Multi-core AES-128-ECB + CRC32/MD5",
                    false,
                );
                paint_divider(ui, DividerInset::Standard);
                key_value_row(ui, "Design System", "OneUI-Crate (Samsung One UI 9 / SESL9)", true);
            });
    }
}
