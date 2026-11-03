//! Interface do BSDConqueror (egui/eframe).
//!
//! Interface do BSDConqueror (listas Found / lista de baixo, locks, preferências,
//! ícones, zoom) agora sobre o scanner NATIVO do FreeBSD (ptrace + procstat),
//! sem scanmem.
//!
//! Arquitetura: o `MemoryScanner` vive numa thread própria (`worker_loop`).
//! A interface só envia pedidos (`Req`) e recebe respostas (`Resp`) por canais,
//! por isso a janela nunca bloqueia durante uma pesquisa longa.

use crate::core::scanner::{self, MemoryScanner, ProcessInfo, ScanResult};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Aumento fixo da letra em relação ao padrão do egui (12,5 pt).
const FONT_BONUS: f32 = 2.0;
/// Fator usado nas larguras/alturas em relação ao tamanho de letra padrão.
const UI_SCALE: f32 = (12.5 + FONT_BONUS) / 12.5;
/// Cor de realce para títulos e cabeçalhos.
const ACCENT: egui::Color32 = egui::Color32::from_rgb(20, 60, 130);

const SCAN_NAMES: [&str; 6] = [
    "2 e 4 Bytes",
    "1 Byte",
    "2 Bytes",
    "4 Bytes",
    "8 Bytes",
    "Auto (1, 2, 4 e 8)",
];

fn scan_widths(idx: usize) -> &'static [usize] {
    match idx {
        0 => &[2, 4],
        1 => &[1],
        2 => &[2],
        3 => &[4],
        4 => &[8],
        _ => &[1, 2, 4, 8],
    }
}

// ───────────────────────────── Utilitários ─────────────────────────────

fn digits(s: &str, radix: u32) -> Option<i64> {
    if s.is_empty() || s.starts_with(|c: char| c == '+' || c == '-') {
        return None;
    }
    i64::from_str_radix(s, radix).ok()
}

/// Inteiro em decimal ou com prefixo 0x / 0b / 0o (e sinal opcional).
fn parse_int(raw: &str) -> Option<i64> {
    let s = raw.trim();
    let (neg, body) = if let Some(r) = s.strip_prefix('-') {
        (true, r)
    } else {
        (false, s.strip_prefix('+').unwrap_or(s))
    };
    let lower = body.to_ascii_lowercase();
    let base0 = if let Some(h) = lower.strip_prefix("0x") {
        digits(h, 16)
    } else if let Some(b) = lower.strip_prefix("0b") {
        digits(b, 2)
    } else if let Some(o) = lower.strip_prefix("0o") {
        digits(o, 8)
    } else if lower.len() > 1 && lower.starts_with('0') {
        None
    } else {
        digits(&lower, 10)
    };
    match base0 {
        Some(v) => Some(if neg { -v } else { v }),
        None => s.parse::<i64>().ok(),
    }
}

fn kind_name(width: usize) -> String {
    if width == 1 {
        "1 Byte".to_string()
    } else {
        format!("{} Bytes", width)
    }
}

// ───────────────────────────── Preferências ─────────────────────────────
// Ficheiro simples chave=valor em $XDG_CONFIG_HOME/bsdconqueror/config.ini
// (ou ~/.config/bsdconqueror/config.ini).

#[derive(Clone, Debug)]
struct Prefs {
    zoom: f32,
    /// Tamanho da janela em pontos "nativos" (antes do zoom).
    win: Option<(f32, f32)>,
    col_results: [f32; 4],
    col_found: [f32; 2],
    col_proc: [f32; 2],
    left_w: f32,
    top_h: f32,
    scan_kind: usize,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            win: None,
            col_results: [56.0, 230.0, 170.0, 100.0],
            col_found: [140.0, 80.0],
            col_proc: [70.0, 130.0],
            left_w: 340.0,
            top_h: 370.0,
            scan_kind: 0,
        }
    }
}

impl Prefs {
    fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("bsdconqueror").join("config.ini"))
    }

    fn load() -> Self {
        let mut p = Self::default();
        let Some(path) = Self::path() else { return p };
        let Ok(text) = std::fs::read_to_string(path) else { return p };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            let nums: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            match k {
                "zoom" if nums.len() == 1 => p.zoom = nums[0].clamp(0.7, 3.0),
                "window" if nums.len() == 2 => {
                    p.win = Some((nums[0].clamp(500.0, 8000.0), nums[1].clamp(350.0, 8000.0)))
                }
                "col_results" if nums.len() == 4 => {
                    for i in 0..4 {
                        p.col_results[i] = nums[i].clamp(30.0, 1200.0);
                    }
                }
                "col_found" if nums.len() == 2 => {
                    for i in 0..2 {
                        p.col_found[i] = nums[i].clamp(30.0, 1200.0);
                    }
                }
                "col_proc" if nums.len() == 2 => {
                    for i in 0..2 {
                        p.col_proc[i] = nums[i].clamp(30.0, 1200.0);
                    }
                }
                "left_w" if nums.len() == 1 => p.left_w = nums[0].clamp(150.0, 2000.0),
                "top_h" if nums.len() == 1 => p.top_h = nums[0].clamp(150.0, 2000.0),
                "scan_kind" if nums.len() == 1 => {
                    let i = nums[0] as usize;
                    if i < SCAN_NAMES.len() {
                        p.scan_kind = i;
                    }
                }
                _ => {}
            }
        }
        p
    }

    fn to_text(&self) -> String {
        let mut t = String::new();
        t.push_str("# BSDConqueror - preferências (geradas automaticamente)\n");
        t.push_str(&format!("zoom={:.2}\n", self.zoom));
        if let Some((w, h)) = self.win {
            t.push_str(&format!("window={:.0},{:.0}\n", w, h));
        }
        t.push_str(&format!(
            "col_results={:.0},{:.0},{:.0},{:.0}\n",
            self.col_results[0], self.col_results[1], self.col_results[2], self.col_results[3]
        ));
        t.push_str(&format!("col_found={:.0},{:.0}\n", self.col_found[0], self.col_found[1]));
        t.push_str(&format!("col_proc={:.0},{:.0}\n", self.col_proc[0], self.col_proc[1]));
        t.push_str(&format!("left_w={:.0}\n", self.left_w));
        t.push_str(&format!("top_h={:.0}\n", self.top_h));
        t.push_str(&format!("scan_kind={}\n", self.scan_kind));
        t
    }

    fn save(&self) {
        if let Some(path) = Self::path() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, self.to_text());
        }
    }
}

// ───────────────────────────── Desenho de tabelas ─────────────────────────────

/// Título com destaque. O egui não traz fonte a negrito, por isso simulamos:
/// o texto é desenhado duas vezes, com um pequeno desvio, na cor de realce.
fn title(ui: &mut egui::Ui, text: &str) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, ACCENT);
    let (rect, _) = ui.allocate_exact_size(galley.size() + egui::vec2(0.7, 0.0), egui::Sense::hover());
    ui.painter().galley(rect.min, galley.clone(), ACCENT);
    ui.painter().galley(rect.min + egui::vec2(0.7, 0.0), galley, ACCENT);
}

