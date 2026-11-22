use bloom_core::{
    ChangelogLang, ConnectedSamsungDevice, CscItem, DecodedBuildCode, FirmwareUpdateType,
    GalaxyCategory, GalaxyDevice, GalaxyTabSubCategory, HistoryItem,
};
use egui::{
    pos2, vec2, Align, Align2, Area, CornerRadius, Id, Layout, Margin, Order, Rect, RichText,
    Sense, Stroke, StrokeKind, Ui,
};
use egui_twoui::feedback::{interaction_factors, paint_state_overlay, recoil_rect};
use egui_twoui::prelude::*;

use crate::state::{format_firmware_tile_title, format_tile_date, BloomPage, BrowseStep};
use crate::widgets::{pill_tag, status_banner};
use crate::BloomApp;

const MAX_DEVICE_LIST_ROWS: usize = 120;

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

fn browse_back_header(ui: &mut Ui, title: &str, subtitle: Option<&str>) -> bool {
    let theme = theme_from_ui(ui);
    let p = &theme.palette;
    let mut back_clicked = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        if ui
            .add(TwoUiIconButton::new(TwoUiIcon::Back).tooltip("Back"))
            .clicked()
        {
            back_clicked = true;
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(
                RichText::new(title)
                    .font(font_id(ui, 17.5, TwoUiFontWeight::SemiBold))
                    .color(p.text_primary),
            );
            if let Some(sub) = subtitle.filter(|s| !s.trim().is_empty()) {
                ui.label(
                    RichText::new(sub)
                        .font(font_id(ui, 13.0, TwoUiFontWeight::Regular))
                        .color(p.text_muted),
                );
            }
        });
    });
    back_clicked
}

fn ease_cubic_directional(linear: f32, forward: bool) -> f32 {
    if forward {
        let u = 1.0 - linear;
        1.0 - u * u * u
    } else {
        linear * linear * linear
    }
}

impl BloomApp {
    pub fn render_browse_page(&mut self, ui: &mut Ui) {
        let col_rect = ui.max_rect();
        self.browse_col_anchor = Some((col_rect.center().x, col_rect.width().max(200.0)));

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
            ui.add_space(10.0);
        }

