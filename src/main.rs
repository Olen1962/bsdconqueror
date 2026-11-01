//! BSDConqueror 0.12.0 — porte do original Python/GTK para Rust + egui/eframe.
//!
//! Arquitetura:
//!   * `Backend`  — equivalente a `ScanmemBackend` (sessão scanmem persistente,
//!                  leitura bloqueante até ao prompt, com timeouts).
//!   * `worker_loop` — thread dedicada que é dona do `Backend`. É a única que
//!                  bloqueia; a UI só envia pedidos (`Req`) e recebe respostas
//!                  (`Resp`) por canais, por isso a janela nunca fica pendurada.
//!   * `BSDConqueror` — a interface egui (equivalente à janela GTK).
//!
//! O editor de células (SingleMemoryEditor) está em `parse_value` + `commit_edit`.

use eframe::egui;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const APP_VERSION: &str = "0.12.0";
const INT32_MIN: i64 = -2_147_483_648;
const UINT32_MAX: i64 = 4_294_967_295;
const LIST_LIMIT: usize = 20;

// ───────────────────────────── Logging ─────────────────────────────
// Equivalente ao logging.basicConfig(filename="bsdconqueror-debug.log").

static LOG_FILE: OnceLock<Mutex<std::fs::File>> = OnceLock::new();

fn init_log() {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    if let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("bsdconqueror-debug.log"))
    {
        let _ = LOG_FILE.set(Mutex::new(f));
    }
}

fn log_line(level: &str, msg: &str) {
    if let Some(m) = LOG_FILE.get() {
        if let Ok(mut f) = m.lock() {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            let _ = writeln!(f, "{:.3} {} {}", secs, level, msg);
        }
    }
}

macro_rules! log_info  { ($($a:tt)*) => { log_line("INFO",    &format!($($a)*)) }; }
macro_rules! log_debug { ($($a:tt)*) => { log_line("DEBUG",   &format!($($a)*)) }; }
macro_rules! log_warn  { ($($a:tt)*) => { log_line("WARNING", &format!($($a)*)) }; }
macro_rules! log_error { ($($a:tt)*) => { log_line("ERROR",   &format!($($a)*)) }; }

// ───────────────────────────── Utilitários ─────────────────────────────

fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.len().saturating_sub(n);
    chars[start..].iter().collect()
}

fn prompt_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|\n)\s*(?:\d+)?\s*>\s*$").unwrap())
}

fn match_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\[\s*(\d+)\]\s+([0-9a-fA-F]+),.*?,\s*([^,]+),\s*([^,]+),\s*\[([^\]]*)\]")
            .unwrap()
    })
}

fn count_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(?:currently have|have)\s+(\d+)\s+matches?").unwrap())
}

/// Último "N matches" do scanmem.
fn match_count(output: &str) -> Option<usize> {
    count_re()
        .captures_iter(output)
        .last()
        .and_then(|c| c[1].parse().ok())
}

#[derive(Clone, Debug)]
struct ScanMatch {
    id: u64,
    address: u64,
    region: String,
    value: String,
    types: String,
}

/// Equivalente a `parse_scanmem_matches`.
fn parse_matches(output: &str) -> Vec<ScanMatch> {
    let mut v = Vec::new();
    for line in output.lines() {
        if let Some(c) = match_re().captures(line.trim()) {
            if let Ok(address) = u64::from_str_radix(&c[2], 16) {
                v.push(ScanMatch {
                    id: c[1].parse().unwrap_or(0),
                    address,
                    region: c[3].trim().to_string(),
                    value: c[4].trim().to_string(),
                    types: c[5].trim().to_string(),
                });
            }
        }
    }
    v
}

fn digits(s: &str, radix: u32) -> Option<i64> {
    if s.is_empty() || s.starts_with(|c| c == '+' || c == '-') {
        return None;
    }
    i64::from_str_radix(s, radix).ok()
}

/// `int(raw, 0)` do Python, com fallback para `int(raw, 10)`.
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
        None // base 0 rejeita zeros à esquerda; cai no fallback decimal
    } else {
        digits(&lower, 10)
    };
    match base0 {
        Some(v) => Some(if neg { -v } else { v }),
        None => s.parse::<i64>().ok(),
    }
}

enum ValErr {
    Invalid,
    Range,
}

fn parse_value(raw: &str) -> Result<i64, ValErr> {
    let v = parse_int(raw).ok_or(ValErr::Invalid)?;
    if !(INT32_MIN..=UINT32_MAX).contains(&v) {
        return Err(ValErr::Range);
    }
    Ok(v)
}

fn find_scanmem() -> Option<String> {
    if let Ok(p) = std::env::var("SCANMEM_PATH") {
        if !p.is_empty() {
            return Some(p);
        }
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let c = dir.join("scanmem");
        if c.is_file() {
            return Some(c.to_string_lossy().into_owned());
        }
    }
    None
}

// ───────────────────────────── Backend scanmem ─────────────────────────────

struct Session {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Vec<u8>>,
    pid: i32,
}

