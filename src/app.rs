use crate::config::{self, SavedConfig};
use crate::draft::{self, DraftReport};
use crate::engine::{self, Creds, Msg, StageInfo};
use crate::evidence;
use crate::llm::{self, LlmConfig, LlmRequest, ProviderKind};
use crate::markdown;
use crate::model::Record;
use crate::plan::{self, PlanOutcome, ResearchPlan};
use crate::report;
use crate::secrets;
use crate::theme::{self, bold, palette, tint};
use eframe::egui;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Sources,
    Limits,
    Model,
    Account,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MainTab {
    Report,
    Evidence,
    Plan,
    History,
}

pub struct App {
    cfg: SavedConfig,
    password: String,
    results: Vec<Record>,
    filtered: Vec<usize>,
    filter: String,
    filter_sub: String,
    filter_kind: String,
    filter_sig: String,
    /// Record shown in the Evidence drawer.
    detail: Option<String>,
    /// True until the drawer body is placed at its top.
    detail_fresh: bool,
    running: bool,
    cancel: Arc<AtomicBool>,
    rx: Option<Receiver<Msg>>,
    auth_rx: Option<Receiver<Msg>>,
    testing: bool,
    auth_status: String,
    llm_key: String,
    llm_status: String,
    llm_testing: bool,
    llm_test_rx: Option<Receiver<Result<String, String>>>,
    plan: Option<ResearchPlan>,
    plan_source: String,
    plan_warning: Option<String>,
    plan_status: String,
    plan_running: bool,
    plan_rx: Option<Receiver<PlanOutcome>>,
    plan_query: String,
    pending_run: bool,
    draft: Option<DraftReport>,
    /// `report.md`, shown in the Markdown preview.
    report_md: Option<String>,
    /// True when the model wrote the report.
    report_ai: bool,
    /// Bumped when a new report loads, so its preview starts at the top.
    report_epoch: u64,
    /// Show the extractive draft instead of the Markdown report.
    show_excerpts: bool,
    stages: Vec<StageInfo>,
    main_tab: MainTab,
    log: VecDeque<String>,
    /// Saved runs shown in the History tab, newest first.
    history: Vec<RunEntry>,
    /// True after the History tab reads the run folders.
    history_loaded: bool,
    requests: usize,
    status: String,
    last_dir: Option<PathBuf>,
    dark_last: bool,
    settings_open: bool,
    settings_tab: SettingsTab,
    log_open: bool,
    #[cfg(target_os = "windows")]
    hwnd: isize,
    #[cfg(target_os = "windows")]
    titlebar_dark: Option<bool>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let cfg = config::load();
        let dark = cfg.dark;
        let llm_key = secrets::load(&secrets::target(&cfg.llm_provider)).unwrap_or_default();
        theme::install_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, dark);
        let mut app = Self {
            cfg,
            password: String::new(),
            results: Vec::new(),
            filtered: Vec::new(),
            filter: String::new(),
            filter_sub: String::new(),
            filter_kind: String::new(),
            filter_sig: String::new(),
            detail: None,
            detail_fresh: false,
            running: false,
            cancel: Arc::new(AtomicBool::new(false)),
            rx: None,
            auth_rx: None,
            testing: false,
            auth_status: String::new(),
            llm_key,
            llm_status: String::new(),
            llm_testing: false,
            llm_test_rx: None,
            plan: None,
            plan_source: String::new(),
            plan_warning: None,
            plan_status: String::new(),
            plan_running: false,
            plan_rx: None,
            plan_query: String::new(),
            pending_run: false,
            draft: None,
            report_md: None,
            report_ai: false,
            report_epoch: 0,
            show_excerpts: false,
            stages: engine::initial_stages(),
            main_tab: MainTab::Report,
            log: VecDeque::new(),
            history: Vec::new(),
            history_loaded: false,
            requests: 0,
            status: "Ask a question, then build a plan or run archive search.".to_string(),
            last_dir: None,
            dark_last: dark,
            settings_open: false,
            settings_tab: SettingsTab::Sources,
            log_open: false,
            #[cfg(target_os = "windows")]
            hwnd: 0,
            #[cfg(target_os = "windows")]
            titlebar_dark: None,
        };
        app.load_last(false);
        app
    }

    fn push_log(&mut self, line: String) {
        self.log.push_back(line);
        if self.log.len() > 300 {
            self.log.pop_front();
        }
    }

    fn account_configured(&self) -> bool {
        !self.cfg.client_id.trim().is_empty()
            && !self.cfg.client_secret.trim().is_empty()
            && !self.cfg.username.trim().is_empty()
    }

    fn account_ready(&self) -> bool {
        self.account_configured() && !self.password.is_empty()
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.rx {
            let mut messages = Vec::new();
            while let Ok(message) = rx.try_recv() {
                messages.push(message);
            }
            for message in messages {
                self.handle(message);
            }
        }
        if let Some(rx) = &self.auth_rx {
            let mut last = None;
            while let Ok(message) = rx.try_recv() {
                last = Some(message);
            }
            if let Some(message) = last {
                if let Msg::Auth(result) = message {
                    self.testing = false;
                    self.auth_status = match result {
                        Ok(text) => text,
                        Err(error) => format!("Failed: {}", error),
                    };
                }
            }
        }
        if let Some(rx) = self.llm_test_rx.take() {
            match rx.try_recv() {
                Ok(result) => {
                    self.llm_testing = false;
                    self.llm_status = match result {
                        Ok(text) => text,
                        Err(error) => format!("Failed: {}", error),
                    };
                }
                Err(TryRecvError::Empty) => self.llm_test_rx = Some(rx),
                Err(TryRecvError::Disconnected) => {
                    self.llm_testing = false;
                    self.llm_status = "Failed: the test thread stopped.".to_string();
                }
            }
        }
        if let Some(rx) = self.plan_rx.take() {
            match rx.try_recv() {
                Ok(outcome) => {
                    let PlanOutcome {
                        plan,
                        source,
                        ai_used,
                        warning,
                        warnings,
                    } = outcome;
                    self.plan_running = false;
                    self.plan_status = match &warning {
                        Some(note) if ai_used => format!("{} (note: {})", source, note),
                        Some(note) => format!("{} ({})", source, note),
                        None => source.clone(),
                    };
                    self.push_log(format!(
                        "[plan] {} · {} sub-questions · {} subreddits",
                        source,
                        plan.sub_questions.len(),
                        plan.subreddits.len()
                    ));
                    if let Some(note) = &warning {
                        self.push_log(format!("[plan] note: {}", note));
                    }
                    for note in &warnings {
                        self.push_log(format!("[plan] warning: {}", note));
                    }
                    self.plan = Some(plan);
                    self.plan_source = source;
                    self.plan_warning = warning;
                    if self.pending_run {
                        self.pending_run = false;
                        self.start_run();
                    }
                }
                Err(TryRecvError::Empty) => self.plan_rx = Some(rx),
                Err(TryRecvError::Disconnected) => {
                    self.plan_running = false;
                    self.plan_status = "Planning stopped without a result.".to_string();
                    if self.pending_run {
                        self.pending_run = false;
                        self.status = "Planning stopped. Run research to retry.".to_string();
                    }
                }
            }
        }
    }

    fn handle(&mut self, message: Msg) {
        match message {
            Msg::Log(line) => self.push_log(line),
            Msg::Progress { total, requests } => {
                self.requests = requests;
                if self.running {
                    self.status = format!("Running... {} records, {} requests", total, requests);
                }
            }
            Msg::Stage(stage) => {
                if let Some(slot) = self.stages.iter_mut().find(|item| item.name == stage.name) {
                    *slot = stage;
                } else {
                    self.stages.push(stage);
                }
            }
            Msg::Auth(_) => {}
            Msg::Done {
                records,
                total,
                dir,
                error,
                draft,
                report_markdown,
                report_ai,
                stages,
            } => {
                self.running = false;
                self.pending_run = false;
                self.results = records;
                self.detail = None;
                self.draft = draft;
                self.report_md = report_markdown;
                self.report_ai = report_ai;
                self.report_epoch = self.report_epoch.wrapping_add(1);
                self.show_excerpts = false;
                if !stages.is_empty() {
                    self.stages = stages;
                }
                self.main_tab = if self.report_md.is_some() || self.draft.is_some() {
                    MainTab::Report
                } else {
                    MainTab::Evidence
                };
                self.filter_sig.clear();
                self.last_dir = Some(PathBuf::from(&dir));
                self.status = match &error {
                    Some(err) => format!(
                        "Stopped: {}. {} records saved with a partial report.",
                        err, total
                    ),
                    None if self.report_md.is_some() => {
                        if self.report_ai {
                            format!("Done. {} records. Cited report ready.", self.results.len())
                        } else {
                            format!("Done. {} records. Draft report ready.", self.results.len())
                        }
                    }
                    None => format!("Done. {} records. Report ready.", self.results.len()),
                };
                self.push_log(self.status.clone());
                self.save_log();
                self.history_loaded = false;
                config::save(&self.cfg);
            }
        }
    }

    fn start_run(&mut self) {
        let topic = self.cfg.topic.trim().to_string();
        if topic.is_empty() {
            self.status = "Enter a topic first.".to_string();
            return;
        }
        if self.plan_running {
            self.pending_run = true;
            self.status = "Building the plan before collection...".to_string();
            return;
        }
        let plan_fresh = self.plan.is_some() && self.plan_query == topic;
        if !plan_fresh {
            self.pending_run = true;
            self.status = "Building a plan for this question...".to_string();
            self.build_plan();
            return;
        }
        let mut settings = engine::Settings::from_config(&self.cfg, self.password.clone());
        settings.plan = self.plan.clone();
        settings.plan_source = self.plan_source.clone();
        settings.llm = Some(LlmConfig::from_saved(&self.cfg, self.llm_key.clone()));
        if !settings.any_tactic() {
            self.pending_run = false;
            self.status =
                "Select at least one tactic. Archive tactics need no account.".to_string();
            return;
        }
        self.pending_run = false;
        self.running = true;
        self.cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.results.clear();
        self.detail = None;
        self.draft = None;
        self.report_md = None;
        self.report_ai = false;
        self.show_excerpts = false;
        self.filtered.clear();
        self.filter_sig.clear();
        self.log.clear();
        self.requests = 0;
        self.last_dir = None;
        self.stages = engine::initial_stages();
        self.main_tab = MainTab::Report;
        self.status = "Starting planned collection...".to_string();
        config::save(&self.cfg);
        engine::spawn(settings, tx, self.cancel.clone());
    }

    fn test_login(&mut self) {
        if !self.account_ready() {
            self.auth_status = "Fill in client ID, secret, username, and password.".to_string();
            return;
        }
        let creds = Creds {
            client_id: self.cfg.client_id.trim().to_string(),
            client_secret: self.cfg.client_secret.trim().to_string(),
            username: self.cfg.username.trim().to_string(),
            password: self.password.clone(),
        };
        let (tx, rx) = channel();
        self.auth_rx = Some(rx);
        self.testing = true;
        self.auth_status = "Testing login...".to_string();
        std::thread::spawn(move || {
            let result = engine::test_account(creds);
            let _ = tx.send(Msg::Auth(result));
        });
    }

    fn build_plan(&mut self) {
        let query = self.cfg.topic.trim().to_string();
        if query.is_empty() {
            self.status = "Enter a research question first.".to_string();
            return;
        }
        if self.plan_running {
            return;
        }
        self.persist_key();
        self.plan_query = query.clone();
        let config = LlmConfig::from_saved(&self.cfg, self.llm_key.clone());
        let subs = engine::parse_subs(&self.cfg.subs);
        let (tx, rx) = channel();
        self.plan_rx = Some(rx);
        self.plan_running = true;
        self.plan_status = if config.is_ready() {
            format!("Planning with {}...", config.provider.label())
        } else {
            "Building a local plan...".to_string()
        };
        self.push_log(format!("[plan] question: {}", query));
        std::thread::spawn(move || {
            let outcome = plan::build(config, &query, &subs);
            let _ = tx.send(outcome);
        });
    }

    fn test_model(&mut self) {
        if self.llm_testing {
            return;
        }
        let config = LlmConfig::from_saved(&self.cfg, self.llm_key.clone());
        if !config.is_ready() {
            self.llm_status =
                "Fill in the base URL, model, and API key. Ollama needs no key.".to_string();
            return;
        }
        self.persist_key();
        let (tx, rx) = channel();
        self.llm_test_rx = Some(rx);
        self.llm_testing = true;
        self.llm_status = "Testing...".to_string();
        std::thread::spawn(move || {
            let result = match llm::client(&config) {
                Ok(provider) => {
                    let request = LlmRequest {
                        system: "Reply with JSON only: {\"ok\":true}".to_string(),
                        user: "ping".to_string(),
                        json: true,
                        max_tokens: 32,
                        temperature: 0.0,
                    };
                    match provider.complete(&request) {
                        Ok(response) => {
                            let model = if response.model.trim().is_empty() {
                                config.model.clone()
                            } else {
                                response.model.clone()
                            };
                            Ok(format!(
                                "OK · {} · {} ms · {} tokens",
                                model,
                                response.latency_ms,
                                response.usage.total()
                            ))
                        }
                        Err(error) => Err(error.to_string()),
                    }
                }
                Err(error) => Err(error.to_string()),
            };
            let _ = tx.send(result);
        });
    }

    fn persist_key(&mut self) {
        let target = secrets::target(&self.cfg.llm_provider);
        let key = self.llm_key.trim();
        let result = if key.is_empty() {
            secrets::delete(&target)
        } else {
            secrets::save(&target, key)
        };
        if let Err(error) = result {
            self.llm_status = format!("Cannot store the API key: {}", error);
        }
    }

    fn plan_hint(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        if self.plan_running {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new("Building plan...")
                        .size(11.0)
                        .color(p.muted),
                );
            });
            return;
        }
        let provider = ProviderKind::from_id(&self.cfg.llm_provider);
        let configured = LlmConfig::from_saved(&self.cfg, self.llm_key.clone()).is_ready();
        let line = if configured {
            format!(
                "Plan: {} · {}",
                provider.label(),
                if self.cfg.llm_model.trim().is_empty() {
                    "default model"
                } else {
                    self.cfg.llm_model.trim()
                }
            )
        } else if provider.needs_key() {
            "Plan: local fallback. Add an AI key for a richer plan.".to_string()
        } else {
            "Plan: the local model is not ready.".to_string()
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(line).size(10.5).color(p.faint));
            if ui.link(egui::RichText::new("Set up").size(10.5)).clicked() {
                self.settings_tab = SettingsTab::Model;
                self.settings_open = true;
            }
        });
        if !self.plan_status.is_empty() {
            ui.label(
                egui::RichText::new(&self.plan_status)
                    .size(10.5)
                    .color(p.muted),
            );
        }
    }

    fn load_last_run(&mut self) {
        self.load_last(true);
    }

    fn load_last(&mut self, announce_errors: bool) {
        let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
        if let Ok(entries) = std::fs::read_dir(config::runs_dir()) {
            for entry in entries.flatten() {
                let path = entry.path();
                let candidate = path.join("results.json");
                if let Ok(metadata) = std::fs::metadata(&candidate) {
                    if let Ok(modified) = metadata.modified() {
                        if newest
                            .as_ref()
                            .map(|(time, _)| modified > *time)
                            .unwrap_or(true)
                        {
                            newest = Some((modified, path));
                        }
                    }
                }
            }
        }
        let Some((_, dir)) = newest else {
            if announce_errors {
                self.status = "No saved runs yet.".to_string();
            }
            return;
        };
        self.load_dir(&dir, announce_errors);
    }

    /// Load one saved run into the app. Returns true when the run loaded.
    fn load_dir(&mut self, dir: &std::path::Path, announce_errors: bool) -> bool {
        let text = match std::fs::read_to_string(dir.join("results.json")) {
            Ok(text) => text,
            Err(error) => {
                if announce_errors {
                    self.status = format!("Cannot read the saved run: {}", error);
                }
                return false;
            }
        };
        let payload: serde_json::Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                if announce_errors {
                    self.status = format!("Cannot parse the saved run: {}", error);
                }
                return false;
            }
        };
        if let Some(topic) = payload.get("topic").and_then(|value| value.as_str()) {
            if !topic.is_empty() {
                self.cfg.topic = topic.to_string();
            }
        }
        let mut records = payload
            .get("records")
            .cloned()
            .and_then(|value| serde_json::from_value::<Vec<Record>>(value).ok())
            .unwrap_or_default();
        let plan = payload
            .get("plan")
            .cloned()
            .and_then(|value| serde_json::from_value::<ResearchPlan>(value).ok())
            .or_else(|| {
                std::fs::read_to_string(dir.join("plan.json"))
                    .ok()
                    .and_then(|text| serde_json::from_str::<ResearchPlan>(&text).ok())
            });
        let plan_source = payload
            .get("plan_source")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let topic = payload
            .get("topic")
            .and_then(|value| value.as_str())
            .filter(|text| !text.trim().is_empty())
            .unwrap_or(&self.cfg.topic)
            .to_string();
        // Older runs have no rank scores. Score them now so the Evidence tab stays useful.
        // Saved runs keep date order, so put them back into rank order for the tab.
        if !records.is_empty() && records.iter().all(|record| record.final_score == 0.0) {
            evidence::prepare(&mut records, plan.as_ref(), &topic);
        } else {
            evidence::sort_ranked(&mut records);
        }
        let mut draft = payload
            .get("draft")
            .cloned()
            .and_then(|value| serde_json::from_value::<DraftReport>(value).ok())
            .or_else(|| {
                std::fs::read_to_string(dir.join("draft-report.json"))
                    .ok()
                    .and_then(|text| serde_json::from_str::<DraftReport>(&text).ok())
            });
        let mut stages = payload
            .get("stages")
            .cloned()
            .and_then(|value| serde_json::from_value::<Vec<StageInfo>>(value).ok())
            .unwrap_or_default();
        if stages.is_empty() {
            stages = saved_stages(plan.as_ref(), draft.as_ref(), records.len());
        } else {
            for stage in &mut stages {
                if stage.status == "running" || stage.status == "pending" {
                    stage.status = if stage.name == "Review" {
                        "skipped"
                    } else {
                        "done"
                    }
                    .to_string();
                }
            }
        }
        if draft.is_none() {
            let pack = evidence::pack(&records, evidence::PACK_LIMIT);
            draft = Some(draft::build(
                plan.as_ref(),
                &topic,
                &pack,
                &plan_source,
                &[],
            ));
        }
        self.plan = plan;
        self.plan_source = if plan_source.trim().is_empty() {
            "Saved run".to_string()
        } else {
            plan_source
        };
        self.plan_warning = None;
        self.plan_query = self
            .plan
            .as_ref()
            .map(|plan| plan.question.clone())
            .unwrap_or_default();
        self.draft = draft;
        self.report_md = std::fs::read_to_string(dir.join("report.md"))
            .ok()
            .filter(|text| !text.trim().is_empty());
        self.report_ai = dir.join("report.json").exists();
        self.report_epoch = self.report_epoch.wrapping_add(1);
        self.show_excerpts = false;
        self.stages = stages;
        self.main_tab = if self.report_md.is_some() || self.draft.is_some() {
            MainTab::Report
        } else {
            MainTab::Evidence
        };
        self.results = records;
        self.detail = None;
        self.filtered.clear();
        self.filter_sig.clear();
        self.last_dir = Some(dir.to_path_buf());
        self.restore_log(dir);
        self.status = format!("Loaded {} records from a saved run.", self.results.len());
        true
    }

    /// Show the log that belongs to a loaded run. Runs made before logs were
    /// saved have no file, so the box says so.
    fn restore_log(&mut self, dir: &std::path::Path) {
        self.log.clear();
        if let Ok(text) = std::fs::read_to_string(dir.join("run.log")) {
            for line in text.lines() {
                if !line.trim().is_empty() {
                    self.log.push_back(line.to_string());
                }
            }
        }
        if self.log.is_empty() {
            self.log
                .push_back("No saved log for this run.".to_string());
        }
    }

    /// Write the current log next to the run files, so History can show it later.
    fn save_log(&self) {
        let Some(dir) = self.last_dir.as_ref() else {
            return;
        };
        let text = self.log.iter().cloned().collect::<Vec<_>>().join("\n");
        let _ = std::fs::write(dir.join("run.log"), text);
    }

    fn export_report(&mut self) {
        let Some(dir) = self.last_dir.clone() else {
            self.status = "Run a search first.".to_string();
            return;
        };
        let path = dir.join("report.html");
        match report::write_report(&self.results, &self.cfg.topic, &path) {
            Ok(()) => {
                let _ = open::that(&path);
                self.status = format!("Report opened: {}", path.display());
            }
            Err(error) => self.status = format!("Report failed: {}", error),
        }
    }

    fn export_draft(&mut self) {
        let Some(preview) = self.draft.clone() else {
            self.status = "Run research first to build a draft.".to_string();
            return;
        };
        let Some(dir) = self.last_dir.clone() else {
            self.status = "Run a search first.".to_string();
            return;
        };
        let path = dir.join("draft-report.md");
        match draft::write_markdown(&preview, &path) {
            Ok(()) => {
                let _ = open::that(&path);
                self.status = format!("Draft opened: {}", path.display());
            }
            Err(error) => self.status = format!("Draft failed: {}", error),
        }
    }

    fn copy_draft(&mut self, ctx: &egui::Context) {
        let Some(preview) = self.draft.as_ref() else {
            return;
        };
        ctx.copy_text(draft::to_markdown(preview));
        self.status = "Draft Markdown copied.".to_string();
    }

    fn refresh_filtered(&mut self) {
        let signature = format!(
            "{}|{}|{}|{}",
            self.filter,
            self.filter_sub,
            self.filter_kind,
            self.results.len()
        );
        if signature == self.filter_sig {
            return;
        }
        self.filter_sig = signature;
        let needle = self.filter.trim().to_lowercase();
        let mut filtered = Vec::new();
        for (index, record) in self.results.iter().enumerate() {
            if !self.filter_sub.is_empty() && record.subreddit != self.filter_sub {
                continue;
            }
            if !self.filter_kind.is_empty() && record.kind != self.filter_kind {
                continue;
            }
            if !needle.is_empty() && !record.search_text().contains(&needle) {
                continue;
            }
            filtered.push(index);
        }
        self.filtered = filtered;
    }

    fn subreddit_list(&self) -> Vec<String> {
        let mut subs: Vec<String> = self
            .results
            .iter()
            .map(|record| record.subreddit.clone())
            .filter(|sub| !sub.is_empty())
            .collect();
        subs.sort();
        subs.dedup();
        subs
    }

    #[cfg(target_os = "windows")]
    fn sync_title_bar(&mut self, ctx: &egui::Context) {
        if self.hwnd == 0 {
            match crate::win::main_window() {
                Some(hwnd) => self.hwnd = hwnd,
                None => {
                    ctx.request_repaint_after(Duration::from_millis(150));
                    return;
                }
            }
        }
        if self.titlebar_dark != Some(self.cfg.dark) {
            let p = palette(self.cfg.dark);
            crate::win::style_title_bar(self.hwnd, self.cfg.dark, p.panel, p.text, p.border);
            self.titlebar_dark = Some(self.cfg.dark);
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn sync_title_bar(&mut self, _ctx: &egui::Context) {}

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 9, p.accent);
            let mark =
                ui.painter()
                    .layout_no_wrap("R".to_owned(), theme::bold_font(15.0), p.on_accent);
            ui.painter()
                .galley(rect.center() - mark.size() / 2.0, mark, p.on_accent);
            ui.add_space(2.0);
            ui.vertical(|ui| {
                ui.label(bold("Reddit Research", 15.0).color(p.text));
                ui.label(
                    egui::RichText::new("Archive and API search")
                        .size(10.5)
                        .color(p.muted),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let theme_label = if self.cfg.dark { "Light" } else { "Dark" };
                if theme::ghost_button(ui, theme_label).clicked() {
                    self.cfg.dark = !self.cfg.dark;
                    config::save(&self.cfg);
                }
                if theme::ghost_button(ui, "Settings").clicked() {
                    self.settings_open = true;
                }
                if theme::ghost_button(ui, "Log").clicked() {
                    self.log_open = true;
                }
                if theme::ghost_button(ui, "Runs").clicked() {
                    let _ = open::that(config::runs_dir());
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.sidebar_inner(ui);
            });
    }

    fn sidebar_inner(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        ui.add_space(2.0);
        theme::section_label(ui, "Research question");
        let topic_response = ui.add(
            egui::TextEdit::multiline(&mut self.cfg.topic)
                .hint_text("Ask anything. For example: does armodafinil help with learning?")
                .desired_rows(3)
                .margin(egui::Margin::symmetric(10, 8))
                .desired_width(f32::INFINITY),
        );
        let run_shortcut = topic_response.has_focus()
            && ui.input(|input| input.key_pressed(egui::Key::Enter) && input.modifiers.command);
        if run_shortcut && !self.running && !self.plan_running {
            self.start_run();
        }

        ui.add_space(10.0);
        let width = ui.available_width();
        if self.running {
            if theme::danger_button(ui, "Cancel run", width).clicked() {
                self.cancel.store(true, Ordering::Relaxed);
                self.status = "Stopping...".to_string();
            }
            ui.add_space(6.0);
            let budget = self.cfg.max_requests.max(1) as f32;
            let fraction = (self.requests as f32 / budget).clamp(0.0, 1.0);
            ui.add(
                egui::ProgressBar::new(fraction)
                    .desired_width(width)
                    .fill(p.accent)
                    .text(format!("{} requests", self.requests)),
            );
        } else {
            if theme::primary_button(ui, "Run research", width).clicked() {
                self.start_run();
            }
            ui.add_space(6.0);
            if theme::secondary_button(ui, "Build research plan", width).clicked() {
                self.build_plan();
            }
            ui.add_space(6.0);
            self.plan_hint(ui);
        }

        ui.add_space(16.0);
        theme::section_label(ui, "Sources");
        self.tactic_chips(ui);

        ui.add_space(16.0);
        theme::section_label(ui, "Data");
        let sub_count = subreddit_count(&self.cfg.subs);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{} subreddits", sub_count))
                    .size(12.5)
                    .color(p.text),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.link(egui::RichText::new("Edit").size(12.0)).clicked() {
                    self.settings_tab = SettingsTab::Sources;
                    self.settings_open = true;
                }
            });
        });
        ui.add_space(6.0);
        ui.add_enabled_ui(self.draft.is_some() && !self.running, |ui| {
            if theme::secondary_button(ui, "Open draft report", width).clicked() {
                self.export_draft();
            }
        });
        ui.add_space(6.0);
        ui.add_enabled_ui(!self.results.is_empty() && !self.running, |ui| {
            if theme::secondary_button(ui, "Open evidence HTML", width).clicked() {
                self.export_report();
            }
        });
        ui.add_enabled_ui(self.last_dir.is_some() && !self.running, |ui| {
            if theme::secondary_button(ui, "Open run folder", width).clicked() {
                if let Some(dir) = &self.last_dir {
                    let _ = open::that(dir);
                }
            }
        });
        ui.add_enabled_ui(!self.running, |ui| {
            if theme::secondary_button(ui, "Load last run", width).clicked() {
                self.load_last_run();
            }
        });

        ui.add_space(18.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(
                "One account. Respectful rate limits. Public content only. Data belongs to its authors.",
            )
            .size(10.5)
            .color(p.faint),
        );
    }

    fn tactic_chips(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        let gap = 8.0;
        let width = ((ui.available_width() - gap) / 2.0).max(70.0);
        egui::Grid::new("archive_chips")
            .num_columns(2)
            .spacing(egui::vec2(gap, gap))
            .show(ui, |ui| {
                if theme::chip_sized(ui, "Posts", self.cfg.posts, true, width).clicked() {
                    self.cfg.posts = !self.cfg.posts;
                }
                if theme::chip_sized(ui, "Comments", self.cfg.comments, true, width).clicked() {
                    self.cfg.comments = !self.cfg.comments;
                }
                ui.end_row();
                if theme::chip_sized(ui, "Threads", self.cfg.threads, true, width).clicked() {
                    self.cfg.threads = !self.cfg.threads;
                }
                if theme::chip_sized(ui, "Authors", self.cfg.profiles, true, width).clicked() {
                    self.cfg.profiles = !self.cfg.profiles;
                }
                ui.end_row();
            });

        ui.add_space(8.0);
        if theme::chip_sized(
            ui,
            "All Reddit",
            self.cfg.all_reddit,
            true,
            ui.available_width(),
        )
        .clicked()
        {
            self.cfg.all_reddit = !self.cfg.all_reddit;
        }
        ui.label(
            egui::RichText::new(
                "Every subreddit, through the PullPush archive. The seed subreddits are searched too.",
            )
            .size(10.5)
            .color(p.faint),
        );

        ui.add_space(10.0);
        let ready = self.account_ready();
        let configured = self.account_configured();
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Reddit API")
                    .font(theme::bold_font(11.0))
                    .color(p.muted),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (text, color) = if ready {
                    ("Connected", p.success)
                } else if configured {
                    ("Password needed", p.post)
                } else {
                    ("Optional", p.faint)
                };
                ui.label(egui::RichText::new(text).size(10.5).color(color));
            });
        });
        egui::Grid::new("official_chips")
            .num_columns(2)
            .spacing(egui::vec2(gap, gap))
            .show(ui, |ui| {
                if theme::chip_sized(ui, "Search", self.cfg.official_search, ready, width).clicked()
                {
                    self.cfg.official_search = !self.cfg.official_search;
                }
                if theme::chip_sized(ui, "Threads", self.cfg.official_threads, ready, width)
                    .clicked()
                {
                    self.cfg.official_threads = !self.cfg.official_threads;
                }
            });
        if !ready {
            let hint = if configured {
                "Your credentials are saved. Enter your password in Settings."
            } else {
                "All-Reddit search works without an account. Connect one for fresh official results."
            };
            ui.add_space(4.0);
            ui.label(egui::RichText::new(hint).size(10.5).color(p.faint));
            if ui
                .link(egui::RichText::new("Open account settings").size(11.0))
                .clicked()
            {
                self.settings_tab = SettingsTab::Account;
                self.settings_open = true;
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        ui.horizontal(|ui| {
            let dot = if self.running {
                p.accent
            } else if self.results.is_empty() {
                p.faint
            } else {
                p.success
            };
            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 3.5, dot);
            ui.add_space(4.0);
            ui.label(egui::RichText::new(&self.status).size(11.5).color(p.muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} records | {} requests",
                        self.results.len(),
                        self.requests
                    ))
                    .size(10.5)
                    .color(p.faint),
                );
                if self.running {
                    ui.spinner();
                }
            });
        });
    }

    fn main_panel(&mut self, ui: &mut egui::Ui) {
        if self.running || self.draft.is_some() || !self.results.is_empty() {
            self.run_view(ui);
        } else if self.plan.is_some() {
            self.plan_view(ui);
        } else {
            self.empty_state(ui);
        }
    }

    fn run_view(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        self.stage_strip(ui);
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            for (label, tab) in [
                ("Report", MainTab::Report),
                ("Evidence", MainTab::Evidence),
                ("Plan", MainTab::Plan),
                ("History", MainTab::History),
            ] {
                if theme::chip(ui, label, self.main_tab == tab, true).clicked() {
                    self.main_tab = tab;
                    if tab != MainTab::Evidence {
                        self.detail = None;
                    }
                    if tab == MainTab::History {
                        self.history_loaded = false;
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.running {
                    ui.spinner();
                    ui.label(egui::RichText::new("Working").size(10.5).color(p.muted));
                } else if self.draft.is_some() {
                    ui.label(
                        egui::RichText::new("Draft · exact excerpts")
                            .size(10.5)
                            .color(p.faint),
                    );
                }
            });
        });
        ui.add_space(10.0);
        match self.main_tab {
            MainTab::Report => self.draft_view(ui),
            MainTab::Evidence => {
                if self.results.is_empty() {
                    notice(
                        ui,
                        "No evidence yet",
                        "Evidence appears here when the collector finishes. Open the History tab to see the run log.",
                    );
                } else {
                    self.evidence_view(ui);
                }
            }
            MainTab::Plan => {
                if self.plan.is_some() {
                    self.plan_view(ui);
                } else {
                    notice(
                        ui,
                        "No plan for this run",
                        "This run used the question keywords. Build a research plan to see the AI search strategy.",
                    );
                }
            }
            MainTab::History => self.history_view(ui),
        }
    }

    fn stage_strip(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        egui::Frame::new()
            .fill(p.panel)
            .stroke(egui::Stroke::new(1.0, p.border))
            .corner_radius(10)
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for stage in &self.stages {
                        let (fg, bg) = match stage.status.as_str() {
                            "done" => (p.success, tint(p.success, 24)),
                            "running" => (p.accent, tint(p.accent, 30)),
                            "error" => (p.danger, tint(p.danger, 26)),
                            "skipped" => (p.faint, tint(p.faint, 16)),
                            _ => (p.muted, tint(p.muted, 18)),
                        };
                        let label = if stage.count > 0 {
                            format!("{} {}", stage.name, stage.count)
                        } else {
                            stage.name.clone()
                        };
                        theme::pill(ui, &label, fg, bg);
                    }
                });
                let note = self
                    .stages
                    .iter()
                    .find(|stage| stage.status == "running" && !stage.note.is_empty())
                    .or_else(|| {
                        self.stages
                            .iter()
                            .rev()
                            .find(|stage| stage.status == "error" && !stage.note.is_empty())
                    });
                if let Some(stage) = note {
                    ui.add_space(3.0);
                    ui.label(
                        egui::RichText::new(format!("{}: {}", stage.name, stage.note))
                            .size(10.5)
                            .color(p.muted),
                    );
                }
            });
    }

    fn report_view(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        let Some(markdown_text) = self.report_md.clone() else {
            return;
        };
        let mut copy = false;
        let mut open = false;
        let mut excerpts = false;
        let words = markdown_text.split_whitespace().count();
        ui.horizontal_wrapped(|ui| {
            ui.label(bold("Research report", 22.0).color(p.text));
            if self.report_ai {
                theme::pill(ui, "AI synthesis", p.accent, tint(p.accent, 24));
            } else {
                theme::pill(ui, "Extractive", p.comment, tint(p.comment, 24));
            }
            theme::pill(ui, &format!("{} words", words), p.muted, tint(p.muted, 18));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::ghost_button(ui, "Open report.md").clicked() {
                    open = true;
                }
                if theme::ghost_button(ui, "Copy Markdown").clicked() {
                    copy = true;
                }
            });
        });
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(&self.cfg.topic)
                    .size(13.0)
                    .color(p.muted),
            );
            if self.draft.is_some() && theme::ghost_button(ui, "Excerpts").clicked() {
                excerpts = true;
            }
        });
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .id_salt(("report", self.report_epoch))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                markdown::show(ui, &markdown_text);
                ui.add_space(20.0);
            });
        if copy {
            ui.ctx().copy_text(markdown_text.clone());
            self.status = "Report Markdown copied.".to_string();
        }
        if open {
            if let Some(dir) = &self.last_dir {
                let path = dir.join("report.md");
                match open::that(&path) {
                    Ok(()) => self.status = format!("Report opened: {}", path.display()),
                    Err(error) => self.status = format!("Cannot open report: {}", error),
                }
            } else {
                self.status = "Run or load a report first.".to_string();
            }
        }
        if excerpts {
            self.show_excerpts = true;
        }
    }

    fn draft_view(&mut self, ui: &mut egui::Ui) {
        if self.report_md.is_some() && !self.show_excerpts {
            self.report_view(ui);
            return;
        }
        let p = palette(ui.visuals().dark_mode);
        let Some(preview) = self.draft.clone() else {
            if self.running {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.spinner();
                    ui.add_space(8.0);
                    ui.label(bold("Collecting evidence", 18.0).color(p.text));
                    ui.add_space(3.0);
                    ui.label(
                        egui::RichText::new(
                            "The draft appears when the run finishes. Exact excerpts only.",
                        )
                        .size(12.0)
                        .color(p.muted),
                    );
                });
                ui.add_space(14.0);
                theme::section_label(ui, "Recent activity");
                egui::Frame::new()
                    .fill(p.panel)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .corner_radius(10)
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        let start = self.log.len().saturating_sub(6);
                        for line in self.log.iter().skip(start) {
                            ui.label(
                                egui::RichText::new(line)
                                    .font(egui::FontId::monospace(10.5))
                                    .color(p.muted),
                            );
                        }
                    });
            } else {
                notice(
                    ui,
                    "No draft report yet",
                    "Run research to collect evidence. The draft shows exact public excerpts and counts.",
                );
            }
            return;
        };
        let mut copy = false;
        let mut open = false;
        let mut back = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(bold("Draft report", 22.0).color(p.text));
            theme::pill(ui, "Extractive", p.comment, tint(p.comment, 24));
            theme::pill(
                ui,
                &format!("{} records", preview.total),
                p.muted,
                tint(p.muted, 18),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::ghost_button(ui, "Open Markdown").clicked() {
                    open = true;
                }
                if theme::ghost_button(ui, "Copy Markdown").clicked() {
                    copy = true;
                }
                if self.report_md.is_some() && theme::ghost_button(ui, "Report").clicked() {
                    back = true;
                }
            });
        });
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(&preview.question)
                .size(13.0)
                .color(p.muted),
        );
        ui.add_space(5.0);
        egui::Frame::new()
            .fill(tint(p.post, if ui.visuals().dark_mode { 20 } else { 14 }))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(
                        "This draft collects exact public excerpts and counts. It does not draw conclusions. Read the linked threads before you act.",
                    )
                    .size(11.0)
                    .color(p.post),
                );
            });
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::new()
                    .fill(tint(p.accent, if ui.visuals().dark_mode { 20 } else { 12 }))
                    .corner_radius(10)
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(&preview.summary)
                                .size(12.0)
                                .color(p.text),
                        );
                    });
                for section in &preview.sections {
                    ui.add_space(12.0);
                    ui.horizontal_wrapped(|ui| {
                        theme::pill(
                            ui,
                            &section.id.to_uppercase(),
                            p.comment,
                            tint(p.comment, 24),
                        );
                        ui.label(bold(&section.question, 14.5).color(p.text));
                        theme::count_badge(ui, section.match_count);
                    });
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(&section.summary)
                            .size(11.5)
                            .color(p.muted),
                    );
                    ui.add_space(5.0);
                    if section.quotes.is_empty() {
                        ui.label(
                            egui::RichText::new("No matching excerpts in this run.")
                                .size(11.0)
                                .color(p.faint),
                        );
                    }
                    for quote in &section.quotes {
                        egui::Frame::new()
                            .fill(p.surface)
                            .stroke(egui::Stroke::new(1.0, p.border))
                            .corner_radius(10)
                            .inner_margin(egui::Margin::symmetric(14, 10))
                            .show(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    let (fg, bg) = if quote.kind == "post" {
                                        (p.post, tint(p.post, 28))
                                    } else {
                                        (p.comment, tint(p.comment, 26))
                                    };
                                    let kind = if quote.kind == "post" {
                                        "POST"
                                    } else {
                                        "COMMENT"
                                    };
                                    theme::pill(ui, kind, fg, bg);
                                    if !quote.subreddit.is_empty() {
                                        ui.label(
                                            egui::RichText::new(format!("r/{}", quote.subreddit))
                                                .size(11.0)
                                                .color(p.muted),
                                        );
                                    }
                                    if !quote.author.is_empty() {
                                        ui.label(
                                            egui::RichText::new(format!("u/{}", quote.author))
                                                .size(10.5)
                                                .color(p.faint),
                                        );
                                    }
                                    ui.label(
                                        egui::RichText::new(format!("{} points", quote.score))
                                            .size(10.5)
                                            .color(p.faint),
                                    );
                                    if !quote.date.is_empty() {
                                        ui.label(
                                            egui::RichText::new(&quote.date)
                                                .size(10.5)
                                                .color(p.faint),
                                        );
                                    }
                                });
                                ui.add_space(4.0);
                                if let Some(context) = &quote.context {
                                    ui.label(
                                        egui::RichText::new(format!("In reply to: {}", context))
                                            .size(10.5)
                                            .italics()
                                            .color(p.faint),
                                    );
                                    ui.add_space(2.0);
                                }
                                ui.label(egui::RichText::new(&quote.text).size(12.5).color(p.text));
                                if quote.truncated {
                                    ui.add_space(2.0);
                                    ui.label(
                                        egui::RichText::new("Excerpt is shortened.")
                                            .size(10.0)
                                            .color(p.faint),
                                    );
                                }
                                ui.add_space(4.0);
                                ui.hyperlink_to(
                                    egui::RichText::new("Open on Reddit").size(11.0),
                                    &quote.permalink,
                                );
                            });
                        ui.add_space(6.0);
                    }
                }
                ui.add_space(12.0);
                theme::section_label(ui, "Method");
                for line in &preview.method {
                    ui.label(
                        egui::RichText::new(format!("· {}", line))
                            .size(11.0)
                            .color(p.muted),
                    );
                }
                ui.add_space(8.0);
                theme::section_label(ui, "Limitations");
                for line in &preview.limitations {
                    ui.label(
                        egui::RichText::new(format!("· {}", line))
                            .size(11.0)
                            .color(p.muted),
                    );
                }
                ui.add_space(24.0);
            });
        if copy {
            self.copy_draft(ui.ctx());
        }
        if open {
            self.export_draft();
        }
        if back {
            self.show_excerpts = false;
        }
    }

    fn history_view(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        if !self.history_loaded {
            self.history = scan_history();
            self.history_loaded = true;
        }
        ui.horizontal(|ui| {
            ui.label(bold("History", 22.0).color(p.text));
            theme::count_badge(ui, self.history.len());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::ghost_button(ui, "Open runs folder").clicked() {
                    let _ = open::that(config::runs_dir());
                }
            });
        });
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(
                "Every saved search on this computer. Click a run to load its report, evidence, plan, and log.",
            )
            .size(11.5)
            .color(p.muted),
        );
        ui.add_space(8.0);

        let current = self.last_dir.clone();
        let running = self.running;
        let mut load: Option<PathBuf> = None;
        let mut blocked = false;
        let list_height = (ui.available_height() * 0.5).clamp(150.0, 360.0);
        egui::ScrollArea::vertical()
            .id_salt("history_list")
            .max_height(list_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.history.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "No saved runs yet. Run research to make the first one.",
                        )
                        .size(11.5)
                        .color(p.faint),
                    );
                }
                for entry in &self.history {
                    let selected = current
                        .as_ref()
                        .map(|dir| dir == &entry.dir)
                        .unwrap_or(false);
                    let frame = egui::Frame::new()
                        .fill(if selected { tint(p.accent, 18) } else { p.surface })
                        .stroke(egui::Stroke::new(
                            1.0,
                            if selected { p.accent } else { p.border },
                        ))
                        .corner_radius(10)
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(&entry.time)
                                        .font(theme::bold_font(11.0))
                                        .color(p.faint),
                                );
                                if entry.error {
                                    theme::pill(ui, "Stopped", p.danger, tint(p.danger, 26));
                                } else if entry.has_report {
                                    theme::pill(
                                        ui,
                                        if entry.has_ai { "AI report" } else { "Draft" },
                                        if entry.has_ai { p.accent } else { p.muted },
                                        if entry.has_ai {
                                            tint(p.accent, 24)
                                        } else {
                                            tint(p.muted, 20)
                                        },
                                    );
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if selected {
                                            ui.label(
                                                egui::RichText::new("Loaded")
                                                    .size(10.5)
                                                    .color(p.accent),
                                            );
                                        }
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} records",
                                                entry.records
                                            ))
                                            .size(10.5)
                                            .color(p.faint),
                                        );
                                    },
                                );
                            });
                            ui.add_space(2.0);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&entry.title)
                                        .font(theme::bold_font(14.0))
                                        .color(p.text),
                                )
                                .truncate(),
                            );
                            ui.add_space(1.0);
                            ui.label(
                                egui::RichText::new(&entry.folder)
                                    .size(10.0)
                                    .color(p.faint),
                            );
                        });
                    let response = frame.response.interact(egui::Sense::click());
                    if response.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if response.clicked() {
                        if running {
                            blocked = true;
                        } else {
                            load = Some(entry.dir.clone());
                        }
                    }
                    ui.add_space(6.0);
                }
            });
        if blocked {
            self.status = "A run is active. Stop it before you load another run.".to_string();
        }
        if let Some(dir) = load {
            self.load_dir(&dir, true);
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(bold("Run log", 16.0).color(p.text));
            theme::count_badge(ui, self.log.len());
            ui.label(
                egui::RichText::new("for the loaded run")
                    .size(10.5)
                    .color(p.faint),
            );
        });
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(p.panel)
            .stroke(egui::Stroke::new(1.0, p.border))
            .corner_radius(10)
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("history_log")
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &self.log {
                            ui.label(
                                egui::RichText::new(line)
                                    .font(egui::FontId::monospace(10.8))
                                    .color(p.muted),
                            );
                        }
                    });
            });
    }

    fn empty_state(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(64.0, 64.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 18, tint(p.accent, if ui.visuals().dark_mode { 34 } else { 22 }));
            let center = rect.center() + egui::vec2(-3.0, -3.0);
            ui.painter()
                .circle_stroke(center, 11.0, egui::Stroke::new(2.5, p.accent));
            ui.painter().line_segment(
                [center + egui::vec2(8.0, 8.0), center + egui::vec2(15.0, 15.0)],
                egui::Stroke::new(2.5, p.accent),
            );
            ui.add_space(16.0);
            ui.label(bold("Ask a research question", 20.0).color(p.text));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "Type a question on the left. Run research to build a plan, collect evidence, and draft a report.\nArchive search needs no account and no browser.",
                )
                .size(12.5)
                .color(p.muted),
            );
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                theme::pill(ui, "AI plan", p.comment, tint(p.comment, 22));
                theme::pill(ui, "Draft report", p.accent, tint(p.accent, 24));
                theme::pill(ui, "Archive + API", p.muted, tint(p.muted, 20));
                theme::pill(ui, "Rate limits on", p.success, tint(p.success, 24));
            });
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new("Try a question")
                    .size(10.5)
                    .color(p.faint),
            );
            for example in [
                "does armodafinil help with learning?",
                "adderall first week: benefits and side effects",
            ] {
                if ui
                    .link(egui::RichText::new(example).size(11.5))
                    .clicked()
                {
                    self.cfg.topic = example.to_string();
                }
            }
        });
    }

    fn plan_view(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        let Some(plan) = self.plan.clone() else {
            return;
        };
        let mut clear = false;
        let mut copy = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(bold("Research plan", 22.0).color(p.text));
            theme::pill(ui, &self.plan_source, p.accent, tint(p.accent, 26));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::ghost_button(ui, "Clear").clicked() {
                    clear = true;
                }
                if theme::ghost_button(ui, "Copy JSON").clicked() {
                    copy = true;
                }
            });
        });
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(&plan.question)
                .size(13.0)
                .color(p.muted),
        );
        if let Some(warning) = &self.plan_warning {
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(tint(p.post, 22))
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(format!("Note: {}", warning))
                            .size(11.0)
                            .color(p.post),
                    );
                });
        }
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                theme::section_label(ui, "Sub-questions");
                for question in &plan.sub_questions {
                    egui::Frame::new()
                        .fill(p.surface)
                        .stroke(egui::Stroke::new(1.0, p.border))
                        .corner_radius(10)
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                theme::pill(
                                    ui,
                                    &question.id.to_uppercase(),
                                    p.comment,
                                    tint(p.comment, 24),
                                );
                                ui.label(bold(&question.text, 13.0).color(p.text));
                            });
                            if !question.keywords.is_empty() {
                                ui.add_space(4.0);
                                ui.horizontal_wrapped(|ui| {
                                    for keyword in &question.keywords {
                                        theme::chip(ui, keyword, false, false);
                                    }
                                });
                            }
                            if !question.success.is_empty() {
                                ui.add_space(2.0);
                                ui.label(
                                    egui::RichText::new(format!("Success: {}", question.success))
                                        .size(10.5)
                                        .color(p.faint),
                                );
                            }
                        });
                    ui.add_space(6.0);
                }

                ui.add_space(6.0);
                theme::section_label(ui, "Subreddits");
                if plan.subreddits.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "No subreddit candidates. The collector will use your default list.",
                        )
                        .size(11.5)
                        .color(p.faint),
                    );
                } else {
                    ui.horizontal_wrapped(|ui| {
                        for pick in &plan.subreddits {
                            theme::pill(
                                ui,
                                &format!("r/{}", pick.name),
                                p.text,
                                tint(p.accent, 22),
                            );
                        }
                    });
                    let names = plan
                        .subreddits
                        .iter()
                        .map(|pick| format!("r/{} ({})", pick.name, pick.why))
                        .collect::<Vec<_>>()
                        .join("\n");
                    ui.label(egui::RichText::new(names).size(10.5).color(p.faint));
                }

                ui.add_space(10.0);
                theme::section_label(ui, "Queries");
                egui::Grid::new("plan_queries")
                    .num_columns(2)
                    .spacing(egui::vec2(18.0, 8.0))
                    .show(ui, |ui| {
                        for (label, list) in [
                            ("Archive posts", &plan.source_queries.archive_posts),
                            ("Archive comments", &plan.source_queries.archive_comments),
                            ("Reddit search (all Reddit)", &plan.source_queries.reddit_search),
                            ("Web discovery", &plan.source_queries.web),
                        ] {
                            ui.label(
                                egui::RichText::new(label)
                                    .font(theme::bold_font(10.5))
                                    .color(p.muted),
                            );
                            ui.vertical(|ui| {
                                if list.is_empty() {
                                    ui.label(
                                        egui::RichText::new("(none)").size(10.5).color(p.faint),
                                    );
                                } else {
                                    for query in list {
                                        ui.label(
                                            egui::RichText::new(query).size(11.5).color(p.text),
                                        );
                                    }
                                }
                            });
                            ui.end_row();
                        }
                    });

                if !plan.notes.is_empty() {
                    ui.add_space(10.0);
                    theme::section_label(ui, "Notes");
                    ui.label(egui::RichText::new(&plan.notes).size(11.0).color(p.muted));
                }
            });
        if copy {
            if let Ok(json) = serde_json::to_string_pretty(&plan) {
                ui.ctx().copy_text(json);
                self.plan_status = "Plan JSON copied.".to_string();
            }
        }
        if clear {
            self.plan = None;
            self.plan_status.clear();
            self.plan_source.clear();
            self.plan_warning = None;
        }
    }

    fn evidence_view(&mut self, ui: &mut egui::Ui) {
        self.refresh_filtered();
        let p = palette(ui.visuals().dark_mode);
        ui.horizontal(|ui| {
            ui.label(bold("Evidence", 22.0).color(p.text));
            theme::count_badge(ui, self.results.len());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::ghost_button(ui, "Open evidence HTML").clicked() {
                    self.export_report();
                }
            });
        });
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(
                "Ranked by relevance, engagement, recency, source, and text depth. Click a card to read the full text.",
            )
            .size(11.5)
            .color(p.muted),
        );
        ui.add_space(10.0);

        egui::Frame::new()
            .fill(p.panel)
            .stroke(egui::Stroke::new(1.0, p.border))
            .corner_radius(12)
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("Search evidence")
                            .margin(egui::Margin::symmetric(10, 7))
                            .desired_width(250.0),
                    );
                    let subs = self.subreddit_list();
                    let sub_text = if self.filter_sub.is_empty() {
                        "All subreddits".to_string()
                    } else {
                        format!("r/{}", self.filter_sub)
                    };
                    egui::ComboBox::from_id_salt("sub_filter")
                        .width(160.0)
                        .selected_text(sub_text)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.filter_sub,
                                String::new(),
                                "All subreddits",
                            );
                            for sub in &subs {
                                ui.selectable_value(
                                    &mut self.filter_sub,
                                    sub.clone(),
                                    format!("r/{}", sub),
                                );
                            }
                        });
                    if theme::chip(ui, "All", self.filter_kind.is_empty(), true).clicked() {
                        self.filter_kind.clear();
                    }
                    if theme::chip(ui, "Posts", self.filter_kind == "post", true).clicked() {
                        self.filter_kind = "post".to_string();
                    }
                    if theme::chip(ui, "Comments", self.filter_kind == "comment", true).clicked() {
                        self.filter_kind = "comment".to_string();
                    }
                    if (!self.filter.is_empty()
                        || !self.filter_sub.is_empty()
                        || !self.filter_kind.is_empty())
                        && theme::ghost_button(ui, "Clear").clicked()
                    {
                        self.filter.clear();
                        self.filter_sub.clear();
                        self.filter_kind.clear();
                    }
                    ui.label(
                        egui::RichText::new(format!(
                            "{} of {}",
                            self.filtered.len(),
                            self.results.len()
                        ))
                        .size(11.0)
                        .color(p.faint),
                    );
                });
            });

        self.refresh_filtered();
        ui.add_space(10.0);
        if self.filtered.is_empty() {
            self.no_matches(ui);
            return;
        }

        let row_height = 178.0;
        let mut open: Option<String> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, row_height, self.filtered.len(), |ui, range| {
                for index in range {
                    if let Some(record_index) = self.filtered.get(index) {
                        if let Some(record) = self.results.get(*record_index) {
                            if draw_card(ui, record, p, index + 1) {
                                open = Some(record.id.clone());
                            }
                            ui.add_space(8.0);
                        }
                    }
                }
            });
        if let Some(id) = open {
            self.detail = Some(id);
            self.detail_fresh = true;
            // The right panel needs a second frame to take its space from the
            // central panel, so ask for one now.
            ui.ctx().request_repaint();
        }
    }

    /// The right-side drawer with the full text of one record.
    fn evidence_drawer(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        let Some(id) = self.detail.clone() else {
            return;
        };
        let Some(record) = self
            .results
            .iter()
            .find(|record| record.id == id)
            .cloned()
        else {
            self.detail = None;
            return;
        };
        let mut close = false;
        ui.horizontal(|ui| {
            ui.label(bold("Full text", 18.0).color(p.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::ghost_button(ui, "Close").clicked() {
                    close = true;
                }
            });
        });
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            let (kind_color, kind_bg) = if record.kind == "post" {
                (p.post, tint(p.post, 32))
            } else {
                (p.comment, tint(p.comment, 32))
            };
            theme::pill(ui, &record.kind.to_uppercase(), kind_color, kind_bg);
            ui.label(
                egui::RichText::new(format!(
                    "r/{} · u/{} · {}",
                    record.subreddit,
                    record.author,
                    record.date()
                ))
                .size(12.0)
                .color(p.muted),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{} pts · rank {:.2}",
                    score_text(record.score),
                    record.final_score
                ))
                .size(11.5)
                .color(p.faint),
            );
        });
        if !record.title.is_empty() {
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&record.title)
                        .font(theme::bold_font(18.0))
                        .color(p.text),
                )
                .wrap(),
            );
        }
        if let Some(context) = &record.parent_excerpt {
            ui.add_space(10.0);
            egui::Frame::new()
                .fill(p.surface)
                .stroke(egui::Stroke::new(1.0, p.border))
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("In reply to")
                            .size(10.0)
                            .strong()
                            .color(p.faint),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(context)
                            .size(11.5)
                            .italics()
                            .color(p.muted),
                    );
                });
        }
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(8.0);

        let body_height = (ui.available_height() - 44.0).max(120.0);
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt(("evidence_drawer_body", record.id.as_str()))
            .max_height(body_height)
            .auto_shrink([false, false]);
        if self.detail_fresh {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        scroll.show(ui, |ui| {
            markdown::show_reddit(ui, &record.body);
        });
        self.detail_fresh = false;

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if !record.permalink.is_empty() {
                ui.hyperlink_to(
                    egui::RichText::new("Open on Reddit")
                        .size(12.5)
                        .color(p.accent),
                    &record.permalink,
                );
            }
            if theme::ghost_button(ui, "Copy text").clicked() {
                ui.ctx().copy_text(record.body.clone());
            }
        });
        if close {
            self.detail = None;
            ui.ctx().request_repaint();
        }
    }

    fn no_matches(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label(bold("No matches", 17.0).color(p.text));
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("Try a shorter search or clear the filters.")
                    .size(12.0)
                    .color(p.muted),
            );
            ui.add_space(8.0);
            if theme::secondary_button(ui, "Clear filters", 140.0).clicked() {
                self.filter.clear();
                self.filter_sub.clear();
                self.filter_kind.clear();
                self.refresh_filtered();
            }
        });
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let was_open = self.settings_open;
        let mut open = self.settings_open;
        let mut done = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                done = self.settings_body(ui);
            });
        self.settings_open = open && !done;
        if was_open && !self.settings_open {
            config::save(&self.cfg);
            self.persist_key();
        }
    }

    fn settings_body(&mut self, ui: &mut egui::Ui) -> bool {
        ui.horizontal(|ui| {
            for (label, tab) in [
                ("Sources", SettingsTab::Sources),
                ("Limits", SettingsTab::Limits),
                ("Model", SettingsTab::Model),
                ("Account", SettingsTab::Account),
            ] {
                if theme::chip(ui, label, self.settings_tab == tab, true).clicked() {
                    self.settings_tab = tab;
                }
            }
        });
        ui.add_space(10.0);
        egui::ScrollArea::vertical()
            .max_height(420.0)
            .auto_shrink([false, false])
            .show(ui, |ui| match self.settings_tab {
                SettingsTab::Sources => self.settings_sources(ui),
                SettingsTab::Limits => self.settings_limits(ui),
                SettingsTab::Model => self.settings_model(ui),
                SettingsTab::Account => self.settings_account(ui),
            });
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        ui.horizontal(|ui| theme::primary_button(ui, "Done", 110.0).clicked())
            .inner
    }

    fn settings_sources(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        ui.label(bold("Subreddits to search", 14.0).color(p.text));
        ui.label(
            egui::RichText::new(
                "Seed subreddits: one name per line or separated by commas. Every run also searches all of Reddit.",
            )
            .size(11.5)
            .color(p.muted),
        );
        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::multiline(&mut self.cfg.subs)
                .desired_rows(10)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace)
                .margin(egui::Margin::symmetric(10, 8)),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if theme::secondary_button(ui, "Reset to defaults", 150.0).clicked() {
                self.cfg.subs = config::DEFAULT_SUBS.to_string();
            }
            ui.label(
                egui::RichText::new(format!("{} names", subreddit_count(&self.cfg.subs)))
                    .size(11.0)
                    .color(p.faint),
            );
        });
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "Tip: large subreddits may reject keyword searches. The app then samples recent rows and filters them on your computer.",
            )
            .size(11.0)
            .color(p.faint),
        );
    }

    fn settings_limits(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        ui.label(bold("Search depth", 14.0).color(p.text));
        ui.add_space(4.0);
        egui::Grid::new("limit_grid")
            .num_columns(2)
            .spacing(egui::vec2(18.0, 10.0))
            .show(ui, |ui| {
                ui.label("Results per subreddit");
                ui.add(egui::Slider::new(&mut self.cfg.limit, 5..=100));
                ui.end_row();
                ui.label("Official results per sub");
                ui.add(egui::Slider::new(&mut self.cfg.official_limit, 10..=100));
                ui.end_row();
                ui.label("Threads to expand");
                ui.add(egui::Slider::new(&mut self.cfg.threads_count, 1..=20));
                ui.end_row();
                ui.label("Authors to check");
                ui.add(egui::Slider::new(&mut self.cfg.profiles_count, 1..=15));
                ui.end_row();
                ui.label("Only last N years (0 = all)");
                ui.add(egui::Slider::new(&mut self.cfg.years_back, 0..=10));
                ui.end_row();
            });
        ui.add_space(10.0);
        ui.label(bold("Rate safety", 14.0).color(p.text));
        ui.add_space(4.0);
        egui::Grid::new("rate_grid")
            .num_columns(2)
            .spacing(egui::vec2(18.0, 10.0))
            .show(ui, |ui| {
                ui.label("Official requests/min");
                ui.add(egui::Slider::new(&mut self.cfg.rpm, 10..=100));
                ui.end_row();
                ui.label("Archive call interval (s)");
                ui.add(egui::Slider::new(&mut self.cfg.archive_interval, 0.5..=5.0));
                ui.end_row();
                ui.label("All-Reddit call interval (s)");
                ui.add(egui::Slider::new(&mut self.cfg.pullpush_interval, 1.0..=10.0));
                ui.end_row();
                ui.label("Request budget");
                ui.add(egui::Slider::new(&mut self.cfg.max_requests, 50..=2000));
                ui.end_row();
            });
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "Lower limits are safer. The default of 45 requests per minute stays below Reddit's free tier.",
            )
            .size(11.0)
            .color(p.faint),
        );
    }

    fn settings_model(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        let current = ProviderKind::from_id(&self.cfg.llm_provider);
        let ready = LlmConfig::from_saved(&self.cfg, self.llm_key.clone()).is_ready();
        ui.horizontal(|ui| {
            ui.label(bold("AI model", 14.0).color(p.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (status, color) = if ready {
                    ("Ready", p.success)
                } else {
                    ("Setup needed", p.post)
                };
                ui.label(egui::RichText::new(status).size(11.0).color(color));
            });
        });
        ui.label(
            egui::RichText::new(
                "The model turns your question into a search plan. DeepSeek is the default.",
            )
            .size(11.0)
            .color(p.muted),
        );
        ui.add_space(8.0);

        let mut chosen = current;
        egui::Grid::new("model_grid")
            .num_columns(2)
            .spacing(egui::vec2(12.0, 8.0))
            .show(ui, |ui| {
                ui.label("Provider");
                egui::ComboBox::from_id_salt("llm_provider")
                    .width(280.0)
                    .selected_text(chosen.label())
                    .show_ui(ui, |ui| {
                        for provider in ProviderKind::ALL {
                            ui.selectable_value(&mut chosen, provider, provider.label());
                        }
                    });
                ui.end_row();

                ui.label("Base URL");
                ui.add(
                    egui::TextEdit::singleline(&mut self.cfg.llm_base_url)
                        .hint_text("https://api.deepseek.com/v1")
                        .desired_width(300.0)
                        .margin(egui::Margin::symmetric(10, 7)),
                );
                ui.end_row();

                ui.label("Model");
                ui.add(
                    egui::TextEdit::singleline(&mut self.cfg.llm_model)
                        .hint_text(current.default_model())
                        .desired_width(300.0)
                        .margin(egui::Margin::symmetric(10, 7)),
                );
                ui.end_row();

                ui.label("API key");
                ui.add(
                    egui::TextEdit::singleline(&mut self.llm_key)
                        .password(true)
                        .hint_text(if current == ProviderKind::Ollama {
                            "Not needed for Ollama"
                        } else {
                            "Stored in Windows Credential Manager"
                        })
                        .desired_width(300.0)
                        .margin(egui::Margin::symmetric(10, 7)),
                );
                ui.end_row();
            });

        if chosen != current {
            let old_base = current.default_base_url();
            let old_model = current.default_model();
            if self.cfg.llm_base_url.trim().is_empty() || self.cfg.llm_base_url.trim() == old_base {
                self.cfg.llm_base_url = chosen.default_base_url().to_string();
            }
            if self.cfg.llm_model.trim().is_empty() || self.cfg.llm_model.trim() == old_model {
                self.cfg.llm_model = chosen.default_model().to_string();
            }
            self.cfg.llm_provider = chosen.id().to_string();
            self.llm_key = secrets::load(&secrets::target(chosen.id())).unwrap_or_default();
            self.llm_status.clear();
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.llm_testing,
                    egui::Button::new("Test model").min_size(egui::vec2(120.0, 32.0)),
                )
                .clicked()
            {
                self.test_model();
            }
            if self.llm_testing {
                ui.spinner();
            }
        });
        if !self.llm_status.is_empty() {
            let color =
                if self.llm_status.starts_with("Failed") || self.llm_status.starts_with("Cannot") {
                    p.danger
                } else {
                    p.muted
                };
            ui.label(
                egui::RichText::new(&self.llm_status)
                    .size(11.0)
                    .color(color),
            );
        }
        if current == ProviderKind::DeepSeek {
            ui.hyperlink_to(
                egui::RichText::new("Get a DeepSeek API key").size(11.5),
                "https://platform.deepseek.com/api_keys",
            );
        }

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(bold("Planning limits", 13.0).color(p.text));
        let mut calls = self.cfg.max_llm_calls as u32;
        let mut rounds = self.cfg.max_rounds as u32;
        egui::Grid::new("llm_limit_grid")
            .num_columns(2)
            .spacing(egui::vec2(18.0, 10.0))
            .show(ui, |ui| {
                ui.label("Temperature");
                ui.add(egui::Slider::new(&mut self.cfg.llm_temperature, 0.0..=1.5));
                ui.end_row();
                ui.label("Max output tokens");
                ui.add(egui::Slider::new(
                    &mut self.cfg.max_llm_output_tokens,
                    256..=8000,
                ));
                ui.end_row();
                ui.label("Max AI calls per run");
                ui.add(egui::Slider::new(&mut calls, 1..=30));
                ui.end_row();
                ui.label("Critic rounds");
                ui.add(egui::Slider::new(&mut rounds, 0..=3));
                ui.end_row();
            });
        self.cfg.max_llm_calls = calls as usize;
        self.cfg.max_rounds = rounds as usize;
        ui.checkbox(
            &mut self.cfg.review_plan,
            "Review the plan before collection",
        );

        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "Your question and the collected Reddit text go to this provider. Ollama keeps them on this computer. API keys stay in Windows Credential Manager, never in config.json.",
            )
            .size(10.5)
            .color(p.faint),
        );
    }

    fn settings_account(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        let ready = self.account_ready();
        let configured = self.account_configured();
        let (status, color) = if ready {
            ("Connected for this session", p.success)
        } else if configured {
            ("Credentials saved. Enter your password.", p.post)
        } else {
            ("Not connected", p.muted)
        };
        ui.horizontal(|ui| {
            ui.label(bold("Reddit account", 14.0).color(p.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(status).size(11.0).color(color));
            });
        });
        ui.label(
            egui::RichText::new(
                "Official search needs one account you own. Create a script app at reddit.com/prefs/apps. The password stays in memory only and is never saved.",
            )
            .size(11.0)
            .color(p.muted),
        );
        ui.add_space(8.0);
        egui::Grid::new("account_grid")
            .num_columns(2)
            .spacing(egui::vec2(12.0, 8.0))
            .show(ui, |ui| {
                ui.label("Client ID");
                ui.add(
                    egui::TextEdit::singleline(&mut self.cfg.client_id)
                        .desired_width(300.0)
                        .margin(egui::Margin::symmetric(10, 7)),
                );
                ui.end_row();
                ui.label("Client secret");
                ui.add(
                    egui::TextEdit::singleline(&mut self.cfg.client_secret)
                        .password(true)
                        .desired_width(300.0)
                        .margin(egui::Margin::symmetric(10, 7)),
                );
                ui.end_row();
                ui.label("Username");
                ui.add(
                    egui::TextEdit::singleline(&mut self.cfg.username)
                        .desired_width(300.0)
                        .margin(egui::Margin::symmetric(10, 7)),
                );
                ui.end_row();
                ui.label("Password");
                ui.add(
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .desired_width(300.0)
                        .margin(egui::Margin::symmetric(10, 7)),
                );
                ui.end_row();
            });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.testing,
                    egui::Button::new("Test login").min_size(egui::vec2(120.0, 32.0)),
                )
                .clicked()
            {
                self.test_login();
            }
            if self.testing {
                ui.spinner();
            }
        });
        if !self.auth_status.is_empty() {
            let color = if self.auth_status.starts_with("Failed") {
                p.danger
            } else {
                p.muted
            };
            ui.label(
                egui::RichText::new(&self.auth_status)
                    .size(11.5)
                    .color(color),
            );
        }
        ui.add_space(4.0);
        ui.hyperlink_to(
            egui::RichText::new("Open Reddit app settings").size(11.5),
            "https://www.reddit.com/prefs/apps",
        );
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "Use one account. Do not create extra accounts to get around rate limits.",
            )
            .size(11.0)
            .color(p.faint),
        );
    }

    fn log_window(&mut self, ctx: &egui::Context) {
        if !self.log_open {
            return;
        }
        let p = palette(self.cfg.dark);
        let mut open = true;
        egui::Window::new("Activity log")
            .open(&mut open)
            .collapsible(false)
            .default_width(560.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                if self.log.is_empty() {
                    ui.label(
                        egui::RichText::new("No activity yet.")
                            .size(12.0)
                            .color(p.muted),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .stick_to_bottom(true)
                        .max_height(320.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for line in &self.log {
                                ui.label(
                                    egui::RichText::new(line)
                                        .monospace()
                                        .size(11.5)
                                        .color(p.muted),
                                );
                            }
                        });
                }
            });
        self.log_open = open;
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.dark_last != self.cfg.dark {
            theme::apply(&ctx, self.cfg.dark);
            self.dark_last = self.cfg.dark;
        }
        self.sync_title_bar(&ctx);
        self.poll();
        if self.detail.is_some()
            && !self.settings_open
            && !self.log_open
            && ctx.input(|input| input.key_pressed(egui::Key::Escape))
        {
            self.detail = None;
        }

        egui::Panel::top("top_bar")
            .exact_size(58.0)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(palette(self.cfg.dark).panel)
                    .inner_margin(egui::Margin::symmetric(20, 12)),
            )
            .show(ui, |ui| self.top_bar(ui));

        egui::Panel::left("sidebar")
            .exact_size(348.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(palette(self.cfg.dark).panel)
                    .inner_margin(egui::Margin::symmetric(18, 14)),
            )
            .show(ui, |ui| self.sidebar(ui));

        egui::Panel::bottom("status_bar")
            .exact_size(36.0)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(palette(self.cfg.dark).panel)
                    .inner_margin(egui::Margin::symmetric(20, 8)),
            )
            .show(ui, |ui| self.status_bar(ui));

        if self.main_tab == MainTab::Evidence && self.detail.is_some() && !self.results.is_empty() {
            let drawer_width = (ui.available_width() * 0.42).clamp(360.0, 560.0);
            egui::Panel::right("evidence_drawer")
                .exact_size(drawer_width)
                .resizable(false)
                .show_separator_line(true)
                .frame(
                    egui::Frame::new()
                        .fill(palette(self.cfg.dark).panel)
                        .inner_margin(egui::Margin::symmetric(18, 16)),
                )
                .show(ui, |ui| self.evidence_drawer(ui));
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(palette(self.cfg.dark).bg)
                    .inner_margin(egui::Margin::symmetric(24, 18)),
            )
            .show(ui, |ui| self.main_panel(ui));

        self.settings_window(&ctx);
        self.log_window(&ctx);

        if self.running
            || self.testing
            || self.rx.is_some()
            || self.auth_rx.is_some()
            || self.plan_running
            || self.plan_rx.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }
}