#[derive(Clone, Copy)]
enum Icon {
    Refresh,
    Search,
    Reset,
    SelectAll,
    Clear,
    Remove,
    Quit,
    Add,
    Lock,
    Unlock,
}

/// Ícone embebido (src/icons), com tamanho proporcional à letra atual.
fn ico(ui: &egui::Ui, icon: Icon) -> egui::Image<'static> {
    let src = match icon {
        Icon::Refresh => egui::include_image!("../icons/refresh-32x32.png"),
        Icon::Search => egui::include_image!("../icons/search-32x32.png"),
        Icon::Reset => egui::include_image!("../icons/reset-32x32.png"),
        Icon::SelectAll => egui::include_image!("../icons/select_all-32x32.png"),
        Icon::Clear => egui::include_image!("../icons/clear-32x32.png"),
        Icon::Remove => egui::include_image!("../icons/remove-32x32.png"),
        Icon::Quit => egui::include_image!("../icons/quit-32x32.png"),
        Icon::Add => egui::include_image!("../icons/add-32x32.png"),
        Icon::Lock => egui::include_image!("../icons/lock-32x32.png"),
        Icon::Unlock => egui::include_image!("../icons/unlock-32x32.png"),
    };
    let sz = ui.text_style_height(&egui::TextStyle::Body) * 1.2;
    egui::Image::new(src).fit_to_exact_size(egui::vec2(sz, sz))
}

/// Desenha uma linha de tabela com fundo completo (seleção, hover, zebra).
/// A última coluna ocupa o espaço restante. Devolve a resposta da linha e o
/// retângulo de cada coluna (para colocar widgets, p.ex. ícone/edição).
fn draw_row(
    ui: &mut egui::Ui,
    widths: &[f32],
    texts: &[&str],
    mono: &[bool],
    selected: bool,
    odd: bool,
    header: bool,
) -> (egui::Response, Vec<egui::Rect>) {
    let total = ui.available_width();
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(total, row_h), egui::Sense::click());
    let mut rects = Vec::with_capacity(widths.len());
    let mut x = rect.left();
    for (i, w) in widths.iter().enumerate() {
        let cw = if i + 1 == widths.len() { (rect.right() - x).max(60.0) } else { *w };
        rects.push(egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(cw, row_h)));
        x += cw;
    }
    if ui.is_rect_visible(rect) {
        let v = ui.visuals();
        let bg = if selected {
            v.selection.bg_fill
        } else if header {
            egui::Color32::from_black_alpha(30)
        } else if resp.hovered() {
            egui::Color32::from_black_alpha(14)
        } else if odd {
            v.faint_bg_color
        } else {
            egui::Color32::TRANSPARENT
        };
        let color = if header { ACCENT } else { v.text_color() };
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, bg);
        for (i, cr) in rects.iter().enumerate() {
            let text = texts.get(i).copied().unwrap_or("");
            if text.is_empty() {
                continue;
            }
            let font = if mono.get(i).copied().unwrap_or(false) {
                egui::TextStyle::Monospace.resolve(ui.style())
            } else {
                egui::TextStyle::Body.resolve(ui.style())
            };
            let clip = painter.with_clip_rect(cr.shrink2(egui::vec2(2.0, 0.0)));
            let pos = cr.left_center() + egui::vec2(8.0, 0.0);
            clip.text(pos, egui::Align2::LEFT_CENTER, text, font.clone(), color);
            if header {
                clip.text(pos + egui::vec2(0.7, 0.0), egui::Align2::LEFT_CENTER, text, font, color);
            }
        }
    }
    (resp, rects)
}

/// Pegas arrastáveis nas separações do cabeçalho: redimensionam as colunas
/// fixas (todas menos a última). `widths` está em unidades base.
fn col_handles(ui: &mut egui::Ui, rects: &[egui::Rect], widths: &mut [f32], k: f32, tag: &str) {
    let n = widths.len().min(rects.len().saturating_sub(1));
    for i in 0..n {
        let x = rects[i].right();
        let handle = egui::Rect::from_center_size(
            egui::pos2(x, rects[i].center().y),
            egui::vec2(8.0, rects[i].height()),
        );
        let id = ui.id().with((tag, i));
        let r = ui.interact(handle, id, egui::Sense::drag());
        if r.hovered() || r.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if r.dragged() {
            widths[i] = (widths[i] + r.drag_delta().x / k.max(0.1)).clamp(30.0, 1200.0);
        }
        ui.painter().vline(
            x,
            rects[i].y_range(),
            egui::Stroke::new(1.0_f32, egui::Color32::from_black_alpha(50)),
        );
    }
}

/// Lógica de seleção (clique, Ctrl, Shift) partilhada pelas duas listas.
fn apply_click(sel: &mut HashSet<usize>, anchor: &mut Option<usize>, i: usize, ctrl: bool, shift: bool) {
    if shift {
        if let Some(a) = *anchor {
            if !ctrl {
                sel.clear();
            }
            for k in a.min(i)..=a.max(i) {
                sel.insert(k);
            }
            return;
        }
    }
    if ctrl {
        if !sel.remove(&i) {
            sel.insert(i);
        }
    } else {
        sel.clear();
        sel.insert(i);
    }
    *anchor = Some(i);
}

// ───────────────────────────── Modelo de dados ─────────────────────────────

/// Identidade de um valor em memória: (endereço, largura em bytes).
type Key = (u64, usize);

#[derive(Clone, Debug)]
struct Row {
    addr: u64,
    width: usize,
    description: String,
    value: String,
    locked: bool,
}