fn spawn_reader(mut out: ChildStdout, tx: Sender<Vec<u8>>) {
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match out.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

impl Session {
    fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    /// Equivalente a `_read_prompt`: lê até o prompt aparecer (ou timeout).
    fn read_prompt(&mut self, timeout_s: u64) -> Result<(String, bool), String> {
        let deadline = Instant::now() + Duration::from_secs(timeout_s);
        let mut data: Vec<u8> = Vec::new();
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(format!(
                    "Timeout à espera do prompt scanmem. Saída: {}",
                    tail(&String::from_utf8_lossy(&data), 1200)
                ));
            }
            let wait = (deadline - now).min(Duration::from_millis(250));
            match self.rx.recv_timeout(wait) {
                Ok(chunk) => {
                    data.extend(chunk);
                    while let Ok(c) = self.rx.try_recv() {
                        data.extend(c);
                    }
                    let text = String::from_utf8_lossy(&data).into_owned();
                    if prompt_re().is_match(text.trim_end()) {
                        return Ok((text, true));
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if self.exited() {
                        while let Ok(c) = self.rx.try_recv() {
                            data.extend(c);
                        }
                        return Ok((String::from_utf8_lossy(&data).into_owned(), false));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Ok((String::from_utf8_lossy(&data).into_owned(), false));
                }
            }
        }
    }

    fn shutdown(&mut self) {
        let _ = writeln!(self.stdin, "exit");
        let _ = self.stdin.flush();
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Backend {
    exe: String,
    session: Option<Session>,
    /// Sem logs de debug (usado na atualização periódica de valores).
    quiet: bool,
}

impl Backend {
    fn alive(&mut self) -> bool {
        match self.session.as_mut() {
            Some(s) => !s.exited(),
            None => false,
        }
    }

    fn close(&mut self) {
        if let Some(mut s) = self.session.take() {
            s.shutdown();
        }
    }

    fn start(&mut self, pid: i32) -> Result<(), String> {
        self.close();
        // `sh -c 'exec scanmem 2>&1'` funde stderr em stdout (como no Python:
        // stderr=subprocess.STDOUT) e mantém a ordem das mensagens.
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("exec \"$0\" 2>&1")
            .arg(&self.exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("PAGER", "cat")
            .spawn()
            .map_err(|e| format!("Falha ao iniciar scanmem: {}", e))?;
        let stdin = child.stdin.take().ok_or("scanmem sem stdin")?;
        let stdout = child.stdout.take().ok_or("scanmem sem stdout")?;
        let (tx, rx) = mpsc::channel();
        spawn_reader(stdout, tx);
        let mut s = Session { child, stdin, rx, pid: 0 };

        // Se algo falhar daqui até ao fim, `s` é largado e o Drop mata o processo.
        let (out, ok) = s.read_prompt(15)?;
        log_debug!("scanmem startup (prompt={}): {}", ok, tail(&out, 1200));
        if !ok {
            return Err(if out.trim().is_empty() {
                "scanmem não apresentou prompt inicial".to_string()
            } else {
                tail(&out, 700)
            });
        }
        s.pid = pid;
        self.session = Some(s);

        let out = match self.command(&format!("pid {}", pid), 20) {
            Ok(o) => o,
            Err(e) => {
                self.close();
                return Err(e);
            }
        };
        log_debug!("pid command: {}", tail(&out, 1200));
        let low = out.to_lowercase();
        if low.contains("error:") || low.contains("failed") {
            self.close();
            return Err(if out.trim().is_empty() {
                format!("Não foi possível associar PID {}", pid)
            } else {
                tail(&out, 700)
            });
        }
        Ok(())
    }

    /// Equivalente a `_command`.
    fn command(&mut self, cmd: &str, timeout_s: u64) -> Result<String, String> {
        let quiet = self.quiet;
        let s = match self.session.as_mut() {
            Some(s) => s,
            None => return Err("sessão scanmem terminou".into()),
        };
        if s.exited() {
            return Err("sessão scanmem terminou".into());
        }
        if !quiet {
            log_debug!("scanmem > {}", cmd);
        }
        while s.rx.try_recv().is_ok() {} // descarta lixo antigo
        writeln!(s.stdin, "{}", cmd).map_err(|e| format!("Erro ao enviar comando: {}", e))?;
        s.stdin
            .flush()
            .map_err(|e| format!("Erro no flush: {}", e))?;
        let (out, ok) = s.read_prompt(timeout_s)?;
        if !quiet {
            log_debug!("scanmem output (prompt={}): {}", ok, tail(&out, 4000));
        }
        if !ok {
            return Err(if out.trim().is_empty() {
                "scanmem terminou antes do prompt".to_string()
            } else {
                tail(&out, 700)
            });
        }
        Ok(out)
    }

    /// Devolve (saída completa, total reportado pelo scanmem).
    fn search(&mut self, pid: i32, value: i64, first: bool) -> Result<(String, Option<usize>), String> {
        let wrong_pid = self.session.as_ref().map(|s| s.pid) != Some(pid);
        if first || !self.alive() || wrong_pid {
            self.start(pid)?;
        }
        let mut output = self.command(&value.to_string(), 90)?;
        let count = match_count(&output);
        if let Some(c) = count {
            if c > LIST_LIMIT {
                log_info!("{} matches; suppressing address list until <= {}", c, LIST_LIMIT);
                return Ok((output, count));
            }
        }
        let list = self.command(&format!("list {}", LIST_LIMIT), 90)?;
        output.push('\n');
        output.push_str(&list);
        Ok((output, count))
    }

    /// Relê os valores atuais dos matches (scanmem `update` + `list`).
    /// Nunca arranca sessões novas: sem sessão ativa devolve uma lista vazia.
    fn refresh(&mut self, pid: i32) -> Result<Vec<ScanMatch>, String> {
        let wrong_pid = self.session.as_ref().map(|s| s.pid) != Some(pid);
        if !self.alive() || wrong_pid {
            return Ok(Vec::new());
        }
        self.quiet = true;
        let r = self.refresh_inner();
        self.quiet = false;
        r
    }

    fn refresh_inner(&mut self) -> Result<Vec<ScanMatch>, String> {
        let up = self.command("update", 20)?;
        let low = up.to_lowercase();
        if low.contains("unknown") || low.contains("error") {
            return Err(tail(&up, 300));
        }
        let out = self.command(&format!("list {}", LIST_LIMIT), 20)?;
        Ok(parse_matches(&out))
    }

    fn write(&mut self, pid: i32, vtype: &str, address: &str, value: i64) -> Result<String, String> {
        let wrong_pid = self.session.as_ref().map(|s| s.pid) != Some(pid);
        if !self.alive() || wrong_pid {
            self.start(pid)?;
        }
        // scanmem 0.17: write <value_type> <address> <value>
        let out = self.command(&format!("write {} {} {}", vtype, address, value), 20)?;
        log_debug!("write address={} type={} value={}: {}", address, vtype, value, tail(&out, 1000));
        Ok(out)
    }
}

// ───────────────────────────── Worker ─────────────────────────────

struct WriteItem {
    address: String,
    value: i64,
    vtype: String,
}

enum WriteKind {
    Edit { address: String, was_locked: bool },
    /// Mesmo valor para várias linhas selecionadas.
    Many { rows: Vec<(String, bool)> },
    Lock,
}

enum Req {
    Search { gid: u64, pid: i32, value: i64, first: bool },
    Write { gid: u64, kind: WriteKind, pid: i32, items: Vec<WriteItem> },
    Refresh { gid: u64, pid: i32 },
    Close,
    Shutdown,
}

struct SearchOut {
    matches: Vec<ScanMatch>,
    total: usize,
}

enum WriteRes {
    Ok,
    /// O scanmem respondeu com "error:" / "failed".
    Failed(String),
    /// Falha de sessão (timeout, processo terminou, etc.).
    Exception(String),
}

enum Resp {
    Search {
        gid: u64,
        pid: i32,
        value: i64,
        first: bool,
        result: Result<SearchOut, String>,
    },
    Write {
        gid: u64,
        kind: WriteKind,
        results: Vec<(String, i64, WriteRes)>,
    },
    Refresh {
        gid: u64,
        result: Result<Vec<ScanMatch>, String>,
    },
}

fn worker_loop(exe: String, rx: Receiver<Req>, tx: Sender<Resp>, ctx: egui::Context) {
    let mut be = Backend { exe, session: None, quiet: false };
    while let Ok(req) = rx.recv() {
        match req {
            Req::Shutdown => break,
            Req::Close => be.close(),
            Req::Refresh { gid, pid } => {
                let result = be.refresh(pid);
                let _ = tx.send(Resp::Refresh { gid, result });
            }
            Req::Search { gid, pid, value, first } => {
                let result = match be.search(pid, value, first) {
                    Ok((out, count)) => {
                        let matches = parse_matches(&out);
                        let total = count.unwrap_or(matches.len());
                        if total == 0 {
                            be.close();
                        }
                        Ok(SearchOut { matches, total })
                    }
                    Err(e) => {
                        log_error!("Falha na pesquisa: {}", e);
                        be.close();
                        Err(e)
                    }
                };
                let _ = tx.send(Resp::Search { gid, pid, value, first, result });
            }
            Req::Write { gid, kind, pid, items } => {
                let mut results: Vec<(String, i64, WriteRes)> = Vec::new();
                let mut failed_msg: Option<String> = None;
                for it in items {
                    if let Some(msg) = &failed_msg {
                        results.push((it.address, it.value, WriteRes::Exception(msg.clone())));
                        continue;
                    }
                    let r = match be.write(pid, &it.vtype, &it.address, it.value) {
                        Ok(out) => {
                            let low = out.to_lowercase();
                            if low.contains("error:") || low.contains("failed") {
                                log_warn!("Write failed at {}: {}", it.address, tail(&out, 500));
                                WriteRes::Failed(tail(&out, 500))
                            } else {
                                log_info!(
                                    "Write address={} type={} value={}",
                                    it.address, it.vtype, it.value
                                );
                                WriteRes::Ok
                            }
                        }
                        Err(e) => {
                            log_error!("Write exception at {}: {}", it.address, e);
                            be.close();
                            failed_msg = Some(e.clone());
                            WriteRes::Exception(e)
                        }
                    };
                    results.push((it.address, it.value, r));
                }
                let _ = tx.send(Resp::Write { gid, kind, results });
            }
        }
        ctx.request_repaint();
    }
    be.close();
}

// ───────────────────────────── Interface ─────────────────────────────

#[derive(Clone, Debug)]
struct ProcessRow {
    pid: i32,
    user: String,
    command: String,
}

#[derive(Clone, Debug)]
struct ResultRow {
    locked: bool,
    description: String,
    address: String,
    types: String,
    value: String,
}

/// `ps -axo pid,user,comm` -> split(None, 2) como no Python.
fn parse_ps_line(line: &str) -> Option<ProcessRow> {
    let line = line.trim();
    let mut it = line.splitn(2, char::is_whitespace);
    let pid_s = it.next()?;
    let rest = it.next()?.trim_start();
    let mut it2 = rest.splitn(2, char::is_whitespace);
    let user = it2.next()?;
    let command = it2.next()?.trim_start();
    if command.is_empty() || pid_s.eq_ignore_ascii_case("pid") {
        return None;
    }
    Some(ProcessRow {
        pid: pid_s.parse().ok()?,
        user: user.to_string(),
        command: command.to_string(),
    })
}

// ───────────────────────────── Preferências ─────────────────────────────
// Ficheiro simples chave=valor em $XDG_CONFIG_HOME/bsdconqueror/config.ini
// (ou ~/.config/bsdconqueror/config.ini). Sem dependências extra.

#[derive(Clone, Debug)]
struct Prefs {
    font_delta: f32,
    zoom: f32,
    /// Tamanho da janela em pontos "nativos" (antes do zoom).
    win: Option<(f32, f32)>,
    /// Larguras (unidades base) das colunas fixas de cada tabela.
    col_results: [f32; 4],
    col_found: f32,
    col_proc: [f32; 2],
    left_w: f32,
    top_h: f32,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            font_delta: 2.0,
            zoom: 1.0,
            win: None,
            col_results: [56.0, 230.0, 170.0, 150.0],
            col_found: 150.0,
            col_proc: [70.0, 130.0],
            left_w: 300.0,
            top_h: 370.0,
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
                "font_delta" if nums.len() == 1 => p.font_delta = nums[0].clamp(-4.0, 14.0),
                "zoom" if nums.len() == 1 => p.zoom = nums[0].clamp(0.7, 3.0),
                "window" if nums.len() == 2 => {
                    p.win = Some((nums[0].clamp(500.0, 8000.0), nums[1].clamp(350.0, 8000.0)))
                }
                "col_results" if nums.len() == 4 => {
                    for i in 0..4 {
                        p.col_results[i] = nums[i].clamp(30.0, 1200.0);
                    }
                }
                "col_found" if nums.len() == 1 => p.col_found = nums[0].clamp(30.0, 1200.0),
                "col_proc" if nums.len() == 2 => {
                    for i in 0..2 {
                        p.col_proc[i] = nums[i].clamp(30.0, 1200.0);
                    }
                }
                "left_w" if nums.len() == 1 => p.left_w = nums[0].clamp(150.0, 2000.0),
                "top_h" if nums.len() == 1 => p.top_h = nums[0].clamp(150.0, 2000.0),
                _ => {}
            }
        }
        p
    }

    fn to_text(&self) -> String {
        let mut t = String::new();
        t.push_str("# BSDConqueror - preferências (geradas automaticamente)\n");
        t.push_str(&format!("font_delta={:.1}\n", self.font_delta));
        t.push_str(&format!("zoom={:.2}\n", self.zoom));
        if let Some((w, h)) = self.win {
            t.push_str(&format!("window={:.0},{:.0}\n", w, h));
        }
        t.push_str(&format!(
            "col_results={:.0},{:.0},{:.0},{:.0}\n",
            self.col_results[0], self.col_results[1], self.col_results[2], self.col_results[3]
        ));
        t.push_str(&format!("col_found={:.0}\n", self.col_found));
        t.push_str(&format!("col_proc={:.0},{:.0}\n", self.col_proc[0], self.col_proc[1]));
        t.push_str(&format!("left_w={:.0}\n", self.left_w));
        t.push_str(&format!("top_h={:.0}\n", self.top_h));
        t
    }

    fn save(&self) {
        if let Some(path) = Self::path() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(&path, self.to_text()) {
                log_warn!("Não foi possível guardar preferências em {:?}: {}", path, e);
            }
        }
    }
}