/// One saved run, as shown in the History tab.
struct RunEntry {
    dir: PathBuf,
    folder: String,
    title: String,
    time: String,
    records: usize,
    has_report: bool,
    has_ai: bool,
    error: bool,
}

/// Read every run folder that has collected records. Newest first.
fn scan_history() -> Vec<RunEntry> {
    let mut entries: Vec<RunEntry> = Vec::new();
    if let Ok(items) = std::fs::read_dir(config::runs_dir()) {
        for item in items.flatten() {
            let dir = item.path();
            if !dir.is_dir() || !dir.join("results.json").exists() {
                continue;
            }
            let folder = dir
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_string();
            let meta = std::fs::read_to_string(dir.join("run.json"))
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
            let topic = meta
                .as_ref()
                .and_then(|value| value.get("topic"))
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let title = if topic.is_empty() {
                folder.clone()
            } else {
                topic
            };
            let started = meta
                .as_ref()
                .and_then(|value| value.get("started"))
                .and_then(|value| value.as_str())
                .unwrap_or("");
            entries.push(RunEntry {
                time: run_time(started, &folder),
                records: meta
                    .as_ref()
                    .and_then(|value| value.get("total"))
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0) as usize,
                has_report: dir.join("report.md").exists(),
                has_ai: dir.join("report.json").exists(),
                error: meta
                    .as_ref()
                    .and_then(|value| value.get("error"))
                    .and_then(|value| value.as_str())
                    .map(|text| !text.trim().is_empty())
                    .unwrap_or(false),
                dir,
                folder,
                title,
            });
        }
    }
    entries.sort_by(|left, right| right.folder.cmp(&left.folder));
    entries
}

