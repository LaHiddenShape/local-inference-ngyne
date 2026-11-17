use eframe::egui::{self, Color32, RichText};

use crate::app::{App, SortBy, Source, UncensoredFilters};
use crate::backend::{Event, RuntimeStatus};
use crate::catalog::UNCENSORED;
use crate::discover::{self, RemoteModel};
use crate::system::{fmt_bytes, Fit};

const PAGE: usize = 40;

impl App {
    pub(crate) fn uncensored_window(&mut self, ctx: &egui::Context) {
        // on open: show the cached list, refresh it in the background when stale
        if self.show_uncensored && !self.unc.was_open {
            if self.unc.models.is_empty() {
                if let Some((models, at)) = discover::load_cache() {
                    self.unc.models = models;
                    self.unc.fetched_at = at;
                }
            }
            let age = now_secs().saturating_sub(self.unc.fetched_at);
            if self.unc.models.is_empty() || age > discover::CACHE_TTL_SECS {
                self.discover();
            }
            if self.unc.limit == 0 {
                self.unc.limit = PAGE;
            }
        }
        self.unc.was_open = self.show_uncensored;

        let mut open = self.show_uncensored;
        egui::Window::new("Uncensored models")
            .open(&mut open)
            .default_size([720.0, 600.0])
            .collapsible(false)
            .show(ctx, |ui| {
                self.runtime_notice(ui);
                self.system_summary(ui);
                ui.separator();
                self.uncensored_header(ui);
                ui.separator();
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    if self.unc.models.is_empty() {
                        // never fetched and offline: built-in list
                        if !self.unc.discovering {
                            ui.label(RichText::new("Built-in list (ollama.com unreachable):").small().color(Color32::GRAY));
                            for c in UNCENSORED {
                                self.catalog_row(ui, c);
                            }
                        }
                        return;
                    }
                    let shown = self.filtered();
                    if shown.is_empty() {
                        ui.label(RichText::new("No model matches the filters.").color(Color32::GRAY));
                    }
                    for (m, sizes) in shown.iter().take(self.unc.limit) {
                        self.remote_row(ui, m, sizes);
                    }
                    if shown.len() > self.unc.limit {
                        ui.vertical_centered(|ui| {
                            if ui.button(format!("Show more ({} left)", shown.len() - self.unc.limit)).clicked() {
                                self.unc.limit += PAGE;
                            }
                        });
                    }
                });
            });
        self.show_uncensored = open;
    }

    fn uncensored_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.unc.filter).hint_text("Filter by name or description").desired_width(240.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.unc.discovering {
                    ui.spinner();
                    ui.label(RichText::new("Searching ollama.com…").small().color(Color32::GRAY));
                } else if ui.small_button("⟳ Refresh").on_hover_text("Search ollama.com again").clicked() {
                    self.discover();
                }
            });
        });
        self.filters_ui(ui);
        let official = self.unc.models.iter().filter(|m| m.official).count();
        let mut line = format!(
            "{} shown · {} models ({} official, {} community)",
            self.filtered().len(),
            self.unc.models.len(),
            official,
            self.unc.models.len() - official
        );
        if self.unc.fetched_at > 0 {
            line.push_str(&format!(" · updated {}", ago(now_secs().saturating_sub(self.unc.fetched_at))));
        }
        ui.label(RichText::new(line).small().color(Color32::GRAY));
        if let Some(e) = &self.unc.error {
            ui.label(RichText::new(format!("⚠ Refresh failed, showing the previous list: {e}")).small().color(Color32::from_rgb(220, 90, 90)));
        }
        ui.label(
            RichText::new(
                "Weights only (no executable code), downloaded by Ollama and checked by SHA-256. \
                 Community models are published by anyone and not reviewed by Ollama. \
                 Uncensored output can be wrong, offensive or harmful.",
            )
            .small()
            .color(Color32::from_rgb(220, 180, 60)),
        );
    }

    pub(crate) fn discover(&mut self) {
        if self.unc.discovering {
            return;
        }
        self.unc.discovering = true;
        let sink = self.sink.clone();
        std::thread::spawn(move || {
            let result = discover::fetch_all(&discover::client());
            sink.send(Event::Discovered(result));
        });
    }

    /// Registry details of `tag`, fetched once in the background.
    fn remote_details(&mut self, tag: &str) -> Option<Result<discover::Details, String>> {
        if let Some(d) = self.unc.details.get(tag) {
            return d.clone();
        }
        self.unc.details.insert(tag.to_string(), None);
        let sink = self.sink.clone();
        let tag = tag.to_string();
        std::thread::spawn(move || {
            let result = discover::details(&discover::client(), &tag);
            sink.send(Event::RemoteDetails { tag, result });
        });
        None
    }

    fn filters_ui(&mut self, ui: &mut egui::Ui) {
        let f = &mut self.unc.filters;
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("unc_source")
                .selected_text(match f.source {
                    Source::All => "All sources",
                    Source::Official => "Official only",
                    Source::Community => "Community only",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut f.source, Source::All, "All sources");
                    ui.selectable_value(&mut f.source, Source::Official, "Official only")
                        .on_hover_text("Published by Ollama in ollama.com/library");
                    ui.selectable_value(&mut f.source, Source::Community, "Community only");
                });
            let max_label = |m: Option<f64>| m.map(|b| format!("≤ {b}B")).unwrap_or_else(|| "Any size".into());
            egui::ComboBox::from_id_salt("unc_max").selected_text(max_label(f.max_params)).show_ui(ui, |ui| {
                for m in [None, Some(3.0), Some(8.0), Some(14.0), Some(32.0), Some(70.0)] {
                    ui.selectable_value(&mut f.max_params, m, max_label(m));
                }
            });
            egui::ComboBox::from_id_salt("unc_sort")
                .selected_text(match f.sort {
                    SortBy::Downloads => "Most downloaded",
                    SortBy::Name => "Name",
                    SortBy::Smallest => "Smallest first",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut f.sort, SortBy::Downloads, "Most downloaded");
                    ui.selectable_value(&mut f.sort, SortBy::Name, "Name");
                    ui.selectable_value(&mut f.sort, SortBy::Smallest, "Smallest first");
                });
            ui.checkbox(&mut f.fits, "Fits my machine")
                .on_hover_text("Estimated from the parameter count until the exact size is loaded");
            ui.checkbox(&mut f.vision, "Vision");
            ui.checkbox(&mut f.tools, "Tools");
            ui.checkbox(&mut f.thinking, "Thinking");
            ui.checkbox(&mut f.known_size, "Known size only")
                .on_hover_text("Many community models do not list their size on ollama.com");
            if *f != UncensoredFilters::default() && ui.small_button("Reset").clicked() {
                *f = UncensoredFilters::default();
            }
        });
    }

    /// Models passing the filters, each with the size tags it can be pulled in.
    fn filtered(&self) -> Vec<(RemoteModel, Vec<String>)> {
        let f = &self.unc.filters;
        let text = self.unc.filter.trim().to_lowercase();
        // while the hardware probe runs the memory is unknown: don't hide anything
        let fits = |bytes: u64| self.probing || matches!(Fit::assess(bytes, &self.sys), Fit::Gpu | Fit::Cpu);
        let mut out: Vec<(RemoteModel, Vec<String>, Option<f64>)> = Vec::new();
        for m in &self.unc.models {
            match f.source {
                Source::Official if !m.official => continue,
                Source::Community if m.official => continue,
                _ => {}
            }
            if !text.is_empty() && !m.name.to_lowercase().contains(&text) && !m.description.to_lowercase().contains(&text) {
                continue;
            }
            let details = match self.unc.details.get(&m.name) {
                Some(Some(Ok(d))) => Some(d.clone()),
                _ => None,
            };
            let has = |c: &str| m.capabilities.iter().any(|x| x == c);
            if f.vision && !(has("vision") || details.as_ref().is_some_and(|d| d.vision)) {
                continue;
            }
            if (f.tools && !has("tools")) || (f.thinking && !has("thinking")) {
                continue;
            }
            let params = m.params();
            let (sizes, smallest) = if params.is_empty() {
                // no size tag on the site: use the registry size once loaded
                match &details {
                    Some(d) => {
                        let approx = d.size as f64 / (0.6 * crate::system::GB);
                        if f.max_params.is_some_and(|max| approx > max * 1.15) || (f.fits && !fits(d.size)) {
                            continue;
                        }
                        (Vec::new(), Some(approx))
                    }
                    None if f.known_size => continue,
                    None => (Vec::new(), None),
                }
            } else {
                let ok: Vec<_> = params
                    .into_iter()
                    .filter(|(_, p)| f.max_params.is_none_or(|max| *p <= max))
                    .filter(|(_, p)| !f.fits || fits(discover::estimated_bytes(*p)))
                    .collect();
                if ok.is_empty() {
                    continue;
                }
                let smallest = ok.first().map(|x| x.1);
                (ok.into_iter().map(|x| x.0).collect(), smallest)
            };
            out.push((m.clone(), sizes, smallest));
        }
        match f.sort {
            SortBy::Downloads => {} // already ordered (official first, then downloads)
            SortBy::Name => out.sort_by(|a, b| a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase())),
            SortBy::Smallest => out.sort_by(|a, b| match (a.2, b.2) {
                (Some(x), Some(y)) => x.total_cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }),
        }
        out.into_iter().map(|(m, s, _)| (m, s)).collect()
    }

    /// `sizes`: the size tags allowed by the filters (empty if the site lists none).
    fn remote_row(&mut self, ui: &mut egui::Ui, m: &RemoteModel, sizes: &[String]) {
        // keep the user's pick if still allowed, else the largest allowed size
        let size = self.unc.size.get(&m.name).filter(|s| sizes.contains(s)).cloned().or_else(|| sizes.last().cloned());
        let tag = match &size {
            Some(s) => format!("{}:{s}", m.name),
            None => m.name.clone(),
        };
        let details = self.remote_details(&tag);
        let url = m.url();
        egui::Frame::new().fill(Color32::from_rgb(20, 20, 24)).inner_margin(8.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let shown: String = m.name.chars().take(48).collect();
                ui.label(RichText::new(shown).strong().size(15.0)).on_hover_text(&m.name);
                let (badge, color) = if m.official {
                    ("official", Color32::from_rgb(90, 200, 120))
                } else {
                    ("community", Color32::from_rgb(220, 180, 60))
                };
                ui.label(RichText::new(badge).small().color(color));
                if let Some(n) = m.downloads {
                    ui.label(RichText::new(format!("↓ {}", short_count(n))).small().color(Color32::GRAY));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let (Some(Ok(d)), false) = (&details, self.probing) {
                        Self::fit_badge(ui, Fit::assess(d.size, &self.sys));
                    }
                });
            });
            if !m.description.is_empty() {
                let d: String = m.description.chars().take(220).collect();
                let d = if m.description.chars().count() > 220 { format!("{d}…") } else { d };
                ui.label(RichText::new(d).color(Color32::from_gray(190)));
            }
            ui.horizontal(|ui| {
                if sizes.len() > 1 {
                    let mut pick = size.clone().unwrap_or_default();
                    egui::ComboBox::from_id_salt(("unc_size", &m.name)).selected_text(&pick).width(70.0).show_ui(ui, |ui| {
                        for s in sizes {
                            ui.selectable_value(&mut pick, s.clone(), s);
                        }
                    });
                    if Some(&pick) != size.as_ref() {
                        self.unc.size.insert(m.name.clone(), pick);
                    }
                } else if let Some(s) = &size {
                    ui.label(RichText::new(s).small().monospace());
                }
                let info = match &details {
                    None => "loading details…".to_string(),
                    Some(Err(e)) => format!("details unavailable ({e})"),
                    Some(Ok(d)) => {
                        let mut parts = vec![fmt_bytes(d.size)];
                        if let Some(c) = d.context_length {
                            parts.push(format!("{}K context", c / 1024));
                        }
                        if d.vision || m.capabilities.iter().any(|c| c == "vision") {
                            parts.push("vision".into());
                        }
                        parts.join(" · ")
                    }
                };
                ui.label(RichText::new(info).small().color(Color32::GRAY));
            });
            ui.horizontal(|ui| {
                ui.hyperlink_to(RichText::new(&url).small().monospace(), &url);
                self.copy_button(ui, &url);
            });
            ui.horizontal(|ui| {
                if self.is_installed(&tag) {
                    ui.label(RichText::new("✓ Installed").color(Color32::from_rgb(90, 200, 120)));
                } else if self.pulls.contains_key(&tag) || self.errors.contains_key(&tag) {
                    self.pull_status(ui, &tag);
                } else {
                    let online = matches!(self.status, RuntimeStatus::Online { .. });
                    let label = match &details {
                        Some(Ok(d)) if matches!(Fit::assess(d.size, &self.sys), Fit::TooBig | Fit::NoDisk) => "Download anyway",
                        _ => "Download",
                    };
                    if ui.add_enabled(online, egui::Button::new(label)).on_hover_text(format!("ollama pull {tag}")).clicked() {
                        self.start_pull(&tag);
                    }
                }
            });
        });
        ui.add_space(6.0);
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn ago(secs: u64) -> String {
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

fn short_count(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..1_000_000 => format!("{:.1}K", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}