/// Pegas arrastáveis nas separações do cabeçalho: redimensionam as colunas
/// fixas (todas menos a última). `widths` está em unidades base (sem zoom de letra).
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

/// Cor de realce para títulos e cabeçalhos.
const ACCENT: egui::Color32 = egui::Color32::from_rgb(20, 60, 130);

/// Título com destaque. O egui não traz fonte a negrito, por isso simulamos:
/// o texto é desenhado duas vezes, com um pequeno desvio, na cor de realce.
fn title(ui: &mut egui::Ui, text: &str) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, ACCENT);
    let (rect, _) = ui.allocate_exact_size(galley.size() + egui::vec2(0.7, 0.0), egui::Sense::hover());
    ui.painter().galley(rect.min, galley.clone(), ACCENT);
    ui.painter().galley(rect.min + egui::vec2(0.7, 0.0), galley, ACCENT);
}

/// Ícone embebido, com tamanho proporcional à letra atual.
fn ico(ui: &egui::Ui, src: egui::ImageSource<'static>) -> egui::Image<'static> {
    let sz = ui.text_style_height(&egui::TextStyle::Body) * 1.2;
    egui::Image::new(src).fit_to_exact_size(egui::vec2(sz, sz))
}

/// Desenha uma linha de tabela com fundo completo (seleção, hover, zebra).
/// A última coluna ocupa o espaço restante. Devolve a resposta da linha e o
/// retângulo de cada coluna (para colocar widgets, p.ex. checkbox/edição).
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

struct BSDConqueror {
    // processos
    processes: Vec<ProcessRow>,
    process_filter: String,
    sort_col: usize,
    sort_asc: bool,
    selected_pid: Option<i32>,
    selected_text: String,

    // backend
    scanmem_path: Option<String>,
    req_tx: Option<Sender<Req>>,
    resp_rx: Receiver<Resp>,
    worker: Option<JoinHandle<()>>,
    generation: u64,
    in_flight: usize,
    searching: bool,

    // estado da pesquisa
    scan_has_results: bool,
    scan_pid: Option<i32>,
    /// endereço ("0x...") -> (valor, tipo scanmem)
    locked: HashMap<String, (i64, String)>,
    last_lock_tick: Instant,

    // controlos
    value_input: String,

    // resultados
    found: usize,
    /// Lista "Found": resultados da pesquisa (só leitura/seleção).
    found_rows: Vec<ResultRow>,
    found_selected: HashSet<usize>,
    found_anchor: Option<usize>,
    result_rows: Vec<ResultRow>,
    selected_rows: HashSet<usize>,
    cursor: Option<usize>,
    anchor: Option<usize>,
    /// (linha, texto, precisa de foco)
    editing: Option<(usize, String, bool)>,

    status: String,

    // tamanho da letra (Ctrl + roda do rato)
    base_sizes: Vec<(egui::TextStyle, f32)>,
    font_delta: f32,
    scroll_acc: f32,

    // preferências persistentes
    col_results: [f32; 4],
    col_found: f32,
    col_proc: [f32; 2],
    left_w: f32,
    top_h: f32,
    win_size: Option<(f32, f32)>,
    cur_prefs: Prefs,
    last_prefs: String,
    last_prefs_check: Instant,

    // atualização em tempo real dos valores
    refresh_fails: u8,
    last_refresh: Instant,
}

impl BSDConqueror {
    fn new(cc: &eframe::CreationContext<'_>, prefs: Prefs) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::light());
        cc.egui_ctx.set_zoom_factor(prefs.zoom);
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let base_sizes: Vec<(egui::TextStyle, f32)> = cc
            .egui_ctx
            .style()
            .text_styles
            .iter()
            .map(|(k, v)| (k.clone(), v.size))
            .collect();
        let scanmem_path = find_scanmem();
        let (resp_tx, resp_rx) = mpsc::channel();
        let (req_tx, worker) = match &scanmem_path {
            Some(p) => {
                let (tx, rx) = mpsc::channel();
                let exe = p.clone();
                let ctx = cc.egui_ctx.clone();
                let h = thread::spawn(move || worker_loop(exe, rx, resp_tx, ctx));
                (Some(tx), Some(h))
            }
            None => (None, None),
        };

        let mut app = Self {
            processes: Vec::new(),
            process_filter: String::new(),
            sort_col: 2,
            sort_asc: true,
            selected_pid: None,
            selected_text: "No process selected".into(),
            scanmem_path,
            req_tx,
            resp_rx,
            worker,
            generation: 0,
            in_flight: 0,
            searching: false,
            scan_has_results: false,
            scan_pid: None,
            locked: HashMap::new(),
            last_lock_tick: Instant::now(),
            value_input: String::new(),
            found: 0,
            found_rows: Vec::new(),
            found_selected: HashSet::new(),
            found_anchor: None,
            result_rows: Vec::new(),
            selected_rows: HashSet::new(),
            cursor: None,
            anchor: None,
            editing: None,
            base_sizes,
            font_delta: prefs.font_delta,
            scroll_acc: 0.0,
            col_results: prefs.col_results,
            col_found: prefs.col_found,
            col_proc: prefs.col_proc,
            left_w: prefs.left_w,
            top_h: prefs.top_h,
            win_size: prefs.win,
            last_prefs: prefs.to_text(),
            cur_prefs: prefs.clone(),
            last_prefs_check: Instant::now(),
            refresh_fails: 0,
            last_refresh: Instant::now(),
            status: "Ready — Search, refine with Next Search, and lock results when 20 or fewer remain."
                .into(),
        };
        app.apply_font(&cc.egui_ctx);
        app.refresh_processes();
        app
    }

    // ── tamanho da letra ──

    fn apply_font(&self, ctx: &egui::Context) {
        ctx.style_mut(|st| {
            for (ts, base) in &self.base_sizes {
                if let Some(f) = st.text_styles.get_mut(ts) {
                    f.size = (base + self.font_delta).max(6.0);
                }
            }
        });
    }

    /// Fator em relação ao tamanho de letra por omissão do egui (12.5).
    fn ui_scale(&self) -> f32 {
        ((12.5 + self.font_delta) / 12.5).max(0.5)
    }

    /// Ctrl + roda do rato: aumenta/diminui a letra (1 pt por "clique").
    fn handle_font_zoom(&mut self, ctx: &egui::Context) {
        let (ctrl, scroll) = ctx.input(|i| (i.modifiers.command, i.raw_scroll_delta.y));
        if !ctrl {
            self.scroll_acc = 0.0;
            return;
        }
        if scroll == 0.0 {
            return;
        }
        self.scroll_acc += scroll;
        let before = self.font_delta;
        while self.scroll_acc >= 40.0 {
            self.font_delta += 1.0;
            self.scroll_acc -= 40.0;
        }
        while self.scroll_acc <= -40.0 {
            self.font_delta -= 1.0;
            self.scroll_acc += 40.0;
        }
        self.font_delta = self.font_delta.clamp(-4.0, 14.0);
        if self.font_delta != before {
            self.apply_font(ctx);
            self.status = format!("Tamanho da letra: {:.1} pt", 12.5 + self.font_delta);
        }
    }

    // ── preferências ──

    /// Recolhe o estado atual; grava no máximo 1x por segundo, e só se mudou.
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
            font_delta: self.font_delta,
            zoom: z,
            win: self.win_size,
            col_results: self.col_results,
            col_found: self.col_found,
            col_proc: self.col_proc,
            left_w: self.left_w,
            top_h: self.top_h,
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

    // ── envio de pedidos ──

    fn send_job(&mut self, req: Req) {
        if let Some(tx) = &self.req_tx {
            if tx.send(req).is_ok() {
                self.in_flight += 1;
            }
        }
    }

    /// Fecha a sessão scanmem e invalida respostas pendentes.
    fn close_backend(&mut self) {
        self.generation += 1;
        self.searching = false;
        if let Some(tx) = &self.req_tx {
            let _ = tx.send(Req::Close);
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
        let output = match Command::new("ps").args(["-axo", "pid,user,comm"]).output() {
            Ok(o) => o,
            Err(e) => {
                self.status = format!("Could not list processes: {}", e);
                return;
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        self.processes = stdout.lines().filter_map(parse_ps_line).collect();
        self.sort_processes();
        if let Some(pid) = self.selected_pid {
            if !self.processes.iter().any(|p| p.pid == pid) {
                self.selected_pid = None;
                self.selected_text = "No process selected".into();
            }
        }
        self.status = format!("{} processes listed", self.processes.len());
    }

    fn on_process_selected(&mut self, p: &ProcessRow) {
        // Outro processo: endereços, locks e pesquisa anteriores deixam de fazer sentido.
        self.clear_locks(false);
        self.close_backend();
        self.scan_has_results = false;
        self.scan_pid = None;
        self.clear_found_view();
        self.found = 0;
        self.clear_work_list();
        self.selected_pid = Some(p.pid);
        self.selected_text = format!(
            "Selected: {}  |  PID: {}  |  User: {}",
            p.command, p.pid, p.user
        );
        self.status = format!(
            "PID {} selected. Initial scan via scanmem is experimental.",
            p.pid
        );
    }

    // ── pesquisa ──

    fn on_search(&mut self) {
        let Some(pid) = self.selected_pid else {
            self.status = "Select a process before starting a search.".into();
            return;
        };
        if self.scanmem_path.is_none() || self.req_tx.is_none() {
            self.status = "scanmem not found. Install sysutils/scanmem first.".into();
            return;
        }
        let raw = self.value_input.trim().to_string();
        if raw.is_empty() {
            self.status = "Enter an integer value to search.".into();
            return;
        }
        let value = match parse_value(&raw) {
            Ok(v) => v,
            Err(ValErr::Invalid) => {
                self.status = "Invalid integer. Use decimal or 0x-prefixed hexadecimal.".into();
                return;
            }
            Err(ValErr::Range) => {
                self.status = "Value is outside the supported 32-bit integer range.".into();
                return;
            }
        };
        let first = !(self.scan_has_results && self.scan_pid == Some(pid));
        let mode = if first { "Search" } else { "Next Search" };
        self.searching = true;
        self.status = format!("{}: PID {}, value {}…", mode, pid, value);
        log_info!("button={} pid={} value={}", mode, pid, value);
        if first {
            self.clear_found_view();
        }
        let gid = self.generation;
        self.send_job(Req::Search { gid, pid, value, first });
    }

    /// Limpa a lista "Found" (resultados da pesquisa).
    fn clear_found_view(&mut self) {
        self.found_rows.clear();
        self.found_selected.clear();
        self.found_anchor = None;
    }

    /// Limpa a lista de baixo (endereços escolhidos para fixar/alterar).
    fn clear_work_list(&mut self) {
        self.result_rows.clear();
        self.selected_rows.clear();
        self.cursor = None;
        self.anchor = None;
        self.editing = None;
    }

    fn finish_search(&mut self, pid: i32, value: i64, first: bool, result: Result<SearchOut, String>) {
        let mode = if first { "Search" } else { "Next Search" };
        match result {
            Ok(out) => {
                let total = out.total;
                self.clear_found_view();
                // Só preenche as grelhas com <= 20 resultados (como no original).
                if total <= LIST_LIMIT {
                    for m in out.matches.iter().take(LIST_LIMIT) {
                        self.found_rows.push(ResultRow {
                            locked: false,
                            description: format!("Match {} ({})", m.id, m.region),
                            address: format!("0x{:x}", m.address),
                            types: m.types.clone(),
                            value: m.value.clone(),
                        });
                    }
                }
                self.found = total;
                self.scan_has_results = total > 0;
                self.scan_pid = if total > 0 { Some(pid) } else { None };
                self.status = if total > LIST_LIMIT {
                    format!(
                        "{}: {} resultados. A lista está oculta para evitar sobrecarregar a grelha; altera o valor e usa Next Search até restarem 20 ou menos.",
                        mode, total
                    )
                } else if total > 0 {
                    format!(
                        "{} encontrou {} resultados para {} no PID {}; a mostrar {}. Podes continuar com Next Search.",
                        mode, total, value, pid, total.min(out.matches.len())
                    )
                } else {
                    "Não foram encontrados resultados. Consulta bsdconqueror-debug.log.".into()
                };
            }
            Err(e) => {
                self.status = format!("Erro scanmem: {}. Consulta bsdconqueror-debug.log.", e);
                self.scan_has_results = false;
                self.scan_pid = None;
            }
        }
    }

    // ── escrita (célula / lock) ──

    /// Edição da coluna Value. Se a linha editada pertence a uma multisseleção,
    /// o mesmo valor é atribuído a todas as linhas selecionadas.
    fn commit_edit(&mut self, row: usize, text: String) {
        let Some(pid) = self.selected_pid else {
            self.status = "Seleciona um processo antes de editar valores.".into();
            return;
        };
        let Some(r) = self.result_rows.get(row) else { return };
        let address = r.address.clone();
        let was_locked = r.locked;
        if self.req_tx.is_none() {
            self.status = format!("Não foi possível alterar {}: scanmem backend indisponível", address);
            return;
        }
        let value = match parse_value(&text) {
            Ok(v) => v,
            Err(ValErr::Invalid) => {
                self.status = format!("Não foi possível alterar {}: valor inteiro inválido", address);
                return;
            }
            Err(ValErr::Range) => {
                self.status = format!(
                    "Não foi possível alterar {}: O valor tem de estar dentro do intervalo inteiro de 32 bits.",
                    address
                );
                return;
            }
        };
        let gid = self.generation;
        let multi = self.selected_rows.contains(&row) && self.selected_rows.len() > 1;
        if multi {
            let mut idx: Vec<usize> = self
                .selected_rows
                .iter()
                .copied()
                .filter(|i| *i < self.result_rows.len())
                .collect();
            idx.sort_unstable();
            let rows: Vec<(String, bool)> = idx
                .iter()
                .map(|&i| (self.result_rows[i].address.clone(), self.result_rows[i].locked))
                .collect();
            let items: Vec<WriteItem> = rows
                .iter()
                .map(|(a, _)| WriteItem { address: a.clone(), value, vtype: "int32".into() })
                .collect();
            self.status = format!("A escrever {} em {} endereço(s)…", value, items.len());
            self.send_job(Req::Write { gid, kind: WriteKind::Many { rows }, pid, items });
        } else {
            self.send_job(Req::Write {
                gid,
                kind: WriteKind::Edit { address: address.clone(), was_locked },
                pid,
                items: vec![WriteItem { address, value, vtype: "int32".into() }],
            });
        }
    }

    fn finish_write(&mut self, kind: WriteKind, results: Vec<(String, i64, WriteRes)>) {
        match kind {
            WriteKind::Edit { address, was_locked } => {
                let Some((_, value, res)) = results.into_iter().next() else { return };
                match res {
                    WriteRes::Ok => {
                        log_info!("CELL EDIT address={} type=int32 value={}", address, value);
                        if let Some(r) = self.result_rows.iter_mut().find(|r| r.address == address) {
                            r.value = value.to_string();
                        }
                        if was_locked {
                            self.locked.insert(address.clone(), (value, "int32".into()));
                        }
                        self.status = format!("Valor de {} alterado para {}.", address, value);
                    }
                    WriteRes::Failed(m) | WriteRes::Exception(m) => {
                        let m = if m.trim().is_empty() { "scanmem não confirmou a escrita".to_string() } else { m };
                        log_error!("Single-cell write failed address={}: {}", address, m);
                        self.status = format!("Não foi possível alterar {}: {}", address, m);
                    }
                }
            }
            WriteKind::Many { rows } => {
                let total = results.len();
                let value = results.first().map(|r| r.1).unwrap_or(0);
                let mut written = 0usize;
                let mut failures = 0usize;
                for (addr, val, res) in &results {
                    match res {
                        WriteRes::Ok => {
                            written += 1;
                            if let Some(row) = self.result_rows.iter_mut().find(|row| row.address == *addr) {
                                row.value = val.to_string();
                            }
                            if rows.iter().any(|(a, l)| a == addr && *l) {
                                self.locked.insert(addr.clone(), (*val, "int32".into()));
                            }
                        }
                        _ => failures += 1,
                    }
                }
                log_info!(
                    "MULTI WRITE value={} selected={} written={} failures={}",
                    value, total, written, failures
                );
                self.status = if failures > 0 {
                    format!(
                        "Valor {}: {}/{} escritos; {} falharam. Consulta bsdconqueror-debug.log.",
                        value, written, total, failures
                    )
                } else {
                    format!("Valor {} escrito em {} endereço(s).", value, written)
                };
            }
            WriteKind::Lock => {
                let mut exc: Option<String> = None;
                for (addr, _, res) in &results {
                    match res {
                        WriteRes::Failed(m) => log_warn!("LOCK write warning address={}: {}", addr, m),
                        WriteRes::Exception(e) => exc = Some(e.clone()),
                        WriteRes::Ok => {}
                    }
                }
                if let Some(e) = exc {
                    log_error!("Lock write failed: {}", e);
                    self.status = format!("Falha no lock: {}. Consulta bsdconqueror-debug.log.", e);
                    // Para de repetir locks obsoletos após falha do backend.
                    self.clear_locks(false);
                }
            }
        }
    }

    fn handle_responses(&mut self) {
        while let Ok(resp) = self.resp_rx.try_recv() {
            self.in_flight = self.in_flight.saturating_sub(1);
            match resp {
                Resp::Search { gid, pid, value, first, result } => {
                    if gid != self.generation {
                        continue;
                    }
                    self.searching = false;
                    self.finish_search(pid, value, first, result);
                }
                Resp::Refresh { gid, result } => {
                    if gid != self.generation {
                        continue;
                    }
                    match result {
                        Ok(m) => {
                            self.refresh_fails = 0;
                            self.apply_refresh(&m);
                        }
                        Err(e) => {
                            self.refresh_fails = self.refresh_fails.saturating_add(1);
                            log_warn!("Atualização de valores falhou: {}", e);
                            if self.refresh_fails == 3 {
                                self.status = "Atualização em tempo real indisponível (ver bsdconqueror-debug.log).".into();
                            }
                        }
                    }
                }
                Resp::Write { gid, kind, results } => {
                    if gid != self.generation {
                        continue;
                    }
                    self.finish_write(kind, results);
                }
            }
        }
    }

    // ── atualização em tempo real ──

    /// Pede ao scanmem os valores atuais (de ~0,7 em 0,7 s), só com a pesquisa
    /// ativa e quando não há outras operações em curso.
    fn tick_refresh(&mut self) {
        if self.req_tx.is_none()
            || self.in_flight > 0
            || self.searching
            || !self.scan_has_results
            || self.found_rows.is_empty()
            || self.refresh_fails >= 3
            || self.last_refresh.elapsed() < Duration::from_millis(700)
        {
            return;
        }
        let Some(pid) = self.scan_pid else { return };
        self.last_refresh = Instant::now();
        let gid = self.generation;
        self.send_job(Req::Refresh { gid, pid });
    }

    /// Atualiza os valores mostrados (lista Found e lista de baixo) por endereço.
    fn apply_refresh(&mut self, matches: &[ScanMatch]) {
        let editing_row = self.editing.as_ref().map(|e| e.0);
        for m in matches {
            let addr = format!("0x{:x}", m.address);
            for r in self.found_rows.iter_mut() {
                if r.address == addr {
                    r.value = m.value.clone();
                }
            }
            for (i, r) in self.result_rows.iter_mut().enumerate() {
                if r.address == addr && editing_row != Some(i) {
                    r.value = m.value.clone();
                }
            }
        }
    }

    // ── locks ──

    /// Equivalente ao timeout de 250 ms: nunca bloqueia, só enfileira.
    fn tick_locks(&mut self) {
        if self.locked.is_empty() || self.req_tx.is_none() || self.in_flight > 0 {
            return;
        }
        let Some(pid) = self.selected_pid else { return };
        if self.last_lock_tick.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_lock_tick = Instant::now();
        let items: Vec<WriteItem> = self
            .locked
            .iter()
            .map(|(a, (v, t))| WriteItem { address: a.clone(), value: *v, vtype: t.clone() })
            .collect();
        let gid = self.generation;
        self.send_job(Req::Write { gid, kind: WriteKind::Lock, pid, items });
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
            let address = row.address.clone();
            if lock {
                match parse_int(row.value.trim()) {
                    Some(v) => {
                        row.locked = true;
                        self.locked.insert(address.clone(), (v, "int32".into()));
                        log_info!("LOCK ON address={} value={} type=int32", address, v);
                        last_msg = format!("Lock ativo: {} = {} (escrita a cada 250 ms)", address, v);
                        done += 1;
                    }
                    None => {
                        last_msg = format!("Não é possível fixar o valor '{}' nesta versão.", row.value.trim());
                        bad += 1;
                    }
                }
            } else {
                row.locked = false;
                self.locked.remove(&address);
                log_info!("LOCK OFF address={}", address);
                last_msg = format!("Lock desativado: {}", address);
                done += 1;
            }
        }
        self.status = if targets.len() == 1 {
            last_msg
        } else if lock {
            format!("Lock ativo em {} endereço(s){}.", done, if bad > 0 { format!("; {} sem valor válido", bad) } else { String::new() })
        } else {
            format!("Lock desativado em {} endereço(s).", done)
        };
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
            let a = self.result_rows[i].address.clone();
            self.locked.remove(&a);
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
        self.close_backend();
        self.scan_has_results = false;
        self.scan_pid = None;
        self.clear_found_view();
        self.found = 0;
        self.value_input.clear();
        self.status = "Search controls reset.".into();
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

    /// Copia linhas da lista "Found" para a lista de baixo (sem duplicar endereços).
    fn add_found_rows(&mut self, idx: &[usize]) {
        let mut added = 0usize;
        let mut dup = 0usize;
        for &i in idx {
            let Some(r) = self.found_rows.get(i) else { continue };
            if self.result_rows.iter().any(|x| x.address == r.address) {
                dup += 1;
                continue;
            }
            let mut row = r.clone();
            row.locked = self.locked.contains_key(&row.address);
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

    fn begin_edit(&mut self, i: usize) {
        if let Some(r) = self.result_rows.get(i) {
            self.editing = Some((i, r.value.clone(), true));
            self.cursor = Some(i);
        }
    }

    /// F2: edita a linha do cursor (ou a primeira selecionada).
    fn begin_edit_cursor(&mut self) {
        let row = self
            .cursor
            .filter(|i| *i < self.result_rows.len())
            .or_else(|| self.selected_rows.iter().copied().min());
        match row {
            Some(i) => self.begin_edit(i),
            None => self.status = "Seleciona uma linha para editar o valor.".into(),
        }
    }

    // ── desenho ──

    fn ui_address_list(&mut self, ui: &mut egui::Ui) {
        let mut add_clicked = false;
        ui.horizontal(|ui| {
            title(ui, &format!("Found: {}", self.found));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can = !self.found_rows.is_empty();
                let add_btn = egui::Button::image_and_text(
                    ico(ui, egui::include_image!("icons/add-32x32.png")),
                    "Add Selected",
                );
                if ui.add_enabled(can, add_btn).clicked() {
                    add_clicked = true;
                }
            });
        });
        ui.separator();
        ui.spacing_mut().item_spacing.y = 0.0;
        let k = self.ui_scale();
        let w = [self.col_found * k, 0.0];
        let mods = ui.input(|i| i.modifiers);
        let mut click: Option<(usize, bool, bool)> = None;
        let mut dbl: Option<usize> = None;
        let (_, hr) = draw_row(ui, &w, &["Address", "Value"], &[false, false], false, false, true);
        col_handles(ui, &hr, std::slice::from_mut(&mut self.col_found), k, "found");
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, row) in self.found_rows.iter().enumerate() {
                let sel = self.found_selected.contains(&i);
                let (resp, _) = draw_row(
                    ui,
                    &w,
                    &[row.address.as_str(), row.value.as_str()],
                    &[true, true],
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
            let w = (ui.available_width() - 90.0).max(60.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.process_filter)
                    .hint_text("Filter process name, PID or user…")
                    .desired_width(w),
            );
            if ui
                .add(egui::Button::image_and_text(
                    ico(ui, egui::include_image!("icons/refresh-32x32.png")),
                    "Refresh",
                ))
                .clicked()
            {
                self.refresh_processes();
            }
        });

        // Tabela de processos
        let q = self.process_filter.trim().to_lowercase();
        let mut clicked: Option<ProcessRow> = None;
        let mut new_sort: Option<usize> = None;
        let k = self.ui_scale();
        let pw = [self.col_proc[0] * k, self.col_proc[1] * k, 0.0];
        let proc_h = 6.0 * (ui.text_style_height(&egui::TextStyle::Body) + 8.0);
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let (resp, rects) = draw_row(
                ui, &pw, &["PID", "User", "Process"], &[false; 3], false, false, true,
            );
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
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let label = if self.scan_has_results { "Next Search" } else { "Search" };
            let can = !self.searching;
            let search_btn = egui::Button::image_and_text(
                ico(ui, egui::include_image!("icons/search-32x32.png")),
                label,
            );
            if ui.add_enabled(can, search_btn).clicked() || (enter && can) {
                self.on_search();
            }
            if ui
                .add(egui::Button::image_and_text(
                    ico(ui, egui::include_image!("icons/reset-32x32.png")),
                    "Reset",
                ))
                .clicked()
            {
                self.on_reset();
            }
        });

        ui.label("Integer scan via scanmem. Lock: tick a result to keep writing its current value every 250 ms.");
    }

    fn ui_results(&mut self, ui: &mut egui::Ui) {
        // Botões centrados por cima da grelha.
        let k = self.ui_scale();
        let bw = 120.0 * k;
        let total = bw * 4.0 + ui.spacing().item_spacing.x * 3.0;
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
            let b = egui::Button::image_and_text(
                ico(ui, egui::include_image!("icons/select_all-32x32.png")),
                "Select all",
            );
            if ui.add_sized([bw, 26.0 * k], b).clicked() {
                self.select_all();
            }
            let b = egui::Button::image_and_text(
                ico(ui, egui::include_image!("icons/clear-32x32.png")),
                "Clear results",
            );
            if ui.add_sized([bw, 26.0 * k], b).clicked() {
                self.on_clear();
            }
            if ui
                .add_sized([bw, 26.0 * k], egui::Button::new("Remove"))
                .on_hover_text("Remove as linhas selecionadas (tecla Delete)")
                .clicked()
            {
                self.remove_selected();
            }
            let b = egui::Button::image_and_text(
                ico(ui, egui::include_image!("icons/quit-32x32.png")),
                "Quit",
            );
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
        let mut start_edit: Option<usize> = None;
        let mut commit: Option<(usize, String)> = None;
        let mut cancel = false;

        let cr = self.col_results;
        let w = [cr[0] * k, cr[1] * k, cr[2] * k, cr[3] * k, 0.0];
        ui.spacing_mut().item_spacing.y = 0.0;
        let (_, hr) = draw_row(
            ui, &w, &["Lock", "Description", "Address", "Type", "Value"], &[false; 5], false, false, true,
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
                let editing_here = matches!(&editing, Some((r, _, _)) if *r == i);
                let value_text = if editing_here { "" } else { row.value.as_str() };
                let (resp, rects) = draw_row(
                    ui,
                    &w,
                    &["", row.description.as_str(), row.address.as_str(), row.types.as_str(), value_text],
                    &[false, false, true, true, true],
                    sel,
                    i % 2 == 1,
                    false,
                );

                let lock_img = if row.locked {
                    ico(ui, egui::include_image!("icons/lock-32x32.png"))
                } else {
                    ico(ui, egui::include_image!("icons/unlock-32x32.png"))
                };
                if ui.put(rects[0], egui::ImageButton::new(lock_img).frame(false)).clicked() {
                    toggle = Some(i);
                }

                if let Some((r, buf, need_focus)) = editing.as_mut() {
                    if *r == i {
                        let er = ui.put(
                            rects[4].shrink2(egui::vec2(4.0, 1.0)),
                            egui::TextEdit::singleline(buf)
                                .id(egui::Id::new(("cell_edit", i)))
                                .font(egui::TextStyle::Monospace)
                                .desired_width(rects[4].width() - 8.0),
                        );
                        if *need_focus {
                            er.request_focus();
                            *need_focus = false;
                        }
                        if er.lost_focus() {
                            if esc {
                                cancel = true;
                            } else {
                                commit = Some((i, buf.clone()));
                            }
                        }
                    }
                }

                if !editing_here {
                    let mut handled = false;
                    if resp.double_clicked() {
                        // Duplo clique na coluna Value (ou F2) edita o valor.
                        let on_value = resp
                            .interact_pointer_pos()
                            .map_or(false, |p| p.x >= rects[4].left());
                        if on_value {
                            start_edit = Some(i);
                            handled = true;
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
        if let Some((i, text)) = commit {
            self.editing = None;
            self.commit_edit(i, text);
        }
        if let Some(i) = start_edit {
            self.begin_edit(i);
        }
        if let Some(i) = toggle {
            self.on_lock_toggled(i);
        }
        if let Some((i, ctrl, shift)) = click {
            self.click_row(i, ctrl, shift);
        }
    }
}

impl eframe::App for BSDConqueror {
    /// FreeBSD/X11: muitas configurações mapeiam a tecla Backspace para o
    /// keysym "Delete". O egui então só apaga *para a frente* e nada acontece
    /// no fim do texto. A tecla física continua a ser Backspace, por isso
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
        self.handle_font_zoom(ctx);
        self.tick_locks();
        self.tick_refresh();
        ctx.request_repaint_after(Duration::from_millis(100));

        if !ctx.wants_keyboard_input() {
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
                        self.ui_address_list(ui);
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

        // X11/XIM: com o IME ativo, setas/Backspace podem ser "engolidos" pelo
        // método de entrada. Esta aplicação só precisa de números, por isso
        // desativamos o IME em todas as caixas de texto.
        ctx.output_mut(|o| o.ime = None);
    }
}

impl Drop for BSDConqueror {
    fn drop(&mut self) {
        self.flush_prefs();
        log_info!("Aplicação encerrada");
        if let Some(tx) = &self.req_tx {
            let _ = tx.send(Req::Shutdown);
        }
        // Só espera pela thread se não houver operação longa em curso;
        // caso contrário o scanmem termina sozinho ao fechar o stdin.
        if self.in_flight == 0 {
            if let Some(h) = self.worker.take() {
                let _ = h.join();
            }
        }
    }
}

/// Ícone da janela (src/icons/GameConqueror.png).
fn load_icon() -> Option<egui::IconData> {
    let bytes = include_bytes!("icons/GameConqueror.png");
    let img = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData { rgba: img.into_raw(), width, height })
}

fn main() -> Result<(), eframe::Error> {
    init_log();
    log_info!("BSDConqueror {} iniciado", APP_VERSION);
    let prefs = Prefs::load();
    let (win_w, win_h) = prefs.win.unwrap_or((920.0, 650.0));
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([win_w, win_h])
        .with_app_id("bsdconqueror");
    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        &format!("BSDConqueror {}", APP_VERSION),
        options,
        Box::new(move |cc| Ok(Box::new(BSDConqueror::new(cc, prefs)))),
    )
}
