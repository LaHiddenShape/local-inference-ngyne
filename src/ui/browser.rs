//! Live model list from ollama.com with filters, shared by the Models window
//! (official library) and the Uncensored window.

use eframe::egui::{self, Color32, RichText};

use crate::app::{App, BrowserState, Filters, SortBy, Source};
use crate::backend::{Event, RuntimeStatus};
use crate::discover::{self, ListKind, RemoteModel};
use crate::system::{fmt_bytes, Fit};

const PAGE: usize = 40;

impl App {
    pub(crate) fn browser(&self, kind: ListKind) -> &BrowserState {
        match kind {
            ListKind::Library => &self.library,
            ListKind::Uncensored => &self.unc,
        }
    }

    pub(crate) fn browser_mut(&mut self, kind: ListKind) -> &mut BrowserState {
        match kind {
            ListKind::Library => &mut self.library,
            ListKind::Uncensored => &mut self.unc,
        }
    }

    /// Call every frame with the window's open flag: on opening, shows the
    /// cached list and refreshes it in the background when stale.
    pub(crate) fn browser_opened(&mut self, kind: ListKind, open: bool) {
        let b = self.browser_mut(kind);
        let just_opened = open && !b.was_open;
        b.was_open = open;
        if !just_opened {
            return;
        }
        if b.models.is_empty() {
            if let Some((models, at)) = discover::load_cache(kind) {
                b.models = models;
                b.fetched_at = at;
            }
        }
        if b.limit == 0 {
            b.limit = PAGE;
        }
        let stale = now_secs().saturating_sub(b.fetched_at) > discover::CACHE_TTL_SECS;
        if b.models.is_empty() || stale {
            self.discover(kind);
        }
    }

    pub(crate) fn discover(&mut self, kind: ListKind) {
        let b = self.browser_mut(kind);
        if b.discovering {
            return;
        }
        b.discovering = true;
        let sink = self.sink.clone();
        std::thread::spawn(move || {
            let result = discover::fetch(&discover::client(), kind);
            sink.send(Event::Discovered { kind, result });
        });
    }