impl Row {
    fn key(&self) -> Key {
        (self.addr, self.width)
    }
    fn addr_text(&self) -> String {
        format!("0x{:x}", self.addr)
    }
    fn kind_text(&self) -> String {
        kind_name(self.width)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum EditCol {
    Desc,
    Value,
}

struct Edit {
    row: usize,
    col: EditCol,
    text: String,
    need_focus: bool,
}

struct Popup {
    title: String,
    msg: String,
    armed: bool,
}

// ───────────────────────────── Worker (scanner nativo) ─────────────────────────────

#[derive(Clone, Debug)]
struct WriteItem {
    addr: u64,
    width: usize,
    value: i64,
}

enum Req {
    Scan { gid: u64, pid: i32, value: i64, widths: Vec<usize>, first: bool },
    Reset,
    /// `rows`: linhas afetadas e se estavam com lock (para atualizar o lock).
    Write { gid: u64, rows: Vec<(Key, bool)>, pid: i32, items: Vec<WriteItem> },
    /// Locks (escritas) + leitura de valores atuais, com um único attach.
    Sync { gid: u64, pid: i32, writes: Vec<WriteItem>, reads: Vec<Key> },
    Shutdown,
}

struct ScanOut {
    result: ScanResult,
    prev_count: usize,
}

struct SyncOut {
    reads: Vec<(Key, Option<i64>)>,
    write_error: Option<String>,
}

enum Resp {
    Scan {
        gid: u64,
        pid: i32,
        value: i64,
        first: bool,
        result: Result<ScanOut, String>,
    },
    Write {
        gid: u64,
        rows: Vec<(Key, bool)>,
        results: Vec<(WriteItem, Result<(), String>)>,
    },
    Sync {
        gid: u64,
        result: Result<SyncOut, String>,
    },
}

fn worker_loop(rx: Receiver<Req>, tx: Sender<Resp>, ctx: egui::Context, cancel: Arc<AtomicBool>) {
    let mut sc = MemoryScanner::with_cancel(cancel);
    while let Ok(req) = rx.recv() {
        match req {
            Req::Shutdown => break,
            Req::Reset => sc.reset_scan(),
            Req::Scan { gid, pid, value, widths, first } => {
                let _ = sc.attach_to_pid(pid);
                let result = if first {
                    sc.perform_first_scan(value, &widths)
                } else {
                    sc.perform_next_scan(value, &widths)
                };
                let prev_count = sc.candidate_count();
                let result = result.map(|r| ScanOut { result: r, prev_count });
                let _ = tx.send(Resp::Scan { gid, pid, value, first, result });
            }
            Req::Write { gid, rows, pid, items } => {
                let _ = sc.attach_to_pid(pid);
                let tuples: Vec<(u64, usize, i64)> =
                    items.iter().map(|w| (w.addr, w.width, w.value)).collect();
                let results: Vec<Result<(), String>> = match sc.write_values(&tuples) {
                    Ok(v) => v,
                    Err(e) => tuples.iter().map(|_| Err(e.clone())).collect(),
                };
                let results = items.into_iter().zip(results).collect();
                let _ = tx.send(Resp::Write { gid, rows, results });
            }
            Req::Sync { gid, pid, writes, reads } => {
                let _ = sc.attach_to_pid(pid);
                let wt: Vec<(u64, usize, i64)> = writes.iter().map(|w| (w.addr, w.width, w.value)).collect();
                let result = sc.sync(&wt, &reads).map(|(wres, rvals)| SyncOut {
                    write_error: wres.into_iter().find_map(|r| r.err()),
                    reads: reads.iter().copied().zip(rvals).collect(),
                });
                let _ = tx.send(Resp::Sync { gid, result });
            }
        }
        ctx.request_repaint();
    }
}

// ───────────────────────────── Aplicação ─────────────────────────────

pub struct BSDConquerorApp {
    // processos
    processes: Vec<ProcessInfo>,
    process_filter: String,
    sort_col: usize,
    sort_asc: bool,
    selected_pid: Option<i32>,
    selected_text: String,

    // worker
    req_tx: Sender<Req>,
    resp_rx: Receiver<Resp>,
    worker: Option<JoinHandle<()>>,
    cancel: Arc<AtomicBool>,
    generation: u64,
    in_flight: usize,
    searching: bool,

    // estado da pesquisa
    scan_has_results: bool,
    scan_pid: Option<i32>,
    locked: HashMap<Key, i64>,
    last_sync: Instant,
    sync_now: bool,
    sync_fails: u8,

    // controlos
    value_input: String,
    scan_kind: usize,
    focus_filter_once: bool,
    focus_value_once: bool,

    // listas
    found_total: usize,
    found_rows: Vec<Row>,
    found_selected: HashSet<usize>,
    found_anchor: Option<usize>,
    result_rows: Vec<Row>,
    selected_rows: HashSet<usize>,
    cursor: Option<usize>,
    anchor: Option<usize>,
    editing: Option<Edit>,

    status: String,
    popup: Option<Popup>,

    // preferências persistentes
    col_results: [f32; 4],
    col_found: [f32; 2],
    col_proc: [f32; 2],
    left_w: f32,
    top_h: f32,
    win_size: Option<(f32, f32)>,
    pending_win: Option<(f32, f32)>,
    frame_no: u32,
    cur_prefs: Prefs,
    last_prefs: String,
    last_prefs_check: Instant,
}

impl BSDConquerorApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let prefs = Prefs::load();
        let ctx = &cc.egui_ctx;
        ctx.set_visuals(egui::Visuals::light());
        egui_extras::install_image_loaders(ctx);
        ctx.set_zoom_factor(prefs.zoom);

        // Letra: aumento fixo; o zoom (Ctrl + roda) é feito pelo zoom_factor do egui.
        let base_sizes: Vec<(egui::TextStyle, f32)> = ctx
            .style()
            .text_styles
            .iter()
            .map(|(k, v)| (k.clone(), v.size))
            .collect();
        ctx.style_mut(|st| {
            for (ts, base) in &base_sizes {
                if let Some(f) = st.text_styles.get_mut(ts) {
                    f.size = base + FONT_BONUS;
                }
            }
        });

        // Ícone da janela.
        if let Ok(img) = image::load_from_memory(include_bytes!("../icons/GameConqueror.png")) {
            let rgba = img.to_rgba8();
            ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(Arc::new(egui::IconData {
                width: rgba.width(),
                height: rgba.height(),
                rgba: rgba.into_raw(),
            }))));
        }

        let cancel = Arc::new(AtomicBool::new(false));
        let (req_tx, req_rx) = mpsc::channel();
        let (resp_tx, resp_rx) = mpsc::channel();
        let worker = {
            let c = cancel.clone();
            let cx = ctx.clone();
            thread::spawn(move || worker_loop(req_rx, resp_tx, cx, c))
        };

        let mut app = Self {
            processes: Vec::new(),
            process_filter: String::new(),
            sort_col: 2,
            sort_asc: true,
            selected_pid: None,
            selected_text: "No process selected".into(),
            req_tx,
            resp_rx,
            worker: Some(worker),
            cancel,
            generation: 0,
            in_flight: 0,
            searching: false,
            scan_has_results: false,
            scan_pid: None,
            locked: HashMap::new(),
            last_sync: Instant::now(),
            sync_now: false,
            sync_fails: 0,
            value_input: String::new(),
            scan_kind: prefs.scan_kind,
            focus_filter_once: true,
            focus_value_once: false,
            found_total: 0,
            found_rows: Vec::new(),
            found_selected: HashSet::new(),
            found_anchor: None,
            result_rows: Vec::new(),
            selected_rows: HashSet::new(),
            cursor: None,
            anchor: None,
            editing: None,
            status: "Selecione um processo, escreva um valor e pesquise (processos de outros utilizadores requerem root).".into(),
            popup: None,
            col_results: prefs.col_results,
            col_found: prefs.col_found,
            col_proc: prefs.col_proc,
            left_w: prefs.left_w,
            top_h: prefs.top_h,
            win_size: prefs.win,
            pending_win: prefs.win,
            frame_no: 0,
            last_prefs: prefs.to_text(),
            cur_prefs: prefs,
            last_prefs_check: Instant::now(),
        };
        app.refresh_processes();
        app
    }

    // ── envio de pedidos ──

    fn send_job(&mut self, req: Req) {
        if self.req_tx.send(req).is_ok() {
            self.in_flight += 1;
        }
    }

    /// Invalida respostas pendentes (nova pesquisa/limpeza/outro processo).
    fn bump_generation(&mut self) {
        self.generation += 1;
        self.searching = false;
    }

    fn show_alert(&mut self, title: impl Into<String>, msg: impl Into<String>) {
        let msg = msg.into();
        self.status = msg.lines().next().unwrap_or("").to_string();
        self.popup = Some(Popup { title: title.into(), msg, armed: false });
    }

    // ── preferências ──

    fn update_prefs(&mut self, ctx: &egui::Context) {
        let (rect, minimized, maximized) = ctx.input(|i| {
            let v = i.viewport();
            (v.inner_rect, v.minimized, v.maximized)
        });
        let z = ctx.zoom_factor();
        if minimized != Some(true) && maximized != Some(true) {
            if let Some(r) = rect {
                self.win_size = Some((r.width() * z, r.height() * z));
            }
        }
        self.cur_prefs = Prefs {
            zoom: z,
            win: self.win_size,
            col_results: self.col_results,
            col_found: self.col_found,
            col_proc: self.col_proc,
            left_w: self.left_w,
            top_h: self.top_h,
            scan_kind: self.scan_kind,
        };
        if self.last_prefs_check.elapsed() >= Duration::from_secs(1) {
            self.last_prefs_check = Instant::now();
            self.flush_prefs();
        }
    }

    fn flush_prefs(&mut self) {
        let t = self.cur_prefs.to_text();
        if t != self.last_prefs {
            self.cur_prefs.save();
            self.last_prefs = t;
        }
    }

    // ── processos ──

    fn sort_processes(&mut self) {
        match self.sort_col {
            0 => self.processes.sort_by_key(|p| p.pid),
            1 => self.processes.sort_by(|a, b| {
                a.user.to_lowercase().cmp(&b.user.to_lowercase()).then(a.pid.cmp(&b.pid))
            }),
            _ => self.processes.sort_by(|a, b| {
                a.command.to_lowercase().cmp(&b.command.to_lowercase()).then(a.pid.cmp(&b.pid))
            }),
        }
        if !self.sort_asc {
            self.processes.reverse();
        }
    }

    fn refresh_processes(&mut self) {
        self.processes = scanner::list_processes();
        self.sort_processes();
        if let Some(pid) = self.selected_pid {
            if !self.processes.iter().any(|p| p.pid == pid) {
                self.selected_pid = None;
                self.selected_text = "No process selected".into();
            }
        }
        self.status = format!("{} processos listados.", self.processes.len());
    }

    fn on_process_selected(&mut self, p: &ProcessInfo) {
        // Outro processo: endereços, locks e pesquisa anteriores deixam de fazer sentido.
        self.clear_locks(false);
        self.bump_generation();
        let _ = self.req_tx.send(Req::Reset);
        self.scan_has_results = false;
        self.scan_pid = None;
        self.clear_found_view();
        self.found_total = 0;
        self.clear_work_list();
        self.sync_fails = 0;
        self.selected_pid = Some(p.pid);
        self.focus_value_once = true;
        self.selected_text = format!(
            "Selected: {}  |  PID: {}  |  User: {}",
            p.command, p.pid, p.user
        );
        self.status = format!("Processo selecionado: {} (PID {}).", p.command, p.pid);
    }

    // ── pesquisa ──

    fn on_search(&mut self) {
        if self.popup.is_some() {
            return;
        }
        let Some(pid) = self.selected_pid else {
            self.show_alert("BSDConqueror - Erro", "Selecione primeiro um processo na lista!");
            return;
        };
        let raw = self.value_input.trim().to_string();
        if raw.is_empty() {
            self.status = "Escreva um valor inteiro para pesquisar.".into();
            return;
        }
        let Some(value) = parse_int(&raw) else {
            self.show_alert("BSDConqueror - Entrada Inválida", "Insira um valor inteiro válido para a pesquisa!");
            return;
        };
        let first = !(self.scan_has_results && self.scan_pid == Some(pid));
        let mode = if first { "Search" } else { "Next Search" };
        self.sync_fails = 0;
        self.searching = true;
        self.status = if self.scan_kind == SCAN_NAMES.len() - 1 {
            format!("{}: PID {}, valor {}… (modo Auto: pode demorar bastante)", mode, pid, value)
        } else {
            format!("{}: PID {}, valor {}… (o processo fica parado durante a pesquisa)", mode, pid, value)
        };
        if first {
            self.clear_found_view();
        }
        self.focus_value_once = true;
        let gid = self.generation;
        let widths = scan_widths(self.scan_kind).to_vec();
        self.send_job(Req::Scan { gid, pid, value, widths, first });
    }

    fn clear_found_view(&mut self) {
        self.found_rows.clear();
        self.found_selected.clear();
        self.found_anchor = None;
    }

    fn clear_work_list(&mut self) {
        self.result_rows.clear();
        self.selected_rows.clear();
        self.cursor = None;
        self.anchor = None;
        self.editing = None;
    }

    fn finish_scan(&mut self, pid: i32, value: i64, first: bool, result: Result<ScanOut, String>) {
        let mode = if first { "Search" } else { "Next Search" };
        match result {
            Ok(out) => {
                let r = out.result;
                if r.kept_previous {
                    // Nenhum candidato com este valor: os anteriores mantêm-se.
                    let sample: Vec<String> = r.sample.iter().map(|v| v.to_string()).collect();
                    self.status = format!(
                        "Nenhum dos {} endereços tem o valor {}. Valores atuais (amostra): {}.",
                        out.prev_count,
                        value,
                        sample.join(", ")
                    );
                    self.sync_now = true;
                    return;
                }
                self.clear_found_view();
                for (a, w, v) in &r.shown {
                    self.found_rows.push(Row {
                        addr: *a,
                        width: *w,
                        description: String::new(),
                        value: v.to_string(),
                        locked: false,
                    });
                }
                self.found_total = r.total;
                self.scan_has_results = r.total > 0;
                self.scan_pid = if r.total > 0 { Some(pid) } else { None };
                let trunc = if r.truncated { " (limite atingido: resultados parciais)" } else { "" };
                self.status = if r.total == 0 {
                    format!("{}: nenhum endereço com o valor {}.", mode, value)
                } else {
                    format!(
                        "{}: {} endereços (a mostrar {}).{}",
                        mode,
                        r.total,
                        r.shown.len(),
                        trunc
                    )
                };
                self.sync_now = true;
            }
            Err(e) => {
                self.show_alert("BSDConqueror - Erro de Execução", format!("Erro: {}", e));
            }
        }
    }

    // ── escrita ──

    /// Edição da coluna Value. Se a linha editada pertence a uma multisseleção,
    /// o mesmo valor é atribuído a todas as linhas selecionadas.
    fn commit_edit(&mut self, row: usize, col: EditCol, text: String) {
        if row >= self.result_rows.len() {
            return;
        }
        if col == EditCol::Desc {
            self.result_rows[row].description = text.trim().to_string();
            return;
        }
        let Some(pid) = self.selected_pid else {
            self.status = "Seleciona um processo antes de editar valores.".into();
            return;
        };
        let Some(value) = parse_int(&text) else {
            self.status = "Valor inteiro inválido.".into();
            return;
        };
        let targets: Vec<usize> = if self.selected_rows.contains(&row) && self.selected_rows.len() > 1 {
            let mut v: Vec<usize> = self
                .selected_rows
                .iter()
                .copied()
                .filter(|i| *i < self.result_rows.len())
                .collect();
            v.sort_unstable();
            v
        } else {
            vec![row]
        };
        let rows: Vec<(Key, bool)> = targets
            .iter()
            .map(|&i| (self.result_rows[i].key(), self.result_rows[i].locked))
            .collect();
        let items: Vec<WriteItem> = targets
            .iter()
            .map(|&i| WriteItem {
                addr: self.result_rows[i].addr,
                width: self.result_rows[i].width,
                value,
            })
            .collect();
        self.status = format!("A escrever {} em {} endereço(s)…", value, items.len());
        let gid = self.generation;
        self.send_job(Req::Write { gid, rows, pid, items });
    }

    fn finish_write(&mut self, rows: Vec<(Key, bool)>, results: Vec<(WriteItem, Result<(), String>)>) {
        let total = results.len();
        let mut ok = 0usize;
        let mut first_err: Option<String> = None;
        let mut last_ok: Option<(Key, i64)> = None;
        for (it, res) in &results {
            let key: Key = (it.addr, it.width);
            match res {
                Ok(()) => {
                    ok += 1;
                    last_ok = Some((key, it.value));
                    if let Some(r) = self.result_rows.iter_mut().find(|r| r.key() == key) {
                        r.value = it.value.to_string();
                    }
                    if rows.iter().any(|(k, l)| *k == key && *l) {
                        self.locked.insert(key, it.value);
                    }
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e.clone());
                    }
                }
            }
        }
        self.sync_now = true;
        if let Some(e) = first_err {
            self.show_alert(
                "BSDConqueror - Erro de Escrita",
                format!("{} de {} escritas falharam.\n\n{}", total - ok, total, e),
            );
            return;
        }
        self.status = match (total, last_ok) {
            (1, Some(((addr, _), v))) => format!("Valor de 0x{:x} alterado para {}.", addr, v),
            (_, Some((_, v))) => format!("Valor {} escrito em {} endereço(s).", v, ok),
            _ => String::new(),
        };
    }

    fn handle_responses(&mut self) {
        while let Ok(resp) = self.resp_rx.try_recv() {
            self.in_flight = self.in_flight.saturating_sub(1);
            match resp {
                Resp::Scan { gid, pid, value, first, result } => {
                    if gid != self.generation {
                        continue;
                    }
                    self.searching = false;
                    self.finish_scan(pid, value, first, result);
                }
                Resp::Write { gid, rows, results } => {
                    if gid != self.generation {
                        continue;
                    }
                    self.finish_write(rows, results);
                }
                Resp::Sync { gid, result } => {
                    if gid != self.generation {
                        continue;
                    }
                    match result {
                        Ok(out) => {
                            self.sync_fails = 0;
                            self.apply_reads(&out.reads);
                            if let Some(e) = out.write_error {
                                self.status = format!("Erro ao fixar valor: {}", e);
                            }
                        }
                        Err(e) => {
                            self.sync_fails = self.sync_fails.saturating_add(1);
                            self.status = format!("Erro de acesso ao processo: {}", e);
                            if self.sync_fails >= 3 {
                                self.clear_locks(false);
                                self.status = format!(
                                    "Erro de acesso ao processo: {} — atualização e locks desativados.",
                                    e
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // ── locks e atualização em tempo real ──

    /// Locks (escritas) + valores atuais (leituras), com um único attach:
    /// de 0,3 em 0,3 s com locks ativos, de 1 em 1 s sem locks.
    fn tick_sync(&mut self) {
        if self.in_flight > 0 || self.searching || self.sync_fails >= 3 {
            return;
        }
        let Some(pid) = self.selected_pid else { return };
        if self.found_rows.is_empty() && self.result_rows.is_empty() {
            return;
        }
        let interval = if self.locked.is_empty() { 1000 } else { 300 };
        if !self.sync_now && self.last_sync.elapsed() < Duration::from_millis(interval) {
            return;
        }
        self.sync_now = false;
        self.last_sync = Instant::now();
        let writes: Vec<WriteItem> = self
            .locked
            .iter()
            .map(|(&(addr, width), &value)| WriteItem { addr, width, value })
            .collect();
        let mut reads: Vec<Key> = Vec::new();
        for r in self.found_rows.iter().chain(self.result_rows.iter()) {
            let k = r.key();
            if !reads.contains(&k) {
                reads.push(k);
            }
        }
        let gid = self.generation;
        self.send_job(Req::Sync { gid, pid, writes, reads });
    }

    /// Atualiza os valores mostrados (listas Found e de baixo) por (endereço, largura).
    fn apply_reads(&mut self, reads: &[(Key, Option<i64>)]) {
        let map: HashMap<Key, Option<i64>> = reads.iter().copied().collect();
        let skip = match &self.editing {
            Some(e) if e.col == EditCol::Value => Some(e.row),
            _ => None,
        };
        let text = |v: Option<i64>| match v {
            Some(x) => x.to_string(),
            None => "?".to_string(),
        };
        for r in self.found_rows.iter_mut() {
            if let Some(v) = map.get(&r.key()) {
                r.value = text(*v);
            }
        }
        for (i, r) in self.result_rows.iter_mut().enumerate() {
            if skip == Some(i) {
                continue;
            }
            if let Some(v) = map.get(&r.key()) {
                r.value = text(*v);
            }
        }
    }

    /// Lock/unlock. Se a linha clicada pertence a uma multisseleção, aplica-se
    /// a todas as linhas selecionadas (cada uma fica fixa no seu valor atual).
    fn on_lock_toggled(&mut self, i: usize) {
        let Some(first) = self.result_rows.get(i) else { return };
        let lock = !first.locked;
        let targets: Vec<usize> = if self.selected_rows.contains(&i) && self.selected_rows.len() > 1 {
            let mut v: Vec<usize> = self
                .selected_rows
                .iter()
                .copied()
                .filter(|t| *t < self.result_rows.len())
                .collect();
            v.sort_unstable();
            v
        } else {
            vec![i]
        };
        let mut done = 0usize;
        let mut bad = 0usize;
        let mut last_msg = String::new();
        for t in &targets {
            let row = &mut self.result_rows[*t];
            let key = row.key();
            if lock {
                match parse_int(row.value.trim()) {
                    Some(v) => {
                        row.locked = true;
                        self.locked.insert(key, v);
                        last_msg = format!("Lock ativo: 0x{:x} = {}", key.0, v);
                        done += 1;
                    }
                    None => {
                        last_msg = format!("Não é possível fixar o valor '{}'.", row.value.trim());
                        bad += 1;
                    }
                }
            } else {
                row.locked = false;
                self.locked.remove(&key);
                last_msg = format!("Lock desativado: 0x{:x}", key.0);
                done += 1;
            }
        }
        self.sync_now = true;
        self.status = if targets.len() == 1 {
            last_msg
        } else if lock {
            format!(
                "Lock ativo em {} endereço(s){}.",
                done,
                if bad > 0 { format!("; {} sem valor válido", bad) } else { String::new() }
            )
        } else {
            format!("Lock desativado em {} endereço(s).", done)
        };
    }

    fn clear_locks(&mut self, update_status: bool) {
        self.locked.clear();
        for r in &mut self.result_rows {
            r.locked = false;
        }
        if update_status {
            self.status = "Todos os locks foram desativados.".into();
        }
    }

    // ── botões ──

    fn on_reset(&mut self) {
        self.clear_locks(false);
        self.bump_generation();
        let _ = self.req_tx.send(Req::Reset);
        self.scan_has_results = false;
        self.scan_pid = None;
        self.clear_found_view();
        self.found_total = 0;
        self.value_input.clear();
        self.focus_value_once = true;
        self.status = "Pesquisa reiniciada.".into();
    }

    fn on_clear(&mut self) {
        self.clear_locks(false);
        self.clear_work_list();
        self.status = "Lista de baixo limpa.".into();
    }

    // ── seleção / edição de linhas ──

    fn select_all(&mut self) {
        self.selected_rows = (0..self.result_rows.len()).collect();
    }

    fn click_row(&mut self, i: usize, ctrl: bool, shift: bool) {
        // Clique simples numa linha já selecionada (multisseleção) mantém a seleção,
        // para o duplo clique / F2 editarem todas as linhas de uma vez.
        if !ctrl && !shift && self.selected_rows.len() > 1 && self.selected_rows.contains(&i) {
            self.cursor = Some(i);
            return;
        }
        apply_click(&mut self.selected_rows, &mut self.anchor, i, ctrl, shift);
        self.cursor = Some(i);
    }

    fn click_found(&mut self, i: usize, ctrl: bool, shift: bool) {
        apply_click(&mut self.found_selected, &mut self.found_anchor, i, ctrl, shift);
    }

    /// Copia linhas da lista Found para a lista de baixo (sem duplicar).
    fn add_found_rows(&mut self, idx: &[usize]) {
        let mut added = 0usize;
        let mut dup = 0usize;
        for &i in idx {
            let Some(r) = self.found_rows.get(i) else { continue };
            if self.result_rows.iter().any(|x| x.key() == r.key()) {
                dup += 1;
                continue;
            }
            let mut row = r.clone();
            row.description = "Sem descrição".to_string();
            row.locked = self.locked.contains_key(&row.key());
            self.result_rows.push(row);
            added += 1;
        }
        self.status = if dup > 0 {
            format!("{} endereço(s) adicionado(s); {} já estavam na lista.", added, dup)
        } else {
            format!("{} endereço(s) adicionado(s) à lista de baixo.", added)
        };
    }

    fn add_selected_found(&mut self) {
        let mut idx: Vec<usize> = self.found_selected.iter().copied().collect();
        idx.sort_unstable();
        if idx.is_empty() {
            self.status = "Seleciona primeiro linhas na lista Found (Ctrl/Shift para várias).".into();
            return;
        }
        self.add_found_rows(&idx);
    }

    /// Remove da lista de baixo as linhas selecionadas (e respetivos locks).
    fn remove_selected(&mut self) {
        let mut idx: Vec<usize> = self
            .selected_rows
            .iter()
            .copied()
            .filter(|i| *i < self.result_rows.len())
            .collect();
        if idx.is_empty() {
            self.status = "Seleciona linhas na lista de baixo para as remover.".into();
            return;
        }
        idx.sort_unstable();
        for &i in &idx {
            let k = self.result_rows[i].key();
            self.locked.remove(&k);
        }
        let set: HashSet<usize> = idx.iter().copied().collect();
        let mut n = 0usize;
        self.result_rows.retain(|_| {
            let keep = !set.contains(&n);
            n += 1;
            keep
        });
        self.selected_rows.clear();
        self.cursor = None;
        self.anchor = None;
        self.editing = None;
        self.status = format!("{} linha(s) removida(s) da lista.", idx.len());
    }

    fn begin_edit(&mut self, i: usize, col: EditCol) {
        if let Some(r) = self.result_rows.get(i) {
            let text = match col {
                EditCol::Desc => r.description.clone(),
                EditCol::Value => r.value.clone(),
            };
            self.editing = Some(Edit { row: i, col, text, need_focus: true });
            self.cursor = Some(i);
        }
    }

    /// F2: edita o valor da linha do cursor (ou da primeira selecionada).
    fn begin_edit_cursor(&mut self) {
        let row = self
            .cursor
            .filter(|i| *i < self.result_rows.len())
            .or_else(|| self.selected_rows.iter().copied().min());
        match row {
            Some(i) => self.begin_edit(i, EditCol::Value),
            None => self.status = "Seleciona uma linha para editar o valor.".into(),
        }
    }

    // ── desenho ──

    fn ui_found(&mut self, ui: &mut egui::Ui) {
        let mut add_clicked = false;
        ui.horizontal(|ui| {
            title(ui, &format!("Found: {}", self.found_total));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can = !self.found_rows.is_empty();
                let btn = egui::Button::image_and_text(ico(ui, Icon::Add), "Add Selected");
                if ui.add_enabled(can, btn).clicked() {
                    add_clicked = true;
                }
            });
        });
        ui.separator();
        ui.spacing_mut().item_spacing.y = 0.0;
        let k = UI_SCALE;
        let w = [self.col_found[0] * k, self.col_found[1] * k, 0.0];
        let mods = ui.input(|i| i.modifiers);
        let mut click: Option<(usize, bool, bool)> = None;
        let mut dbl: Option<usize> = None;
        let (_, hr) = draw_row(ui, &w, &["Address", "Type", "Value"], &[false; 3], false, false, true);
        col_handles(ui, &hr, &mut self.col_found, k, "found");
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, row) in self.found_rows.iter().enumerate() {
                let sel = self.found_selected.contains(&i);
                let a = row.addr_text();
                let t = row.kind_text();
                let (resp, _) = draw_row(
                    ui,
                    &w,
                    &[a.as_str(), t.as_str(), row.value.as_str()],
                    &[true, false, true],
                    sel,
                    i % 2 == 1,
                    false,
                );
                if resp.double_clicked() {
                    dbl = Some(i);
                } else if resp.clicked() {
                    click = Some((i, mods.command, mods.shift));
                }
            }
        });
        if let Some((i, c, sh)) = click {
            self.click_found(i, c, sh);
        }
        if let Some(i) = dbl {
            self.add_found_rows(&[i]);
        }
        if add_clicked {
            self.add_selected_found();
        }
    }

    fn ui_controls(&mut self, ui: &mut egui::Ui) {
        title(ui, "Process selection");
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 130.0).max(60.0);
            let filter_resp = ui.add(
                egui::TextEdit::singleline(&mut self.process_filter)
                    .hint_text("Filter process name, PID or user…")
                    .desired_width(w),
            );
            if self.focus_filter_once {
                filter_resp.request_focus();
                self.focus_filter_once = false;
            }
            if ui
                .add(egui::Button::image_and_text(ico(ui, Icon::Refresh), "Refresh"))
                .clicked()
            {
                self.refresh_processes();
            }
        });

        // Tabela de processos
        let q = self.process_filter.trim().to_lowercase();
        let mut clicked: Option<ProcessInfo> = None;
        let mut new_sort: Option<usize> = None;
        let k = UI_SCALE;
        let pw = [self.col_proc[0] * k, self.col_proc[1] * k, 0.0];
        let proc_h = 6.0 * (ui.text_style_height(&egui::TextStyle::Body) + 8.0);
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let (resp, rects) =
                draw_row(ui, &pw, &["PID", "User", "Process"], &[false; 3], false, false, true);
            col_handles(ui, &rects, &mut self.col_proc, k, "proc");
            // Seta de ordenação desenhada como triângulo (a fonte do egui não tem ▲/▼).
            if let Some(r) = rects.get(self.sort_col) {
                let (cx, cy, sz) = (r.right() - 14.0 * k, r.center().y, 4.0 * k);
                let pts = if self.sort_asc {
                    vec![
                        egui::pos2(cx - sz, cy + sz * 0.6),
                        egui::pos2(cx + sz, cy + sz * 0.6),
                        egui::pos2(cx, cy - sz * 0.8),
                    ]
                } else {
                    vec![
                        egui::pos2(cx - sz, cy - sz * 0.6),
                        egui::pos2(cx + sz, cy - sz * 0.6),
                        egui::pos2(cx, cy + sz * 0.8),
                    ]
                };
                ui.painter().add(egui::Shape::convex_polygon(pts, ACCENT, egui::Stroke::NONE));
            }
            if resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    new_sort = rects.iter().position(|r| r.contains(pos));
                }
            }
            egui::ScrollArea::vertical()
                .max_height(proc_h)
                .min_scrolled_height(proc_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let mut n = 0usize;
                    for p in &self.processes {
                        if !q.is_empty()
                            && !format!("{} {} {}", p.pid, p.user, p.command)
                                .to_lowercase()
                                .contains(&q)
                        {
                            continue;
                        }
                        let sel = self.selected_pid == Some(p.pid);
                        let pid_s = p.pid.to_string();
                        let (resp, _) = draw_row(
                            ui,
                            &pw,
                            &[pid_s.as_str(), p.user.as_str(), p.command.as_str()],
                            &[true, false, false],
                            sel,
                            n % 2 == 1,
                            false,
                        );
                        if resp.clicked() {
                            clicked = Some(p.clone());
                        }
                        n += 1;
                    }
                });
        });
        if let Some(c) = new_sort {
            if self.sort_col == c {
                self.sort_asc = !self.sort_asc;
            } else {
                self.sort_col = c;
                self.sort_asc = true;
            }
            self.sort_processes();
        }
        if let Some(p) = clicked {
            if self.selected_pid != Some(p.pid) {
                self.on_process_selected(&p);
            }
        }

        ui.label(self.selected_text.as_str());
        ui.separator();

        // Linha de valor
        ui.horizontal(|ui| {
            ui.label("Value:");
            let w = (ui.available_width() - 200.0).max(60.0);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.value_input)
                    .hint_text("Integer value, e.g. 123456")
                    .desired_width(w),
            );
            if self.focus_value_once {
                resp.request_focus();
                self.focus_value_once = false;
            }
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let label = if self.scan_has_results { "Next Search" } else { "Search" };
            let can = !self.searching;
            let search_btn = egui::Button::image_and_text(ico(ui, Icon::Search), label);
            if ui.add_enabled(can, search_btn).clicked() || (enter && can) {
                self.on_search();
            }
            if ui
                .add(egui::Button::image_and_text(ico(ui, Icon::Reset), "Reset"))
                .clicked()
            {
                self.on_reset();
            }
        });

        // Tamanho dos valores a procurar (largura em bytes).
        ui.horizontal(|ui| {
            ui.label("Data type:");
            egui::ComboBox::new("scan_kind_combo", "")
                .selected_text(SCAN_NAMES[self.scan_kind])
                .show_ui(ui, |ui| {
                    for (i, n) in SCAN_NAMES.iter().enumerate() {
                        ui.selectable_value(&mut self.scan_kind, i, *n);
                    }
                });
        });

        ui.label("Native scan (ptrace): the target process is paused briefly while scanning. Lock: click the padlock to keep writing a value.");
    }

    fn ui_results(&mut self, ui: &mut egui::Ui) {
        // Botões centrados por cima da grelha.
        let k = UI_SCALE;
        let bw = 120.0 * k;
        let total = bw * 4.0 + ui.spacing().item_spacing.x * 3.0;
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
            let b = egui::Button::image_and_text(ico(ui, Icon::SelectAll), "Select all");
            if ui.add_sized([bw, 26.0 * k], b).clicked() {
                self.select_all();
            }
            let b = egui::Button::image_and_text(ico(ui, Icon::Clear), "Clear results");
            if ui.add_sized([bw, 26.0 * k], b).clicked() {
                self.on_clear();
            }
            let b = egui::Button::image_and_text(ico(ui, Icon::Remove), "Remove");
            if ui
                .add_sized([bw, 26.0 * k], b)
                .on_hover_text("Remove as linhas selecionadas (tecla Delete)")
                .clicked()
            {
                self.remove_selected();
            }
            let b = egui::Button::image_and_text(ico(ui, Icon::Quit), "Quit");
            if ui.add_sized([bw, 26.0 * k], b).clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
        ui.add_space(4.0);

        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let mods = ui.input(|i| i.modifiers);
        let was_editing = self.editing.is_some();
        let mut editing = self.editing.take();
        let mut toggle: Option<usize> = None;
        let mut click: Option<(usize, bool, bool)> = None;
        let mut start_edit: Option<(usize, EditCol)> = None;
        let mut commit: Option<(usize, EditCol, String)> = None;
        let mut cancel = false;

        let cr = self.col_results;
        let w = [cr[0] * k, cr[1] * k, cr[2] * k, cr[3] * k, 0.0];
        ui.spacing_mut().item_spacing.y = 0.0;
        let (_, hr) = draw_row(
            ui,
            &w,
            &["Lock", "Description", "Address", "Type", "Value"],
            &[false; 5],
            false,
            false,
            true,
        );
        col_handles(ui, &hr, &mut self.col_results, k, "results");

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if self.result_rows.is_empty() {
                ui.add_space(8.0);
                ui.weak("Seleciona linhas na lista Found e usa \"Add Selected\" (ou duplo clique numa linha) para as trazer para aqui.");
            }
            for i in 0..self.result_rows.len() {
                let sel = self.selected_rows.contains(&i);
                let row = &self.result_rows[i];
                let (ed_desc, ed_val) = match &editing {
                    Some(e) if e.row == i => (e.col == EditCol::Desc, e.col == EditCol::Value),
                    _ => (false, false),
                };
                let addr_t = row.addr_text();
                let kind_t = row.kind_text();
                let desc_t = if ed_desc { "" } else { row.description.as_str() };
                let val_t = if ed_val { "" } else { row.value.as_str() };
                let (resp, rects) = draw_row(
                    ui,
                    &w,
                    &["", desc_t, addr_t.as_str(), kind_t.as_str(), val_t],
                    &[false, false, true, false, true],
                    sel,
                    i % 2 == 1,
                    false,
                );

                let lock_img = if row.locked { ico(ui, Icon::Lock) } else { ico(ui, Icon::Unlock) };
                if ui.put(rects[0], egui::ImageButton::new(lock_img).frame(false)).clicked() {
                    toggle = Some(i);
                }

                if let Some(e) = editing.as_mut() {
                    if e.row == i {
                        let col = e.col;
                        let (rect, mono) = if col == EditCol::Desc { (rects[1], false) } else { (rects[4], true) };
                        let mut te = egui::TextEdit::singleline(&mut e.text)
                            .id(egui::Id::new(("cell_edit", i)))
                            .desired_width(rect.width() - 8.0);
                        if mono {
                            te = te.font(egui::TextStyle::Monospace);
                        }
                        let er = ui.put(rect.shrink2(egui::vec2(4.0, 1.0)), te);
                        if e.need_focus {
                            er.request_focus();
                            e.need_focus = false;
                        }
                        if er.lost_focus() {
                            if esc {
                                cancel = true;
                            } else {
                                commit = Some((i, col, e.text.clone()));
                            }
                        }
                    }
                }

                if !(ed_desc || ed_val) {
                    let mut handled = false;
                    if resp.double_clicked() {
                        // Duplo clique em Value ou Description (ou F2) edita a célula.
                        if let Some(pos) = resp.interact_pointer_pos() {
                            if rects[4].contains(pos) {
                                start_edit = Some((i, EditCol::Value));
                                handled = true;
                            } else if rects[1].contains(pos) {
                                start_edit = Some((i, EditCol::Desc));
                                handled = true;
                            }
                        }
                    }
                    if !handled && resp.clicked() {
                        click = Some((i, mods.command, mods.shift));
                    }
                }
            }
        });

        self.editing = editing;
        if esc && !was_editing && !ui.ctx().wants_keyboard_input() {
            self.selected_rows.clear();
        }
        if cancel {
            self.editing = None;
        }
        if let Some((i, col, text)) = commit {
            self.editing = None;
            self.commit_edit(i, col, text);
        }
        if let Some((i, col)) = start_edit {
            self.begin_edit(i, col);
        }
        if let Some(i) = toggle {
            self.on_lock_toggled(i);
        }
        if let Some((i, ctrl, shift)) = click {
            self.click_row(i, ctrl, shift);
        }
    }

    /// Janela de aviso/erro (não altera o estilo global).
    fn show_popup(&mut self, ctx: &egui::Context) {
        let Some(p) = self.popup.as_ref() else { return };
        let (title, msg, armed) = (p.title.clone(), p.msg.clone(), p.armed);
        let fg = egui::Color32::from_rgb(240, 240, 240);
        let frame = egui::Frame::window(&ctx.style()).fill(egui::Color32::from_rgb(45, 45, 45));
        let mut close = false;
        egui::Window::new(egui::RichText::new(title).color(fg))
            .frame(frame)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(msg).color(fg));
                ui.add_space(14.0);
                ui.vertical_centered(|ui| {
                    if ui.button("    OK    ").clicked() {
                        close = true;
                    }
                });
                ui.add_space(4.0);
            });
        if armed {
            if ctx.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
        } else if let Some(pp) = self.popup.as_mut() {
            // Só "arma" no frame seguinte, para o Enter que abriu o aviso não o fechar logo.
            pp.armed = true;
            ctx.request_repaint();
        }
        if close {
            self.popup = None;
        }
    }
}