/// Show the run start time. Fall back to the `YYYYMMDD-HHMMSS` folder prefix.
fn run_time(started: &str, folder: &str) -> String {
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(started) {
        return time.format("%Y-%m-%d %H:%M").to_string();
    }
    let date = folder.get(0..8);
    let clock = folder.get(9..15);
    if let (Some(date), Some(clock)) = (date, clock) {
        if folder.as_bytes().get(8) == Some(&b'-')
            && date.bytes().all(|byte| byte.is_ascii_digit())
            && clock.bytes().all(|byte| byte.is_ascii_digit())
        {
            return format!(
                "{}-{}-{} {}:{}",
                &date[0..4],
                &date[4..6],
                &date[6..8],
                &clock[0..2],
                &clock[2..4]
            );
        }
    }
    String::new()
}

fn notice(ui: &mut egui::Ui, title: &str, text: &str) {
    let p = palette(ui.visuals().dark_mode);
    ui.add_space(28.0);
    egui::Frame::new()
        .fill(p.panel)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(12)
        .inner_margin(egui::Margin::symmetric(22, 18))
        .show(ui, |ui| {
            ui.label(bold(title, 18.0).color(p.text));
            ui.add_space(4.0);
            ui.label(egui::RichText::new(text).size(12.0).color(p.muted));
        });
}