    /// Search box, refresh button, filters and status line.
    pub(crate) fn browser_header(&mut self, ui: &mut egui::Ui, kind: ListKind) {
        ui.horizontal(|ui| {
            let b = self.browser_mut(kind);
            ui.add(egui::TextEdit::singleline(&mut b.filter).hint_text("Filter by name or description").desired_width(240.0));
            let discovering = b.discovering;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if discovering {
                    ui.spinner();
                    ui.label(RichText::new("Fetching ollama.com…").small().color(Color32::GRAY));
                } else if ui.small_button("⟳ Refresh").on_hover_text("Fetch the list from ollama.com again").clicked() {
                    self.discover(kind);
                }
            });
        });
        self.filters_ui(ui, kind);
        let b = self.browser(kind);
        let total = b.models.len();
        let mut line = format!("{} shown · {total} models", self.filtered(kind).len());
        if kind == ListKind::Uncensored {
            let official = b.models.iter().filter(|m| m.official).count();
            line.push_str(&format!(" ({official} official, {} community)", total - official));
        }
        if b.fetched_at > 0 {
            line.push_str(&format!(" · updated {}", ago(now_secs().saturating_sub(b.fetched_at))));
        }
        ui.label(RichText::new(line).small().color(Color32::GRAY));
        if let Some(e) = &b.error {
            let msg = if b.models.is_empty() {
                format!("⚠ Could not load the list: {e}")
            } else {
                format!("⚠ Refresh failed, showing the previous list: {e}")
            };
            ui.label(RichText::new(msg).small().color(Color32::from_rgb(220, 90, 90)));
        }
    }

    /// The filtered rows, paginated with "Show more".
    pub(crate) fn browser_list(&mut self, ui: &mut egui::Ui, kind: ListKind) {
        let b = self.browser(kind);
        if b.models.is_empty() {
            let msg = if b.discovering {
                "Loading the list from ollama.com…"
            } else {
                "No list available (ollama.com unreachable). You can still pull a model by name."
            };
            ui.label(RichText::new(msg).color(Color32::GRAY));
            return;
        }
        let limit = b.limit;
        let shown = self.filtered(kind);
        if shown.is_empty() {
            ui.label(RichText::new("No model matches the filters.").color(Color32::GRAY));
        }
        for (m, sizes) in shown.iter().take(limit) {
            self.remote_row(ui, kind, m, sizes);
        }
        if shown.len() > limit {
            ui.vertical_centered(|ui| {
                if ui.button(format!("Show more ({} left)", shown.len() - limit)).clicked() {
                    self.browser_mut(kind).limit += PAGE;
                }
            });
        }
    }

    /// Registry details of `tag`, fetched once in the background.
    fn remote_details(&mut self, tag: &str) -> Option<Result<discover::Details, String>> {
        if let Some(d) = self.remote_details.get(tag) {
            return d.clone();
        }
        self.remote_details.insert(tag.to_string(), None);
        let sink = self.sink.clone();
        let tag = tag.to_string();
        std::thread::spawn(move || {
            let result = discover::details(&discover::client(), &tag);
            sink.send(Event::RemoteDetails { tag, result });
        });
        None
    }

    fn filters_ui(&mut self, ui: &mut egui::Ui, kind: ListKind) {
        let f = &mut self.browser_mut(kind).filters;
        let id = |s: &str| (s.to_string(), kind);
        ui.horizontal_wrapped(|ui| {
            // the library is official only
            if kind == ListKind::Uncensored {
                egui::ComboBox::from_id_salt(id("source"))
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
            }
            let max_label = |m: Option<f64>| m.map(|b| format!("≤ {b}B")).unwrap_or_else(|| "Any size".into());
            egui::ComboBox::from_id_salt(id("max")).selected_text(max_label(f.max_params)).show_ui(ui, |ui| {
                for m in [None, Some(3.0), Some(8.0), Some(14.0), Some(32.0), Some(70.0)] {
                    ui.selectable_value(&mut f.max_params, m, max_label(m));
                }
            });
            egui::ComboBox::from_id_salt(id("sort"))
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
                .on_hover_text("Some models do not list their size on ollama.com");
            if *f != Filters::default() && ui.small_button("Reset").clicked() {
                *f = Filters::default();
            }
        });
    }

    /// Models passing the filters, each with the size tags it can be pulled in.
    fn filtered(&self, kind: ListKind) -> Vec<(RemoteModel, Vec<String>)> {
        let b = self.browser(kind);
        let f = &b.filters;
        let text = b.filter.trim().to_lowercase();
        // while the hardware probe runs the memory is unknown: don't hide anything
        let fits = |bytes: u64| self.probing || matches!(Fit::assess(bytes, &self.sys), Fit::Gpu | Fit::Cpu);
        let mut out: Vec<(RemoteModel, Vec<String>, Option<f64>)> = Vec::new();
        for m in &b.models {
            match f.source {
                Source::Official if !m.official => continue,
                Source::Community if m.official => continue,
                _ => {}
            }
            if !text.is_empty() && !m.name.to_lowercase().contains(&text) && !m.description.to_lowercase().contains(&text) {
                continue;
            }
            let details = match self.remote_details.get(&m.name) {
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
            SortBy::Downloads => {} // already ordered by downloads
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
    fn remote_row(&mut self, ui: &mut egui::Ui, kind: ListKind, m: &RemoteModel, sizes: &[String]) {
        // keep the user's pick if still allowed, else the largest allowed size
        let picked = self.browser(kind).size.get(&m.name).filter(|s| sizes.contains(s)).cloned();
        let size = picked.or_else(|| sizes.last().cloned());
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
                if kind == ListKind::Uncensored {
                    let (badge, color) = if m.official {
                        ("official", Color32::from_rgb(90, 200, 120))
                    } else {
                        ("community", Color32::from_rgb(220, 180, 60))
                    };
                    ui.label(RichText::new(badge).small().color(color));
                }
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
                    egui::ComboBox::from_id_salt(("size", kind, &m.name)).selected_text(&pick).width(70.0).show_ui(ui, |ui| {
                        for s in sizes {
                            ui.selectable_value(&mut pick, s.clone(), s);
                        }
                    });
                    if Some(&pick) != size.as_ref() {
                        self.browser_mut(kind).size.insert(m.name.clone(), pick);
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

    /// Whether `tag` is one of the rows a list can show (to avoid listing its
    /// download progress twice).
    pub(crate) fn listed(&self, tag: &str) -> bool {
        let name = tag.split(':').next().unwrap_or(tag);
        self.library.models.iter().chain(&self.unc.models).any(|m| m.name == name)
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