impl eframe::App for BSDConquerorApp {
    /// FreeBSD/X11: muitas configurações mapeiam a tecla Backspace para o
    /// keysym "Delete". A tecla física continua a ser Backspace, por isso
    /// repomos o comportamento esperado.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        for ev in &mut raw_input.events {
            if let egui::Event::Key { key, physical_key, .. } = ev {
                if *key == egui::Key::Delete && *physical_key == Some(egui::Key::Backspace) {
                    *key = egui::Key::Backspace;
                }
            }
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Restaura o tamanho guardado da janela (já com o zoom aplicado).
        self.frame_no = self.frame_no.saturating_add(1);
        if self.frame_no == 2 {
            if let Some((w, h)) = self.pending_win.take() {
                let z = ctx.zoom_factor();
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w / z, h / z)));
            }
        }

        // Ctrl + roda do rato: aumenta/diminui toda a interface (texto incluído).
        // (Ctrl +, Ctrl - e Ctrl 0 já são tratados pelo próprio egui.)
        let (zoom, ctrl, scroll) =
            ctx.input(|i| (i.zoom_delta(), i.modifiers.command, i.raw_scroll_delta.y));
        let factor = if (zoom - 1.0).abs() > f32::EPSILON {
            zoom
        } else if ctrl && scroll != 0.0 {
            (scroll / 500.0).exp()
        } else {
            1.0
        };
        if (factor - 1.0).abs() > f32::EPSILON {
            ctx.set_zoom_factor((ctx.zoom_factor() * factor).clamp(0.7, 3.0));
        }

        self.handle_responses();
        self.tick_sync();
        ctx.request_repaint_after(Duration::from_millis(100));

        if self.popup.is_none() && !ctx.wants_keyboard_input() {
            if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::A)) {
                self.select_all();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F2)) {
                self.begin_edit_cursor();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
                self.remove_selected();
            }
        }

        egui::TopBottomPanel::bottom("status_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.searching {
                    ui.spinner();
                }
                ui.label(self.status.as_str());
            });
        });

        let top = egui::TopBottomPanel::top("upper_panel")
            .resizable(true)
            .default_height(self.top_h)
            .show(ctx, |ui| {
                let side = egui::SidePanel::left("address_panel")
                    .default_width(self.left_w)
                    .resizable(true)
                    .show_inside(ui, |ui| {
                        self.ui_found(ui);
                    });
                self.left_w = side.response.rect.width();
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    self.ui_controls(ui);
                });
            });
        self.top_h = top.response.rect.height();

        egui::CentralPanel::default().show(ctx, |ui| {
            self.ui_results(ui);
        });

        self.update_prefs(ctx);
        self.show_popup(ctx);

        // X11/XIM: com o IME ativo, setas/Backspace podem ser "engolidos" pelo
        // método de entrada. A aplicação só precisa de números e notas curtas.
        ctx.output_mut(|o| o.ime = None);
    }
}

impl Drop for BSDConquerorApp {
    fn drop(&mut self) {
        self.flush_prefs();
        // Interrompe pesquisas longas para o processo alvo ser libertado (PT_DETACH).
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.req_tx.send(Req::Shutdown);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}