fn saved_stages(
    plan: Option<&ResearchPlan>,
    draft: Option<&DraftReport>,
    total: usize,
) -> Vec<StageInfo> {
    let quotes = draft
        .map(|preview| {
            preview
                .sections
                .iter()
                .map(|section| section.quotes.len())
                .sum::<usize>()
        })
        .unwrap_or(0);
    let sections = draft.map(|preview| preview.sections.len()).unwrap_or(0);
    let mut stages = engine::initial_stages();
    for stage in &mut stages {
        stage.note = "Loaded from the saved run.".to_string();
        match stage.name.as_str() {
            "Plan" => {
                stage.status = if plan.is_some() { "done" } else { "skipped" }.to_string();
                stage.count = plan.map(|value| value.sub_questions.len()).unwrap_or(0);
            }
            "Collect" | "Rank" => {
                stage.status = "done".to_string();
                stage.count = total;
            }
            "Review" => {
                stage.status = "skipped".to_string();
            }
            "Write" => {
                stage.status = "done".to_string();
                stage.count = sections;
            }
            "Validate" => {
                stage.status = if quotes > 0 { "done" } else { "skipped" }.to_string();
                stage.count = quotes;
            }
            "Export" => {
                stage.status = "done".to_string();
            }
            _ => {}
        }
    }
    stages
}

