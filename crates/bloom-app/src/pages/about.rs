use egui::Ui;
use egui_twoui::prelude::*;

use crate::BloomApp;

impl BloomApp {
    pub fn render_about_page(&mut self, ui: &mut Ui) {
        let (_update_clicked, link_idx) =
            AppInfoView::new("Project Bloom", env!("CARGO_PKG_VERSION"))
                .update_state(AppUpdateState::LatestInstalled)
                .links(&["Open Source", "GitHub"])
                .show(ui);

        if let Some(idx) = link_idx {
            if idx == 0 {
                self.licenses_sheet_open = true;
            } else {
                ui.ctx().open_url(egui::OpenUrl::new_tab(
                    "https://github.com/nightofnoreturn/Project-Bloom",
                ));
            }
        }
    }
}