        if let BrowseStep::RegionPicker {
            device,
            return_to_builds,
        } = self.browse_step.clone()
        {
            self.render_inline_region_picker(ui, device, return_to_builds);
            ui.add_space(76.0);
            return;
        }

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
            return;
        }

        let query_trimmed = self.device_search_query.trim().to_owned();
        if !query_trimmed.is_empty() {
            self.render_global_search_results(
                ui,
                &query_trimmed,
                db_syncing,
                db_error.as_deref(),
                catalog_len,
                catalog_rev,
            );
            ui.add_space(76.0);
            return;
        }

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
                ui.add_space(76.0);
                return;
            } else if let Some(ref err) = db_error {
                status_banner(ui, TwoUiIcon::Info, p.functional_red, "Sync failed", Some(err));
                ui.add_space(10.0);
                if ui.add(TwoUiButton::primary("Retry")).clicked() {
                    self.trigger_sync_galaxy_db(ui.ctx(), false);
                }
                ui.add_space(76.0);
                return;
            }
        }

        match self.browse_step.clone() {
            BrowseStep::Categories => self.render_categories_step(ui),
            BrowseStep::TabSubCategories => self.render_tab_subcategories_step(ui),
            BrowseStep::Lineups { category, tab_sub } => {
                self.render_lineups_step(ui, category, tab_sub)
            }
            BrowseStep::Models {
                category,
                tab_sub,
                lineup_key,
                lineup_title,
            } => self.render_models_step(ui, category, tab_sub, &lineup_key, &lineup_title),
            BrowseStep::Variants {
                category,
                tab_sub,
                lineup_key,
                lineup_title,
                marketing_name,
            } => self.render_variants_step(
                ui,
                category,
                tab_sub,
                lineup_key,
                lineup_title,
                &marketing_name,
            ),
            BrowseStep::RegionPicker { .. } => {}
        }

        ui.add_space(76.0);
    }

    pub fn render_browse_bottom_search_bar(&mut self, ui: &mut Ui) {
        let in_region_picker = matches!(self.browse_step, BrowseStep::RegionPicker { .. });
        let (query, hint, focus_id) = if in_region_picker {
            (
                &mut self.region_picker_query,
                "Search region",
                Id::unique("bloom_browse_region_search_focus"),
            )
        } else {
            (
                &mut self.device_search_query,
                "Search",
                Id::unique("bloom_browse_catalog_search_focus"),
            )
        };

        let (col_center_x, col_w) = self.browse_col_anchor.unwrap_or_else(|| {
            let r = ui.max_rect();
            (r.center().x, (r.width() - 32.0).clamp(200.0, 840.0))
        });

        let prev_focused: bool = ui.ctx().data(|d| d.get_temp(focus_id).unwrap_or(false));
        let is_expanded = prev_focused || !query.trim().is_empty();
        let linear_t = ui.ctx().animate_bool_with_time(
            Id::unique("bloom_browse_search_width_anim"),
            is_expanded,
            0.24,
        );
        let ease_t = ease_cubic_directional(linear_t, is_expanded);

        let collapsed_w = (if in_region_picker { 200.0_f32 } else { 172.0_f32 }).min(col_w);
        let current_w = collapsed_w + (col_w - collapsed_w) * ease_t;
        let offset_x = col_center_x - ui.ctx().content_rect().center().x;

        let area_resp = Area::new(Id::unique("bloom_browse_bottom_search_area"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_BOTTOM, vec2(offset_x, -16.0))
            .pivot(Align2::CENTER_BOTTOM)
            .show(ui.ctx(), |ui| {
                ui.allocate_ui_with_layout(
                    vec2(current_w, 52.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_width(current_w);
                        TwoUiSearchBar::new(query)
                            .hint(hint)
                            .floating(true)
                            .show(ui)
                    },
                )
                .inner
            });

        let (edit_resp, _) = area_resp.inner;
        let pill_rect = area_resp.response.rect;
        // Read `has_focus()` BEFORE locking `ctx.data_mut` to avoid re-entrant `Memory` lock.
        let mut now_focused = edit_resp.has_focus();
        if !now_focused
            && ui.input(|i| {
                i.pointer.primary_clicked()
                    && i.pointer.interact_pos().is_some_and(|pt| pill_rect.contains(pt))
            })
        {
            edit_resp.request_focus();
            now_focused = true;
        }
        ui.ctx().data_mut(|d| d.insert_temp(focus_id, now_focused));
    }

    fn render_categories_step(&mut self, ui: &mut Ui) {
        let counts = match self.shared.lock() {
            Ok(g) => g.device_catalog.category_counts(),
            Err(_) => return,
        };

        SectionHeader::new("Categories").top_space(4.0).show(ui);
        let mut chosen: Option<GalaxyCategory> = None;

        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                for (idx, (cat, count)) in counts.into_iter().enumerate() {
                    let summary = format!("{count} models");
                    let resp = ui.add(
                        CardItem::new(cat.title())
                            .summary(summary)
                            .chevron()
                            .enabled(count > 0)
                            .top_divider(idx > 0),
                    );
                    if resp.clicked() {
                        chosen = Some(cat);
                    }
                }
            });

        if let Some(cat) = chosen {
            self.browse_step = match cat {
                GalaxyCategory::Tab => BrowseStep::TabSubCategories,
                _ => BrowseStep::Lineups {
                    category: cat,
                    tab_sub: None,
                },
            };
        }
    }

    fn render_tab_subcategories_step(&mut self, ui: &mut Ui) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        if browse_back_header(ui, GalaxyCategory::Tab.title(), None) {
            self.browse_step = BrowseStep::Categories;
            return;
        }
        ui.add_space(10.0);

        let subs = match self.shared.lock() {
            Ok(g) => g.device_catalog.tab_subcategory_counts(),
            Err(_) => return,
        };

        if subs.is_empty() {
            CardSection::new()
                .inner_margin(Margin::symmetric(20, 18))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("No devices")
                            .font(font_id(ui, 14.5, TwoUiFontWeight::Medium))
                            .color(p.text_muted),
                    );
                });
            return;
        }

        let mut chosen: Option<GalaxyTabSubCategory> = None;
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                for (idx, (sub, count)) in subs.into_iter().enumerate() {
                    let summary = format!("{count} models");
                    if ui
                        .add(
                            CardItem::new(sub.title())
                                .summary(summary)
                                .chevron()
                                .top_divider(idx > 0),
                        )
                        .clicked()
                    {
                        chosen = Some(sub);
                    }
                }
            });

        if let Some(sub) = chosen {
            self.browse_step = BrowseStep::Lineups {
                category: GalaxyCategory::Tab,
                tab_sub: Some(sub),
            };
        }
    }

    fn render_lineups_step(
        &mut self,
        ui: &mut Ui,
        category: GalaxyCategory,
        tab_sub: Option<GalaxyTabSubCategory>,
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;
        let title = tab_sub.map_or(category.title(), |s| s.title());

        if browse_back_header(ui, title, None) {
            self.browse_step = if tab_sub.is_some() {
                BrowseStep::TabSubCategories
            } else {
                BrowseStep::Categories
            };
            return;
        }
        ui.add_space(10.0);

        let lineups = match self.shared.lock() {
            Ok(g) => g.device_catalog.lineups_for(category, tab_sub),
            Err(_) => return,
        };

        if lineups.is_empty() {
            CardSection::new()
                .inner_margin(Margin::symmetric(20, 18))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("No devices")
                            .font(font_id(ui, 14.5, TwoUiFontWeight::Medium))
                            .color(p.text_muted),
                    );
                });
            return;
        }

        let mut picked_idx: Option<usize> = None;
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                for (idx, lineup) in lineups.iter().enumerate() {
                    let summary = if lineup.device_names.len() > 1 {
                        format!(
                            "{} models · {} variants",
                            lineup.device_names.len(),
                            lineup.total_variants
                        )
                    } else if lineup.total_variants > 1 {
                        format!("{} variants", lineup.total_variants)
                    } else {
                        "1 model".to_owned()
                    };
                    if ui
                        .add(
                            CardItem::new(&lineup.display_title)
                                .summary(summary)
                                .chevron()
                                .top_divider(idx > 0),
                        )
                        .clicked()
                    {
                        picked_idx = Some(idx);
                    }
                }
            });

        if let Some(idx) = picked_idx {
            let lineup = lineups.into_iter().nth(idx);
            if let Some(lineup) = lineup {
                if lineup.device_names.len() > 1 {
                    self.browse_step = BrowseStep::Models {
                        category,
                        tab_sub,
                        lineup_key: lineup.key,
                        lineup_title: lineup.display_title,
                    };
                } else if let Some(single_name) = lineup.device_names.into_iter().next() {
                    let variants = match self.shared.lock() {
                        Ok(g) => g.device_catalog.variants_for_marketing_name(&single_name),
                        Err(_) => Vec::new(),
                    };
                    if variants.len() == 1 {
                        if let Some(dev) = variants.into_iter().next() {
                            self.select_device_variant(dev, ui.ctx());
                        }
                    } else {
                        self.browse_step = BrowseStep::Variants {
                            category,
                            tab_sub,
                            lineup_key: None,
                            lineup_title: None,
                            marketing_name: single_name,
                        };
                    }
                }
            }
        }
    }

    fn render_models_step(
        &mut self,
        ui: &mut Ui,
        category: GalaxyCategory,
        tab_sub: Option<GalaxyTabSubCategory>,
        lineup_key: &str,
        lineup_title: &str,
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        if browse_back_header(ui, lineup_title, None) {
            self.browse_step = BrowseStep::Lineups { category, tab_sub };
            return;
        }
        ui.add_space(10.0);

        let models = match self.shared.lock() {
            Ok(g) => g.device_catalog.models_for_lineup(category, tab_sub, lineup_key),
            Err(_) => return,
        };

        if models.is_empty() {
            CardSection::new()
                .inner_margin(Margin::symmetric(20, 18))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("No models")
                            .font(font_id(ui, 14.5, TwoUiFontWeight::Medium))
                            .color(p.text_muted),
                    );
                });
            return;
        }

        let mut picked_idx: Option<usize> = None;
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                for (idx, group) in models.iter().enumerate() {
                    let summary = if group.variants.len() > 1 {
                        format!("{} variants", group.variants.len())
                    } else if let Some(first) = group.variants.first() {
                        first.model.clone()
                    } else {
                        String::new()
                    };
                    if ui
                        .add(
                            CardItem::new(&group.marketing_name)
                                .summary(summary)
                                .chevron()
                                .top_divider(idx > 0),
                        )
                        .clicked()
                    {
                        picked_idx = Some(idx);
                    }
                }
            });

        if let Some(idx) = picked_idx {
            if let Some(mut group) = models.into_iter().nth(idx) {
                if group.variants.len() == 1 {
                    if let Some(dev) = group.variants.pop() {
                        self.select_device_variant(dev, ui.ctx());
                    }
                } else {
                    self.browse_step = BrowseStep::Variants {
                        category,
                        tab_sub,
                        lineup_key: Some(lineup_key.to_owned()),
                        lineup_title: Some(lineup_title.to_owned()),
                        marketing_name: group.marketing_name,
                    };
                }
            }
        }
    }

    fn render_variants_step(
        &mut self,
        ui: &mut Ui,
        category: GalaxyCategory,
        tab_sub: Option<GalaxyTabSubCategory>,
        lineup_key: Option<String>,
        lineup_title: Option<String>,
        marketing_name: &str,
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        if browse_back_header(ui, marketing_name, Some("Available models")) {
            self.browse_step = match (lineup_key, lineup_title) {
                (Some(key), Some(title)) => BrowseStep::Models {
                    category,
                    tab_sub,
                    lineup_key: key,
                    lineup_title: title,
                },
                _ => BrowseStep::Lineups { category, tab_sub },
            };
            return;
        }
        ui.add_space(10.0);

        let variants = match self.shared.lock() {
            Ok(g) => g.device_catalog.variants_for_marketing_name(marketing_name),
            Err(_) => return,
        };

        if variants.is_empty() {
            CardSection::new()
                .inner_margin(Margin::symmetric(20, 18))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("No variants")
                            .font(font_id(ui, 14.5, TwoUiFontWeight::Medium))
                            .color(p.text_muted),
                    );
                });
            return;
        }

        let mut picked: Option<GalaxyDevice> = None;
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                for (idx, dev) in variants.iter().enumerate() {
                    let summary = if dev.device.trim().is_empty() {
                        dev.marketing_name.clone()
                    } else {
                        format!("{} · {}", dev.marketing_name, dev.device)
                    };
                    if ui
                        .add(
                            CardItem::new(&dev.model)
                                .summary(summary)
                                .chevron()
                                .top_divider(idx > 0),
                        )
                        .clicked()
                    {
                        picked = Some(dev.clone());
                    }
                }
            });

        if let Some(dev) = picked {
            self.select_device_variant(dev, ui.ctx());
        }
    }

    fn render_global_search_results(
        &mut self,
        ui: &mut Ui,
        query_trimmed: &str,
        db_syncing: bool,
        db_error: Option<&str>,
        catalog_len: usize,
        catalog_rev: u64,
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        if catalog_len == 0 && !db_syncing {
            if let Some(err) = db_error {
                status_banner(ui, TwoUiIcon::Info, p.functional_red, "Sync failed", Some(err));
                ui.add_space(10.0);
                if ui.add(TwoUiButton::primary("Retry")).clicked() {
                    self.trigger_sync_galaxy_db(ui.ctx(), false);
                }
                ui.add_space(12.0);
            }
        }

        if self.device_search_cached_query != query_trimmed
            || self.device_search_cached_rev != catalog_rev
        {
            if let Ok(guard) = self.shared.lock() {
                let (indices, total) =
                    guard
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

        let total_matches = self.device_search_cached_total;
        let query_upper = query_trimmed.to_ascii_uppercase();
        let show_manual_fallback = query_upper.len() >= 4
            && !catalog_rows
                .iter()
                .any(|(_, d)| d.model.eq_ignore_ascii_case(&query_upper));

        if catalog_rows.is_empty() && !show_manual_fallback {
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

                for (label, dev) in catalog_rows {
                    if ui
                        .add(CardItem::new(label).chevron().top_divider(row_num > 0))
                        .clicked()
                    {
                        picked_device = Some(dev);
                    }
                    row_num += 1;
                }
            });

        if total_matches > MAX_DEVICE_LIST_ROWS {
            let cap = format!("{MAX_DEVICE_LIST_ROWS} / {total_matches}");
            TwoUiPreferenceCaption::new(&cap).show(ui);
        }

        if let Some(dev) = picked_device {
            self.select_device_variant(dev, ui.ctx());
        }
    }

    pub(crate) fn select_device_variant(&mut self, dev: GalaxyDevice, ctx: &egui::Context) {
        if let Some(default_code) = self.config.default_region_csc.as_deref() {
            if let Some(region) = self.csc_db.find_exact(default_code).cloned() {
                self.selected_my_device = Some(dev.clone());
                self.selected_my_region = Some(region.clone());
                self.config.model = dev.model.clone();
                self.config.csc = region.code.clone();
                self.page_id = BloomPage::Browse.id();
                self.fetch_builds_for_selection(&dev, &region, ctx);
                return;
            }
        }
        self.open_region_picker_for(dev, false);
    }

    pub(crate) fn open_region_picker_for(&mut self, dev: GalaxyDevice, return_to_builds: bool) {
        if !matches!(self.browse_step, BrowseStep::RegionPicker { .. }) {
            self.last_catalog_step = self.browse_step.clone();
        }
        self.region_picker_query.clear();
        self.browse_step = BrowseStep::RegionPicker {
            device: dev,
            return_to_builds,
        };
    }

    pub(crate) fn apply_selected_device_and_region(
        &mut self,
        dev: GalaxyDevice,
        region: CscItem,
        ctx: &egui::Context,
    ) {
        self.region_picker_query.clear();
        ctx.data_mut(|d| d.insert_temp(Id::unique("bloom_browse_region_search_focus"), false));
        self.browse_step = if matches!(self.last_catalog_step, BrowseStep::RegionPicker { .. }) {
            BrowseStep::Categories
        } else {
            self.last_catalog_step.clone()
        };
        self.selected_my_device = Some(dev.clone());
        self.selected_my_region = Some(region.clone());
        self.config.model = dev.model.clone();
        self.config.csc = region.code.clone();
        self.page_id = BloomPage::Browse.id();
        self.fetch_builds_for_selection(&dev, &region, ctx);
    }

    fn render_inline_region_picker(
        &mut self,
        ui: &mut Ui,
        dev: GalaxyDevice,
        return_to_builds: bool,
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        let subtitle = dev.display_label();
        if browse_back_header(ui, "Select region", Some(&subtitle)) {
            self.region_picker_query.clear();
            let restore = if matches!(self.last_catalog_step, BrowseStep::RegionPicker { .. }) {
                BrowseStep::Categories
            } else {
                self.last_catalog_step.clone()
            };
            self.browse_step = restore;
            if !return_to_builds && self.selected_my_region.is_none() {
                self.selected_my_device = None;
            }
            return;
        }
        ui.add_space(10.0);

        // Globally neutral A–Z list ordered by country/region name (zero geographic bias).
        let ordered = self.csc_db.search(&self.region_picker_query);
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
            if self.config.default_region_csc.is_none() && !self.config.dont_ask_default_region {
                self.default_region_dialog_pending = Some((dev, region));
                self.default_region_dont_ask_draft = false;
                self.default_region_dialog_open = true;
            } else {
                self.apply_selected_device_and_region(dev, region, ui.ctx());
            }
        }
    }

    pub fn render_default_region_dialog(&mut self, ctx: &egui::Context) {
        let Some((_dev, region)) = self.default_region_dialog_pending.clone() else {
            self.default_region_dialog_open = false;
            return;
        };

        let body = format!(
            "Save {} as your default region?\n\nYou can change this anytime in Settings.",
            region.region_label()
        );
        if let Some(idx) = TwoUiDialog::new("bloom_default_region_dialog", "Set default region?")
            .body(&body)
            .width(400.0)
            .single_choice_option(&mut self.default_region_dont_ask_draft, "Don't ask again")
            .action(DialogAction::cancel("Cancel"))
            .action(DialogAction::cancel("No"))
            .action(DialogAction::primary("Yes"))
            .show(ctx, &mut self.default_region_dialog_open, |_| {})
        {
            if let Some((dev, region)) = self.default_region_dialog_pending.take() {
                match idx {
                    1 => {
                        if self.default_region_dont_ask_draft {
                            self.config.dont_ask_default_region = true;
                        }
                        self.apply_selected_device_and_region(dev, region, ctx);
                    }
                    2 => {
                        self.config.default_region_csc = Some(region.code.clone());
                        if self.default_region_dont_ask_draft {
                            self.config.dont_ask_default_region = true;
                        }
                        self.apply_selected_device_and_region(dev, region, ctx);
                    }
                    _ => {}
                }
            }
        } else if !self.default_region_dialog_open {
            self.default_region_dialog_pending = None;
        }
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

        let mut go_back_from_grid = false;

        // Compact selected device + region header card
        CardSection::new()
            .inner_margin(Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(10.0, 8.0);
                    if ui
                        .add(TwoUiIconButton::new(TwoUiIcon::Back).tooltip("Back"))
                        .clicked()
                    {
                        go_back_from_grid = true;
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
                            self.open_region_picker_for(dev.clone(), true);
                        }
                    });
                });
            });

        if go_back_from_grid {
            self.selected_my_device = None;
            self.selected_my_region = None;
            self.selected_grid_build = None;
            self.firmware_info_open = false;
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
                    self.open_region_picker_for(dev.clone(), true);
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
        let box_h = tile_w;

        enum ClickedCard {
            OpenInfo(HistoryItem),
            ConfirmLocked(HistoryItem),
        }
        let mut clicked_card: Option<ClickedCard> = None;

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
                    let is_lower_binary = installed_bit.is_some_and(|cur_bit| {
                        decoded
                            .as_ref()
                            .is_some_and(|d| d.is_downgrade_blocked(cur_bit))
                    });
                    let is_locked = is_lower_binary && !self.config.allow_lower_binary;
                    let is_latest = global_idx == 0;

                    let tile_title = format_firmware_tile_title(
                        item.one_ui_version.as_deref(),
                        item.android_version.as_deref(),
                    );
                    let build_date = format_tile_date(item);
                    let type_label = match decoded.as_ref().map(|d| d.update_type) {
                        Some(FirmwareUpdateType::Feature) => "Feature",
                        Some(FirmwareUpdateType::Security) => "Security",
                        _ => "Update",
                    };

                    ui.vertical(|ui| {
                        ui.set_width(tile_w);
                        ui.spacing_mut().item_spacing.y = 0.0;

                        let (tile_rect, tile_resp) =
                            ui.allocate_exact_size(vec2(tile_w, box_h), Sense::click());
                        if ui.is_rect_visible(tile_rect) {
                            let (press_t, hover_t) = if is_locked {
                                (0.0, 0.0)
                            } else {
                                interaction_factors(ui, &tile_resp)
                            };
                            let vis_rect = recoil_rect(tile_rect, press_t, 1.0);
                            let radius = CornerRadius::same(22);

                            let (fill, stroke) = if is_now {
                                (p.selected_bg, Stroke::new(2.0, p.primary))
                            } else if is_locked {
                                (
                                    p.surface.gamma_multiply(0.55),
                                    Stroke::new(0.85, p.floating_stroke.gamma_multiply(0.55)),
                                )
                            } else if is_latest {
                                (p.surface, Stroke::new(2.0, p.functional_green))
                            } else {
                                (p.surface, Stroke::new(0.85, p.floating_stroke))
                            };

                            ui.painter().rect(
                                vis_rect,
                                radius,
                                fill,
                                stroke,
                                StrokeKind::Inside,
                            );
                            if !is_locked {
                                paint_state_overlay(ui, vis_rect, radius, p, hover_t, press_t);
                            }

                            let text_fg = if is_locked {
                                p.text_disabled
                            } else {
                                p.text_primary
                            };
                            let type_g = ui.painter().layout_no_wrap(
                                type_label.to_owned(),
                                font_id(ui, 15.0, TwoUiFontWeight::SemiBold),
                                text_fg,
                            );

                            if is_locked {
                                let gap_y = 6.0;
                                let icon_h = 20.0;
                                let total_h = icon_h + gap_y + type_g.size().y;
                                let top_y = vis_rect.center().y - total_h * 0.5;
                                let lock_rect = Rect::from_center_size(
                                    pos2(vis_rect.center().x, top_y + icon_h * 0.5),
                                    vec2(icon_h, icon_h),
                                );
                                TwoUiIcon::Lock.paint(ui, lock_rect, p.text_disabled);
                                let text_pos = pos2(
                                    vis_rect.center().x - type_g.size().x * 0.5,
                                    top_y + icon_h + gap_y,
                                );
                                ui.painter().galley(text_pos, type_g, text_fg);
                            } else {
                                ui.painter().galley(
                                    vis_rect.center() - type_g.size() * 0.5,
                                    type_g,
                                    text_fg,
                                );
                            }
                        }

                        if tile_resp.clicked() {
                            clicked_card = Some(if is_locked {
                                ClickedCard::ConfirmLocked(item.clone())
                            } else {
                                ClickedCard::OpenInfo(item.clone())
                            });
                        }

                        // Strictly 2 short lines below card: One UI/OS title + date
                        ui.add_space(7.0);
                        ui.horizontal(|ui| {
                            ui.add_space(4.0);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(
                                    RichText::new(tile_title)
                                        .font(font_id(ui, 14.5, TwoUiFontWeight::SemiBold))
                                        .color(if is_locked {
                                            p.text_disabled
                                        } else {
                                            p.text_primary
                                        }),
                                );
                                ui.label(
                                    RichText::new(build_date)
                                        .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                                        .color(if is_locked {
                                            p.text_disabled
                                        } else {
                                            p.text_muted
                                        }),
                                );
                            });
                        });
                    });
                }
            });
        }

        if let Some(action) = clicked_card {
            match action {
                ClickedCard::OpenInfo(build) => {
                    self.open_firmware_info(build, ui.ctx());
                }
                ClickedCard::ConfirmLocked(build) => {
                    self.lower_binary_dialog_device_name = connected_match
                        .map(|c| c.display_title().to_owned())
                        .unwrap_or_else(|| dev.display_label());
                    self.lower_binary_dialog_target_build = Some(build);
                    self.lower_binary_ack_draft = false;
                    self.lower_binary_dialog_open = true;
                }
            }
        }
    }

    pub fn render_lower_binary_dialog(&mut self, ctx: &egui::Context) {
        if !self.lower_binary_dialog_open {
            return;
        }
        let body = format!(
            "The binary version of {} is higher than this firmware. Installing it will be rejected by the device.\n\nAre you sure you want to continue?",
            self.lower_binary_dialog_device_name
        );

        if let Some(idx) = TwoUiDialog::new("bloom_lower_binary_dialog", "Lower binary version")
            .body(&body)
            .width(400.0)
            .single_choice_option(&mut self.lower_binary_ack_draft, "I know what I'm doing")
            .action(DialogAction::cancel("Cancel"))
            .action(DialogAction::primary("OK"))
            .show(ctx, &mut self.lower_binary_dialog_open, |_| {})
        {
            if idx == 1 {
                if self.lower_binary_ack_draft {
                    self.config.allow_lower_binary = true;
                    if let Some(build) = self.lower_binary_dialog_target_build.take() {
                        self.open_firmware_info(build, ctx);
                    }
                } else {
                    self.lower_binary_dialog_open = true;
                }
            } else {
                self.lower_binary_dialog_target_build = None;
            }
        } else if !self.lower_binary_dialog_open {
            self.lower_binary_dialog_target_build = None;
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
}