fn draw_card(ui: &mut egui::Ui, record: &Record, p: theme::Palette, rank: usize) -> bool {
    let mut open = false;
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        egui::vec2(width, 170.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            let frame = egui::Frame::new()
                .fill(p.surface)
                .stroke(egui::Stroke::new(1.0, p.border))
                .corner_radius(12)
                .inner_margin(egui::Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    // The drawer narrows the list. Drop the extra score text
                    // before the two ends of the meta row can overlap.
                    let narrow = ui.available_width() < 520.0;
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("#{}", rank))
                                .font(theme::bold_font(12.5))
                                .color(p.accent),
                        );
                        let (kind_color, kind_bg) = if record.kind == "post" {
                            (p.post, tint(p.post, 32))
                        } else {
                            (p.comment, tint(p.comment, 32))
                        };
                        theme::pill(ui, &record.kind.to_uppercase(), kind_color, kind_bg);
                        ui.label(
                            egui::RichText::new(format!("r/{}", record.subreddit))
                                .font(theme::bold_font(12.0))
                                .color(p.text),
                        );
                        ui.label(egui::RichText::new("·").size(12.0).color(p.faint));
                        ui.label(
                            egui::RichText::new(format!("u/{}", record.author))
                                .size(12.0)
                                .color(p.muted),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!("{:.2}", record.final_score))
                                    .font(theme::bold_font(12.0))
                                    .color(p.accent),
                            );
                            ui.label(egui::RichText::new("rank").size(9.5).color(p.faint));
                            if !narrow {
                                ui.label(
                                    egui::RichText::new(format!("rel {:.2}", record.relevance))
                                        .size(10.5)
                                        .color(p.faint),
                                );
                            }
                            ui.label(
                                egui::RichText::new(format!("{} pts", score_text(record.score)))
                                    .size(11.0)
                                    .color(p.muted),
                            );
                            ui.label(egui::RichText::new(record.date()).size(11.0).color(p.faint));
                        });
                    });
                    if !record.title.is_empty() {
                        ui.add_space(2.0);
                        let response = ui.add(
                            egui::Label::new(
                                egui::RichText::new(&record.title)
                                    .font(theme::bold_font(15.0))
                                    .color(p.text),
                            )
                            .truncate()
                            .sense(egui::Sense::click()),
                        );
                        if response.clicked() {
                            open = true;
                        }
                    }
                    if let Some(context) = &record.parent_excerpt {
                        ui.add_space(2.0);
                        let response = ui.add(
                            egui::Label::new(
                                egui::RichText::new(format!("In reply to: {}", context))
                                    .size(10.5)
                                    .italics()
                                    .color(p.faint),
                            )
                            .truncate()
                            .sense(egui::Sense::click()),
                        );
                        if response.clicked() {
                            open = true;
                        }
                    }
                    ui.add_space(2.0);
                    let response = ui.add(
                        egui::Label::new(
                            egui::RichText::new(snippet(record, 220))
                                .size(13.0)
                                .color(p.muted),
                        )
                        .truncate()
                        .sense(egui::Sense::click()),
                    );
                    if response.clicked() {
                        open = true;
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if theme::ghost_button(ui, "Read full text").clicked() {
                            open = true;
                        }
                        if !record.permalink.is_empty() {
                            ui.hyperlink_to(
                                egui::RichText::new("Open on Reddit")
                                    .size(12.5)
                                    .color(p.accent),
                                &record.permalink,
                            );
                        }
                        if !record.sub_questions.is_empty() {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Q: {}",
                                    record.sub_questions.join(", ")
                                ))
                                .size(10.5)
                                .color(p.muted),
                            );
                        }
                        if let Some(note) = record.notes.first() {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(note).size(10.0).color(p.faint),
                                )
                                .truncate(),
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let mut sources: Vec<&String> = record.sources.iter().collect();
                            sources.sort();
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(
                                        sources
                                            .iter()
                                            .map(|source| source.as_str())
                                            .collect::<Vec<_>>()
                                            .join(" + "),
                                    )
                                    .size(10.5)
                                    .color(p.faint),
                                )
                                .truncate(),
                            );
                        });
                    });
                });
            if frame.response.hovered() {
                ui.painter().rect_stroke(
                    frame.response.rect,
                    12,
                    egui::Stroke::new(1.0, tint(p.accent, 150)),
                    egui::StrokeKind::Inside,
                );
            }
        },
    );
    open
}

