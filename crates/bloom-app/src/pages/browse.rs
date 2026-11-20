use bloom_core::{
    CatalogCategoryItem, CatalogMarketingGroup, ChangelogLang, ConnectedSamsungDevice, CscItem,
    DecodedBuildCode, FirmwareUpdateType, GalaxyDevice, GalaxyRootSeries, GalaxyTabSeries,
    HistoryItem,
};
use egui::{
    pos2, vec2, Align, Color32, CornerRadius, Frame, Id, Margin, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui,
};
use egui_twoui::feedback::{interaction_factors, paint_state_overlay, recoil_rect};
use egui_twoui::prelude::*;

use crate::state::{
    extract_emblem_text, format_firmware_tile_title, format_tile_date, short_build_tail, BloomPage,
    BrowseNavState,
};
use crate::widgets::{key_value_row, pill_tag, status_banner};
use crate::BloomApp;

const MAX_DEVICE_LIST_ROWS: usize = 120;

enum SearchResultRow {
    MarketingGroup(String),
    SpecificModel(String, GalaxyDevice),
}

fn installed_build_for_model(
    connected: &[ConnectedSamsungDevice],
    model: &str,
) -> Option<String> {
    connected
        .iter()
        .find(|d| d.model.eq_ignore_ascii_case(model))
        .and_then(|d| d.firmware_build.as_ref())
        .map(|fw| fw.split('/').next().unwrap_or(fw).trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty())
}

fn render_browse_step_header(ui: &mut Ui, title: &str, subtitle: Option<&str>) -> bool {
    let theme = theme_from_ui(ui);
    let p = &theme.palette;
    let mut back_clicked = false;

    CardSection::new()
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if ui
                    .add(
                        TwoUiIconButton::new(TwoUiIcon::Back)
                            .size(34.0)
                            .icon_size(18.0)
                            .tint(p.text_primary),
                    )
                    .clicked()
                {
                    back_clicked = true;
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(
                        RichText::new(title)
                            .font(font_id(ui, 15.5, TwoUiFontWeight::SemiBold))
                            .color(p.text_primary),
                    );
                    if let Some(sub) = subtitle.filter(|s| !s.trim().is_empty()) {
                        ui.label(
                            RichText::new(sub)
                                .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                                .color(p.text_muted),
                        );
                    }
                });
            });
        });
    ui.add_space(12.0);
    back_clicked
}

impl BloomApp {
    pub fn render_browse_floating_search_bar(&mut self, ui: &mut Ui) {
        let has_text = !self.device_search_query.is_empty();
        let is_expanded = self.browse_search_focused || has_text;
        let t = animate_toggle(
            ui,
            Id::unique("bloom_browse_search_expand_t"),
            is_expanded,
            0.24,
            ease_fast_out_slow_in,
        );
        let full_w = ui.max_rect().width().max(200.0);
        let compact_w = 148.0_f32.min(full_w);
        let cur_w = egui::lerp(compact_w..=full_w, t);

        ui.set_width(cur_w);
        ui.set_max_width(cur_w);

        let (search_resp, back_clicked) = TwoUiSearchBar::new(&mut self.device_search_query)
            .hint("Search")
            .show_back_button(is_expanded)
            .floating(true)
            .show(ui);

        let pill_rect = ui.min_rect();
        let clicked_inside_pill = !back_clicked
            && ui.input(|i| {
                i.pointer.primary_clicked()
                    && i.pointer
                        .interact_pos()
                        .is_some_and(|pos| pill_rect.contains(pos))
            });
        if clicked_inside_pill && !search_resp.has_focus() {
            search_resp.request_focus();
        }

        if search_resp.changed()
            && self.selected_my_device.is_some()
            && self.selected_my_region.is_some()
        {
            self.selected_my_device = None;
            self.selected_my_region = None;
            self.selected_grid_build = None;
            self.build_detail_sheet_open = false;
        }

        if back_clicked {
            self.device_search_query.clear();
            search_resp.surrender_focus();
            self.browse_search_focused = false;
        } else {
            self.browse_search_focused = search_resp.has_focus();
        }
    }

    pub fn navigate_browse_back(&mut self) {
        self.device_search_query.clear();
        match self.browse_nav.clone() {
            BrowseNavState::Root => {}
            BrowseNavState::TabRoot => {
                self.browse_nav = BrowseNavState::Root;
            }
            BrowseNavState::Category { root, tab_sub } => {
                if root == GalaxyRootSeries::Tab && tab_sub.is_some() {
                    self.browse_nav = BrowseNavState::TabRoot;
                } else {
                    self.browse_nav = BrowseNavState::Root;
                }
            }
            BrowseNavState::SubSeries { root, tab_sub, .. } => {
                self.browse_nav = BrowseNavState::Category { root, tab_sub };
            }
            BrowseNavState::ModelVariants {
                root,
                tab_sub,
                sub_series,
                ..
            } => {
                if let (Some(root), Some((base_family, display_title))) = (root, sub_series) {
                    self.browse_nav = BrowseNavState::SubSeries {
                        root,
                        tab_sub,
                        base_family,
                        display_title,
                    };
                } else if let Some(root) = root {
                    self.browse_nav = BrowseNavState::Category { root, tab_sub };
                } else {
                    self.browse_nav = BrowseNavState::Root;
                }
            }
            BrowseNavState::RegionPicker {
                root,
                tab_sub,
                sub_series,
                from_builds_grid,
                device,
            } => {
                if from_builds_grid && self.selected_my_region.is_some() {
                    self.selected_my_device = Some(device);
                } else if root.is_some() || sub_series.is_some() {
                    self.browse_nav = BrowseNavState::ModelVariants {
                        root,
                        tab_sub,
                        sub_series,
                        marketing_name: device.marketing_name,
                    };
                } else {
                    self.browse_nav = BrowseNavState::Root;
                }
            }
        }
    }

    pub fn render_browse_page(&mut self, ui: &mut Ui) {
        let snap = match self.shared.lock() {
            Ok(g) => (
                g.device_db_syncing,
                g.device_db_error.clone(),
                g.device_catalog.len(),
                g.device_catalog_revision,
                g.my_device_builds_loading,
                g.my_device_builds_error.clone(),
                g.my_device_builds.clone(),
                g.connected_devices.clone(),
            ),
            Err(_) => return,
        };
        let (
            db_syncing,
            db_error,
            catalog_len,
            catalog_rev,
            builds_loading,
            builds_error,
            builds_opt,
            connected_devices,
        ) = snap;

        if db_syncing || builds_loading {
            ui.ctx().request_repaint();
        }

        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        if db_syncing && catalog_len > 0 {
            ui.horizontal(|ui| {
                ui.add(TwoUiSpinner::indeterminate(SpinnerSize::Small));
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Syncing")
                        .font(font_id(ui, 13.0, TwoUiFontWeight::Medium))
                        .color(p.text_muted),
                );
            });
            ui.add_space(8.0);
        }

        if !matches!(self.browse_nav, BrowseNavState::RegionPicker { .. }) {
            if let (Some(dev), Some(region)) =
                (self.selected_my_device.clone(), self.selected_my_region.clone())
            {
                self.render_selected_device_grid(
                    ui,
                    &dev,
                    &region,
                    builds_loading,
                    builds_error.as_deref(),
                    builds_opt.as_ref(),
                    &connected_devices,
                );
                ui.add_space(64.0);
                return;
            }
        }

        // Inline Region Picker Category View (replaces the old bottom sheet)
        if let BrowseNavState::RegionPicker { ref device, .. } = self.browse_nav.clone() {
            self.render_inline_region_category(ui, device);
            ui.add_space(64.0);
            return;
        }

        let query_trimmed = self.device_search_query.trim().to_owned();