fn snippet(record: &Record, max_chars: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut last_space = false;
    for ch in record.body.chars() {
        let ch = if ch.is_whitespace() { ' ' } else { ch };
        if ch == ' ' && last_space {
            continue;
        }
        last_space = ch == ' ';
        out.push(ch);
        count += 1;
        if count >= max_chars {
            break;
        }
    }
    let out = out.trim();
    if out.is_empty() {
        return "(no text)".to_string();
    }
    if record.body.chars().count() > max_chars {
        format!("{}...", out)
    } else {
        out.to_string()
    }
}

fn score_text(score: i64) -> String {
    if score >= 1000 {
        format!("{:.1}k", score as f64 / 1000.0)
    } else {
        score.to_string()
    }
}

fn subreddit_count(text: &str) -> usize {
    text.split(|c: char| c == ',' || c == '\n' || c == '\r')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_time_uses_the_saved_start() {
        assert_eq!(
            run_time("2026-11-22T07:43:15+05:30", "20260101-000000-x"),
            "2026-11-22 07:43"
        );
    }

    #[test]
    fn run_time_falls_back_to_the_folder_name() {
        assert_eq!(run_time("", "20251120-184501-moda"), "2025-11-20 18:45");
        assert_eq!(run_time("", "odd-name"), "");
    }
}