        // Empty catalog first-run states
        if catalog_len == 0 {
            if db_syncing {
                ui.add_space(44.0);
                ui.vertical_centered(|ui| {
                    ui.add(TwoUiSpinner::indeterminate(SpinnerSize::Large));
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new("Loading")
                            .font(font_id(ui, 14.5, TwoUiFontWeight::Medium))
                            .color(p.text_muted),
                    );
                });
                if query_trimmed.len() < 4 {
                    return;
                }
                ui.add_space(20.0);
            } else if let Some(ref err) = db_error {
                status_banner(ui, TwoUiIcon::Info, p.functional_red, "Sync failed", Some(err));
                ui.add_space(10.0);
                if ui.add(TwoUiButton::primary("Retry")).clicked() {
                    self.trigger_sync_galaxy_db(ui.ctx(), false);
                }
                ui.add_space(12.0);
            }
        }

        if !query_trimmed.is_empty() {
            self.render_browse_search_results(ui, &query_trimmed, catalog_rev, db_syncing);
            ui.add_space(64.0);
            return;
        }

        self.render_browse_category_hierarchy(ui);
        ui.add_space(64.0);
    }

    fn render_inline_region_category(&mut self, ui: &mut Ui, dev: &GalaxyDevice) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        if render_browse_step_header(ui, "Region", Some(&dev.display_label())) {
            self.navigate_browse_back();
            return;
        }

        let ordered = self.csc_db.search(self.device_search_query.trim());
        if ordered.is_empty() {
            CardSection::new()
                .inner_margin(Margin::symmetric(20, 18))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("No results")
                            .font(font_id(ui, 14.5, TwoUiFontWeight::Medium))
                            .color(p.text_muted),
                    );
                });
            return;
        }

        let mut chosen_region: Option<CscItem> = None;
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                for (idx, item) in ordered.iter().enumerate() {
                    let mut row = CardItem::new(item.region_label())
                        .chevron()
                        .top_divider(idx > 0);
                    if let Some(carriers) = item.carrier_summary() {
                        row = row.summary(carriers);
                    }
                    if ui.add(row).clicked() {
                        chosen_region = Some((*item).clone());
                    }
                }
            });

        if let Some(region) = chosen_region {
            self.handle_region_chosen_in_browse(dev.clone(), region, ui.ctx());
        }
    }

    fn render_browse_category_hierarchy(&mut self, ui: &mut Ui) {
        match self.browse_nav.clone() {
            BrowseNavState::Root => {
                let mut picked_root: Option<GalaxyRootSeries> = None;
                CardSection::new()
                    .inner_margin(Margin::symmetric(0, 6))
                    .show(ui, |ui| {
                        for (idx, series) in GalaxyRootSeries::ALL.iter().copied().enumerate() {
                            if ui
                                .add(
                                    CardItem::new(series.title())
                                        .chevron()
                                        .top_divider(idx > 0),
                                )
                                .clicked()
                            {
                                picked_root = Some(series);
                            }
                        }
                    });
                if let Some(root) = picked_root {
                    if root == GalaxyRootSeries::Tab {
                        self.browse_nav = BrowseNavState::TabRoot;
                    } else {
                        self.browse_nav = BrowseNavState::Category {
                            root,
                            tab_sub: None,
                        };
                    }
                }
            }
            BrowseNavState::TabRoot => {
                if render_browse_step_header(ui, GalaxyRootSeries::Tab.title(), None) {
                    self.navigate_browse_back();
                    return;
                }
                let tab_series = self
                    .shared
                    .lock()
                    .map(|g| g.device_catalog.available_tab_series())
                    .unwrap_or_default();
                let mut picked_tab: Option<GalaxyTabSeries> = None;
                CardSection::new()
                    .inner_margin(Margin::symmetric(0, 6))
                    .show(ui, |ui| {
                        for (idx, sub) in tab_series.iter().copied().enumerate() {
                            if ui
                                .add(CardItem::new(sub.title()).chevron().top_divider(idx > 0))
                                .clicked()
                            {
                                picked_tab = Some(sub);
                            }
                        }
                    });
                if let Some(sub) = picked_tab {
                    self.browse_nav = BrowseNavState::Category {
                        root: GalaxyRootSeries::Tab,
                        tab_sub: Some(sub),
                    };
                }
            }
            BrowseNavState::Category { root, tab_sub } => {
                let title = tab_sub.map(|s| s.title()).unwrap_or_else(|| root.title());
                let subtitle = tab_sub.map(|_| GalaxyRootSeries::Tab.title());
                if render_browse_step_header(ui, title, subtitle) {
                    self.navigate_browse_back();
                    return;
                }

                let items: Vec<CatalogCategoryItem> = self
                    .shared
                    .lock()
                    .map(|g| g.device_catalog.category_items(root, tab_sub))
                    .unwrap_or_default();

                let mut next_nav: Option<BrowseNavState> = None;
                CardSection::new()
                    .inner_margin(Margin::symmetric(0, 6))
                    .show(ui, |ui| {
                        for (idx, item) in items.into_iter().enumerate() {
                            match item {
                                CatalogCategoryItem::SubSeries {
                                    base_family,
                                    display_title,
                                    ..
                                } => {
                                    if ui
                                        .add(
                                            CardItem::new(&display_title)
                                                .chevron()
                                                .top_divider(idx > 0),
                                        )
                                        .clicked()
                                    {
                                        next_nav = Some(BrowseNavState::SubSeries {
                                            root,
                                            tab_sub,
                                            base_family,
                                            display_title,
                                        });
                                    }
                                }
                                CatalogCategoryItem::SingleDevice(group) => {
                                    if ui
                                        .add(
                                            CardItem::new(&group.marketing_name)
                                                .chevron()
                                                .top_divider(idx > 0),
                                        )
                                        .clicked()
                                    {
                                        next_nav = Some(BrowseNavState::ModelVariants {
                                            root: Some(root),
                                            tab_sub,
                                            sub_series: None,
                                            marketing_name: group.marketing_name,
                                        });
                                    }
                                }
                            }
                        }
                    });

                if let Some(nav) = next_nav {
                    self.browse_nav = nav;
                }
            }
            BrowseNavState::SubSeries {
                root,
                tab_sub,
                base_family,
                display_title,
            } => {
                let parent_title = tab_sub.map(|s| s.title()).unwrap_or_else(|| root.title());
                if render_browse_step_header(ui, &display_title, Some(parent_title)) {
                    self.navigate_browse_back();
                    return;
                }

                let groups: Vec<CatalogMarketingGroup> = self
                    .shared
                    .lock()
                    .map(|g| {
                        g.device_catalog
                            .category_items(root, tab_sub)
                            .into_iter()
                            .find_map(|item| match item {
                                CatalogCategoryItem::SubSeries {
                                    base_family: fam,
                                    marketing_names,
                                    ..
                                } if fam.eq_ignore_ascii_case(&base_family) => {
                                    Some(marketing_names)
                                }
                                _ => None,
                            })
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();

                let mut picked_marketing: Option<String> = None;
                CardSection::new()
                    .inner_margin(Margin::symmetric(0, 6))
                    .show(ui, |ui| {
                        for (idx, group) in groups.into_iter().enumerate() {
                            if ui
                                .add(
                                    CardItem::new(&group.marketing_name)
                                        .chevron()
                                        .top_divider(idx > 0),
                                )
                                .clicked()
                            {
                                picked_marketing = Some(group.marketing_name);
                            }
                        }
                    });

                if let Some(marketing_name) = picked_marketing {
                    self.browse_nav = BrowseNavState::ModelVariants {
                        root: Some(root),
                        tab_sub,
                        sub_series: Some((base_family, display_title)),
                        marketing_name,
                    };
                }
            }
            BrowseNavState::ModelVariants {
                root,
                tab_sub,
                sub_series,
                marketing_name,
            } => {
                let parent_sub = sub_series
                    .as_ref()
                    .map(|(_, t)| t.as_str())
                    .or_else(|| tab_sub.map(|s| s.title()))
                    .or_else(|| root.map(|r| r.title()));
                if render_browse_step_header(ui, &marketing_name, parent_sub) {
                    self.navigate_browse_back();
                    return;
                }

                let models: Vec<GalaxyDevice> = self
                    .shared
                    .lock()
                    .ok()
                    .and_then(|g| g.device_catalog.find_marketing_group(&marketing_name))
                    .map(|g| g.models)
                    .unwrap_or_default();

                let mut picked_model: Option<GalaxyDevice> = None;
                CardSection::new()
                    .inner_margin(Margin::symmetric(0, 6))
                    .show(ui, |ui| {
                        for (idx, dev) in models.into_iter().enumerate() {
                            if ui
                                .add(CardItem::new(&dev.model).chevron().top_divider(idx > 0))
                                .clicked()
                            {
                                picked_model = Some(dev);
                            }
                        }
                    });

                if let Some(dev) = picked_model {
                    self.select_model_in_browse(dev, ui.ctx());
                }
            }
            BrowseNavState::RegionPicker { .. } => {}
        }
    }

    fn render_browse_search_results(
        &mut self,
        ui: &mut Ui,
        query_trimmed: &str,
        catalog_rev: u64,
        db_syncing: bool,
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        if self.device_search_cached_query != query_trimmed
            || self.device_search_cached_rev != catalog_rev
        {
            if let Ok(guard) = self.shared.lock() {
                let (indices, total) = guard
                    .device_catalog
                    .search_indices(query_trimmed, MAX_DEVICE_LIST_ROWS);
                self.device_search_cached_indices = indices;
                self.device_search_cached_total = total;
                self.device_search_cached_query = query_trimmed.to_owned();
                self.device_search_cached_rev = catalog_rev;
            }
        }

        let catalog_rows: Vec<(String, GalaxyDevice)> = if let Ok(guard) = self.shared.lock() {
            let entries = guard.device_catalog.entries();
            self.device_search_cached_indices
                .iter()
                .filter_map(|&idx| {
                    entries
                        .get(idx)
                        .map(|e| (e.display_label.clone(), e.device.clone()))
                })
                .collect()
        } else {
            Vec::new()
        };

        let query_lower = query_trimmed.to_ascii_lowercase();
        let query_tokens: Vec<&str> = query_lower.split_whitespace().collect();
        let mut display_rows: Vec<SearchResultRow> = Vec::new();
        let mut seen_marketing: Vec<String> = Vec::new();

        for (label, dev) in &catalog_rows {
            let name_lower = dev.marketing_name.to_ascii_lowercase();
            let all_tokens_in_name = query_tokens.iter().all(|t| name_lower.contains(t));
            if all_tokens_in_name {
                if !seen_marketing
                    .iter()
                    .any(|m| m.eq_ignore_ascii_case(&dev.marketing_name))
                {
                    seen_marketing.push(dev.marketing_name.clone());
                    display_rows.push(SearchResultRow::MarketingGroup(dev.marketing_name.clone()));
                }
            } else {
                display_rows.push(SearchResultRow::SpecificModel(label.clone(), dev.clone()));
            }
        }

        let query_upper = query_trimmed.to_ascii_uppercase();
        let show_manual_fallback = query_upper.len() >= 4
            && !catalog_rows
                .iter()
                .any(|(_, d)| d.model.eq_ignore_ascii_case(&query_upper));

        if display_rows.is_empty() && !show_manual_fallback {
            if !db_syncing {
                CardSection::new()
                    .inner_margin(Margin::symmetric(20, 18))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("No results")
                                .font(font_id(ui, 14.5, TwoUiFontWeight::Medium))
                                .color(p.text_muted),
                        );
                    });
            }
            return;
        }

        let mut picked_device: Option<GalaxyDevice> = None;
        let mut picked_group: Option<String> = None;

        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                let mut row_num = 0usize;
                if show_manual_fallback {
                    let manual_dev = GalaxyDevice {
                        marketing_name: "Galaxy".to_owned(),
                        device: String::new(),
                        model: query_upper.clone(),
                    };
                    if ui
                        .add(
                            CardItem::new(&query_upper)
                                .chevron()
                                .top_divider(false),
                        )
                        .clicked()
                    {
                        picked_device = Some(manual_dev);
                    }
                    row_num += 1;
                }

                for row in display_rows {
                    match row {
                        SearchResultRow::MarketingGroup(marketing_name) => {
                            if ui
                                .add(
                                    CardItem::new(&marketing_name)
                                        .chevron()
                                        .top_divider(row_num > 0),
                                )
                                .clicked()
                            {
                                picked_group = Some(marketing_name);
                            }
                        }
                        SearchResultRow::SpecificModel(label, dev) => {
                            if ui
                                .add(CardItem::new(label).chevron().top_divider(row_num > 0))
                                .clicked()
                            {
                                picked_device = Some(dev);
                            }
                        }
                    }
                    row_num += 1;
                }
            });

        if let Some(marketing_name) = picked_group {
            self.device_search_query.clear();
            self.browse_search_focused = false;
            self.browse_nav = BrowseNavState::ModelVariants {
                root: None,
                tab_sub: None,
                sub_series: None,
                marketing_name,
            };
        } else if let Some(dev) = picked_device {
            self.device_search_query.clear();
            self.browse_search_focused = false;
            self.select_model_in_browse(dev, ui.ctx());
        }
    }

    pub(crate) fn select_model_in_browse(&mut self, dev: GalaxyDevice, ctx: &egui::Context) {
        let def_code = self.config.default_csc.trim();
        if !def_code.is_empty() {
            if let Some(default_region) = self.csc_db.find_exact(def_code).cloned() {
                self.apply_browse_device_and_region(dev, default_region, ctx);
                return;
            }
        }
        self.open_region_picker_for(dev);
    }

    pub(crate) fn open_region_picker_for(&mut self, dev: GalaxyDevice) {
        self.device_search_query.clear();
        self.browse_search_focused = false;
        let (root, tab_sub, sub_series, from_builds_grid) = match self.browse_nav.clone() {
            BrowseNavState::ModelVariants {
                root,
                tab_sub,
                sub_series,
                ..
            } => (root, tab_sub, sub_series, self.selected_my_region.is_some()),
            BrowseNavState::RegionPicker {
                root,
                tab_sub,
                sub_series,
                from_builds_grid,
                ..
            } => (root, tab_sub, sub_series, from_builds_grid),
            _ => (None, None, None, self.selected_my_region.is_some()),
        };
        self.browse_nav = BrowseNavState::RegionPicker {
            root,
            tab_sub,
            sub_series,
            from_builds_grid,
            device: dev,
        };
    }

    pub(crate) fn handle_region_chosen_in_browse(
        &mut self,
        dev: GalaxyDevice,
        region: CscItem,
        ctx: &egui::Context,
    ) {
        if self.config.default_csc.trim().is_empty() && !self.config.dont_ask_default_csc {
            self.pending_default_region_selection = Some((dev, region));
            self.dont_ask_default_csc_draft = false;
            self.default_region_dialog_open = true;
        } else {
            self.apply_browse_device_and_region(dev, region, ctx);
        }
    }

    pub(crate) fn apply_browse_device_and_region(
        &mut self,
        dev: GalaxyDevice,
        region: CscItem,
        ctx: &egui::Context,
    ) {
        self.device_search_query.clear();
        self.browse_search_focused = false;
        if let BrowseNavState::RegionPicker {
            root,
            tab_sub,
            sub_series,
            device,
            ..
        } = self.browse_nav.clone()
        {
            self.browse_nav = BrowseNavState::ModelVariants {
                root,
                tab_sub,
                sub_series,
                marketing_name: device.marketing_name,
            };
        }
        self.selected_my_device = Some(dev.clone());
        self.selected_my_region = Some(region.clone());
        self.config.model = dev.model.clone();
        self.config.csc = region.code.clone();
        self.page_id = BloomPage::Browse.id();
        self.fetch_builds_for_selection(&dev, &region, ctx);
    }

    #[allow(clippy::too_many_arguments)]
    fn render_selected_device_grid(
        &mut self,
        ui: &mut Ui,
        dev: &GalaxyDevice,
        region: &CscItem,
        loading: bool,
        error: Option<&str>,
        builds_opt: Option<&bloom_core::HistoryResult>,
        connected_devices: &[ConnectedSamsungDevice],
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        let connected_match = connected_devices
            .iter()
            .find(|c| c.model.eq_ignore_ascii_case(&dev.model));
        let installed_build = installed_build_for_model(connected_devices, &dev.model);
        let installed_bit = installed_build
            .as_deref()
            .and_then(DecodedBuildCode::decode)
            .map(|d| d.bootloader_level);

        let mut close_builds_grid = false;

        // Compact selected device + region header card
        CardSection::new()
            .inner_margin(Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(10.0, 8.0);
                    if ui
                        .add(
                            TwoUiIconButton::new(TwoUiIcon::Back)
                                .size(34.0)
                                .icon_size(18.0)
                                .tint(p.text_primary),
                        )
                        .clicked()
                    {
                        close_builds_grid = true;
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 7.0;
                            if connected_match.is_some() {
                                let (dr, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                                ui.painter().circle_filled(dr.center(), 4.0, p.functional_green);
                            }
                            ui.label(
                                RichText::new(dev.display_label())
                                    .font(font_id(ui, 15.5, TwoUiFontWeight::SemiBold))
                                    .color(p.text_primary),
                            );
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(8.0, 4.0);
                            ui.label(
                                RichText::new(region.region_label())
                                    .font(font_id(ui, 13.0, TwoUiFontWeight::Medium))
                                    .color(p.primary_high),
                            );
                            if let Some(ref cur) = installed_build {
                                pill_tag(ui, cur, p.tone_on_tone, p.text_secondary);
                            }
                            if let Some(pct) = connected_match.and_then(|c| c.battery_percent) {
                                pill_tag(
                                    ui,
                                    &format!("{pct}%"),
                                    p.tone_on_tone,
                                    if pct <= 15 {
                                        p.functional_red
                                    } else {
                                        p.functional_green
                                    },
                                );
                            }
                        });
                    });
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                TwoUiButton::outline("Region")
                                    .enabled(!loading)
                                    .min_size(vec2(78.0, 34.0)),
                            )
                            .clicked()
                        {
                            self.open_region_picker_for(dev.clone());
                        }
                    });
                });
            });

        if close_builds_grid {
            self.selected_my_device = None;
            self.selected_my_region = None;
            self.selected_grid_build = None;
            self.build_detail_sheet_open = false;
            return;
        }

        ui.add_space(16.0);

        if loading {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                ui.add(TwoUiSpinner::indeterminate(SpinnerSize::Large));
                ui.add_space(12.0);
                ui.label(
                    RichText::new("Loading")
                        .font(font_id(ui, 14.5, TwoUiFontWeight::Medium))
                        .color(p.text_muted),
                );
            });
            return;
        }

        if let Some(err) = error {
            status_banner(
                ui,
                TwoUiIcon::Info,
                p.functional_red,
                "Unavailable",
                Some(err),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(TwoUiButton::primary("Retry")).clicked() {
                    self.fetch_builds_for_selection(dev, region, ui.ctx());
                }
                if ui.add(TwoUiButton::outline("Region")).clicked() {
                    self.open_region_picker_for(dev.clone());
                }
            });
            return;
        }

        let Some(res) = builds_opt else {
            return;
        };

        if res.items.is_empty() {
            CardSection::new()
                .inner_margin(Margin::symmetric(20, 18))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("No builds")
                            .font(font_id(ui, 14.5, TwoUiFontWeight::Medium))
                            .color(p.text_muted),
                    );
                });
            return;
        }

        let avail_w = ui.available_width().max(240.0);
        let gap = 14.0;
        let cols = (((avail_w + gap) / (148.0 + gap)).floor() as usize).clamp(2, 4);
        let tile_w = ((avail_w - gap * (cols as f32 - 1.0)) / cols as f32)
            .floor()
            .max(100.0);
        let box_h = (tile_w * 0.78).clamp(96.0, 142.0);

        let mut clicked_build: Option<HistoryItem> = None;

        for (chunk_idx, row_items) in res.items.chunks(cols).enumerate() {
            if chunk_idx > 0 {
                ui.add_space(16.0);
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (col_idx, item) in row_items.iter().enumerate() {
                    let global_idx = chunk_idx * cols + col_idx;
                    let decoded = item.decoded_build();
                    let is_now = installed_build
                        .as_deref()
                        .is_some_and(|cur| item.build.eq_ignore_ascii_case(cur));
                    let is_locked = installed_bit.is_some_and(|cur_bit| {
                        decoded
                            .as_ref()
                            .is_some_and(|d| d.is_downgrade_blocked(cur_bit))
                    });

                    let tile_title = format_firmware_tile_title(
                        item.one_ui_version.as_deref(),
                        item.android_version.as_deref(),
                    );
                    let emblem = extract_emblem_text(
                        item.one_ui_version.as_deref(),
                        item.android_version.as_deref(),
                    );
                    let build_date = format_tile_date(item);

                    ui.vertical(|ui| {
                        ui.set_width(tile_w);
                        ui.spacing_mut().item_spacing.y = 0.0;

                        let (tile_rect, tile_resp) =
                            ui.allocate_exact_size(vec2(tile_w, box_h), Sense::click());
                        if ui.is_rect_visible(tile_rect) {
                            let (press_t, hover_t) = interaction_factors(ui, &tile_resp);
                            let vis_rect = recoil_rect(tile_rect, press_t, 1.0);
                            let radius = CornerRadius::same(22);

                            let fill = if is_now {
                                p.selected_bg
                            } else {
                                p.surface
                            };
                            let stroke = if is_now {
                                Stroke::new(1.6, p.primary)
                            } else {
                                Stroke::new(0.85, p.floating_stroke)
                            };
                            ui.painter().rect(
                                vis_rect,
                                radius,
                                fill,
                                stroke,
                                StrokeKind::Inside,
                            );
                            paint_state_overlay(ui, vis_rect, radius, p, hover_t, press_t);

                            // Top-left status pill: `Now` (installed on USB phone) or `Latest` (server newest)
                            if is_now || global_idx == 0 {
                                let (badge_label, badge_bg, badge_fg) = if is_now {
                                    ("Now", p.primary, p.on_primary)
                                } else {
                                    ("Latest", p.selected_bg, p.primary_high)
                                };
                                let tag_g = ui.painter().layout_no_wrap(
                                    badge_label.to_owned(),
                                    font_id(ui, 10.0, TwoUiFontWeight::SemiBold),
                                    badge_fg,
                                );
                                let pill_sz = tag_g.size() + vec2(12.0, 4.0);
                                let pill_rect = Rect::from_min_size(
                                    pos2(vis_rect.left() + 10.0, vis_rect.top() + 10.0),
                                    pill_sz,
                                );
                                ui.painter().rect_filled(
                                    pill_rect,
                                    CornerRadius::same(8),
                                    badge_bg,
                                );
                                ui.painter().galley(
                                    pill_rect.center() - tag_g.size() * 0.5,
                                    tag_g,
                                    badge_fg,
                                );
                            }

                            // Top-right update-type visual icon (Feature = Star, Security = SecurityOutline)
                            if let Some(ref d) = decoded {
                                let type_icon_col = |base: Color32| {
                                    if is_locked {
                                        base.gamma_multiply(0.45)
                                    } else {
                                        base
                                    }
                                };
                                let icon_rect = Rect::from_center_size(
                                    pos2(vis_rect.right() - 16.0, vis_rect.top() + 16.0),
                                    vec2(14.0, 14.0),
                                );
                                match d.update_type {
                                    FirmwareUpdateType::Feature => {
                                        TwoUiIcon::Star.paint(
                                            ui,
                                            icon_rect,
                                            type_icon_col(p.primary_high),
                                        );
                                    }
                                    FirmwareUpdateType::Security => {
                                        TwoUiIcon::SecurityOutline.paint(
                                            ui,
                                            icon_rect,
                                            type_icon_col(p.functional_green),
                                        );
                                    }
                                    FirmwareUpdateType::Other(_) => {}
                                }
                            }

                            // Center One UI / Android number emblem
                            let emblem_center = vis_rect.center() - vec2(0.0, 4.0);
                            let emblem_r = (box_h * 0.24).clamp(18.0, 26.0);
                            let halo_col = p
                                .primary
                                .gamma_multiply(if is_locked { 0.06 } else { 0.14 });
                            ui.painter().circle_filled(emblem_center, emblem_r, halo_col);

                            let emblem_fg = if is_locked {
                                p.text_disabled
                            } else {
                                p.primary_high
                            };
                            let emblem_size = if emblem.len() >= 4 { 14.5 } else { 18.0 };
                            let os_g = ui.painter().layout_no_wrap(
                                emblem,
                                font_id(ui, emblem_size, TwoUiFontWeight::SemiBold),
                                emblem_fg,
                            );
                            ui.painter().galley(
                                emblem_center - os_g.size() * 0.5,
                                os_g,
                                emblem_fg,
                            );

                            // Bottom-center subtle 4-char revision tail (e.g. EZJD)
                            let tail = short_build_tail(&item.build);
                            let pda_g = ui.painter().layout_no_wrap(
                                tail.to_owned(),
                                font_id(ui, 11.0, TwoUiFontWeight::Medium),
                                p.text_muted,
                            );
                            let text_pos = pos2(
                                vis_rect.center().x - pda_g.size().x * 0.5,
                                vis_rect.bottom() - pda_g.size().y - 9.0,
                            );
                            ui.painter()
                                .with_clip_rect(vis_rect.shrink(6.0))
                                .galley(text_pos, pda_g, p.text_muted);

                            // Bottom-right LockOutline icon if Bootloader Bit < installed Bit
                            if is_locked {
                                let lock_rect = Rect::from_center_size(
                                    pos2(vis_rect.right() - 16.0, vis_rect.bottom() - 15.0),
                                    vec2(13.0, 13.0),
                                );
                                TwoUiIcon::LockOutline.paint(ui, lock_rect, p.text_muted);
                            }
                        }

                        if tile_resp.clicked() {
                            clicked_build = Some(item.clone());
                        }

                        // Strictly 2 short lines below kotak: One UI/OS title + date
                        ui.add_space(7.0);
                        ui.horizontal(|ui| {
                            ui.add_space(4.0);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(
                                    RichText::new(tile_title)
                                        .font(font_id(ui, 14.5, TwoUiFontWeight::SemiBold))
                                        .color(if is_locked {
                                            p.text_secondary
                                        } else {
                                            p.text_primary
                                        }),
                                );
                                ui.label(
                                    RichText::new(build_date)
                                        .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                                        .color(p.text_muted),
                                );
                            });
                        });
                    });
                }
            });
        }

        if let Some(build) = clicked_build {
            self.selected_grid_build = Some(build);
            self.build_detail_sheet_open = true;
        }
    }

    pub fn trigger_sync_galaxy_db(&mut self, ctx: &egui::Context, force_rebuild: bool) {
        if force_rebuild {
            self.device_render_textures.clear();
        }
        crate::tasks::spawn_sync_galaxy_db(&self.rt, self.shared.clone(), ctx.clone(), force_rebuild);
    }

    pub(crate) fn fetch_builds_for_selection(
        &self,
        dev: &GalaxyDevice,
        region: &CscItem,
        ctx: &egui::Context,
    ) {
        let lang = if self.config.history_lang_idx == 1 {
            ChangelogLang::Indonesian
        } else {
            ChangelogLang::English
        };
        crate::tasks::spawn_fetch_my_device_builds(
            &self.rt,
            self.shared.clone(),
            ctx.clone(),
            dev.model.clone(),
            region.code.clone(),
            lang,
        );
    }

    pub fn render_default_region_dialog(&mut self, ctx: &egui::Context) {
        if !self.default_region_dialog_open {
            return;
        }
        let Some((_, ref region)) = self.pending_default_region_selection else {
            self.default_region_dialog_open = false;
            return;
        };

        let body = format!(
            "Save {} as your default region? You can change this anytime in Settings.",
            region.region_label()
        );

        let action_idx =
            TwoUiDialog::new("bloom_default_region_dialog", "Save default region?")
                .body(&body)
                .single_choice_option(&mut self.dont_ask_default_csc_draft, "Don't ask again")
                .action(DialogAction::cancel("Cancel"))
                .action(DialogAction::cancel("No"))
                .action(DialogAction::primary("Yes"))
                .show(ctx, &mut self.default_region_dialog_open, |_| {});

        if let Some(idx) = action_idx {
            if let Some((dev, region)) = self.pending_default_region_selection.take() {
                match idx {
                    0 => {
                        // Cancel: remain on Region picker list without applying
                    }
                    1 => {
                        if self.dont_ask_default_csc_draft {
                            self.config.dont_ask_default_csc = true;
                        }
                        self.apply_browse_device_and_region(dev, region, ctx);
                    }
                    2 => {
                        self.config.default_csc = region.code.clone();
                        if self.dont_ask_default_csc_draft {
                            self.config.dont_ask_default_csc = true;
                        }
                        self.apply_browse_device_and_region(dev, region, ctx);
                    }
                    _ => {}
                }
            }
        } else if !self.default_region_dialog_open {
            self.pending_default_region_selection = None;
        }
    }

    pub fn render_build_detail_sheet(&mut self, ctx: &egui::Context) {
        let Some(item) = self.selected_grid_build.clone() else {
            self.build_detail_sheet_open = false;
            return;
        };
        if !self.build_detail_sheet_open {
            return;
        }

        let connected_devices = self
            .shared
            .lock()
            .map(|g| g.connected_devices.clone())
            .unwrap_or_default();
        let model_code = self
            .selected_my_device
            .as_ref()
            .map(|d| d.model.clone())
            .unwrap_or_else(|| self.config.model.clone());
        let region_code = self
            .selected_my_region
            .as_ref()
            .map(|r| r.code.clone())
            .unwrap_or_else(|| self.config.csc.clone());

        let installed_build = installed_build_for_model(&connected_devices, &model_code);
        let installed_bit = installed_build
            .as_deref()
            .and_then(DecodedBuildCode::decode)
            .map(|d| d.bootloader_level);
        let decoded = item.decoded_build();
        let is_now = installed_build
            .as_deref()
            .is_some_and(|cur| item.build.eq_ignore_ascii_case(cur));
        let is_locked = installed_bit.is_some_and(|cur_bit| {
            decoded
                .as_ref()
                .is_some_and(|d| d.is_downgrade_blocked(cur_bit))
        });

        let theme = theme_from_ctx(ctx);
        let p = &theme.palette;
        let sheet_title = format_firmware_tile_title(
            item.one_ui_version.as_deref(),
            item.android_version.as_deref(),
        );
        let build_date = format_tile_date(&item);
        let sec_patch = item
            .changelog
            .as_ref()
            .and_then(|c| c.sec_patch.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let mut open_decrypt = false;
        let mut copy_fw = false;

        TwoUiBottomSheet::new("browse_build_detail", &sheet_title)
            .max_width(560.0)
            .max_height(500.0)
            .show(ctx, &mut self.build_detail_sheet_open, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                    if is_now {
                        pill_tag(ui, "Now", p.primary, p.on_primary);
                    }
                    if let Some(ref d) = decoded {
                        match d.update_type {
                            FirmwareUpdateType::Feature => {
                                pill_tag(ui, "Feature", p.selected_bg, p.primary_high);
                            }
                            FirmwareUpdateType::Security => {
                                pill_tag(
                                    ui,
                                    "Security",
                                    p.functional_green.gamma_multiply(0.18),
                                    p.functional_green,
                                );
                            }
                            FirmwareUpdateType::Other(ch) => {
                                pill_tag(ui, &ch.to_string(), p.tone_on_tone, p.text_secondary);
                            }
                        }
                        let (bit_bg, bit_fg) = if is_locked {
                            (p.functional_red.gamma_multiply(0.18), p.functional_red)
                        } else if installed_bit.is_some() {
                            (p.functional_green.gamma_multiply(0.18), p.functional_green)
                        } else {
                            (p.tone_on_tone, p.text_primary)
                        };
                        pill_tag(
                            ui,
                            &format!("Bit {} ({})", d.bootloader_bit, d.bootloader_level),
                            bit_bg,
                            bit_fg,
                        );
                        if let Some(rev) = d.revision {
                            pill_tag(ui, &format!("Rev {rev}"), p.tone_on_tone, p.text_secondary);
                        }
                        pill_tag(ui, &d.region_group, p.tone_on_tone, p.text_muted);
                    }
                });

                CardSection::new()
                    .fill(p.surface_container)
                    .inner_margin(Margin::symmetric(0, 4))
                    .show(ui, |ui| {
                        key_value_row(ui, "Build", &item.build, true);
                        paint_divider(ui, DividerInset::Standard);
                        key_value_row(ui, "Date", &build_date, false);
                        if let Some(patch) = sec_patch {
                            paint_divider(ui, DividerInset::Standard);
                            key_value_row(ui, "Patch", patch, false);
                        }
                        paint_divider(ui, DividerInset::Standard);
                        key_value_row(ui, "Full", &item.firmware, false);
                    });

                if let Some(notes) = item
                    .changelog
                    .as_ref()
                    .and_then(|c| c.notes.as_deref())
                    .filter(|s| !s.trim().is_empty())
                {
                    Frame::NONE
                        .fill(p.surface_container)
                        .corner_radius(CornerRadius::same(16))
                        .inner_margin(Margin::symmetric(14, 12))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width().max(120.0));
                            ui.label(
                                RichText::new(notes)
                                    .font(font_id(ui, 13.5, TwoUiFontWeight::Regular))
                                    .color(p.text_primary),
                            );
                        });
                }

                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(10.0, 8.0);
                    if ui
                        .add(
                            TwoUiButton::contained_primary("Decrypt")
                                .icon(TwoUiIcon::SecurityOutline),
                        )
                        .clicked()
                    {
                        open_decrypt = true;
                    }
                    if ui
                        .add(TwoUiButton::outline("Copy").icon(TwoUiIcon::Copy))
                        .clicked()
                    {
                        copy_fw = true;
                    }
                });
            });

        if copy_fw {
            ctx.copy_text(item.firmware.clone());
            self.toast.show_message(ctx, "Copied", 1.8);
        }
        if open_decrypt {
            self.build_detail_sheet_open = false;
            self.load_build_into_decrypt(model_code, region_code, item.firmware, ctx);
        }
    }
}
