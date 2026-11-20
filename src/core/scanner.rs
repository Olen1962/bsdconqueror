// src/core/scanner.rs
//
// Pesquisa de memória usando EXCLUSIVAMENTE ferramentas nativas do FreeBSD:
//   * `ps`           -> lista de processos
//   * `procstat -v`    -> mapas de memória do processo
//   * `ptrace(2)`      -> PT_ATTACH / PT_IO (PIOD_READ_D / PIOD_WRITE_D) / PT_DETACH
//
// Não precisa de crates externas (ptrace/waitpid vêm da libc do sistema).
// Requer root (ou security.bsd.unprivileged_proc_debug=1 e o mesmo utilizador).
//
// Alterações em relação à versão anterior:
//   * `list_processes()` também existe como função livre;
//   * `write_values()` e `sync()` fazem várias escritas/leituras com UM só attach
//     (o processo alvo fica parado menos vezes);
//   * `with_cancel()` permite interromper pesquisas longas (ao fechar a aplicação).

use std::cmp::Ordering as CmpOrdering;
use std::collections::HashMap;
use std::ffi::c_void;
use std::os::raw::{c_char, c_int};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// ───────────── Interface nativa (sys/ptrace.h do FreeBSD) ─────────────

const PT_ATTACH: c_int = 10;
const PT_DETACH: c_int = 11;
const PT_IO: c_int = 12;
const PIOD_READ_D: c_int = 1;
const PIOD_WRITE_D: c_int = 2;

#[repr(C)]
struct PtraceIoDesc {
    piod_op: c_int,
    piod_offs: *mut c_void, // endereço no processo alvo
    piod_addr: *mut c_void, // buffer local
    piod_len: usize,
}

unsafe extern "C" {
    fn ptrace(request: c_int, pid: c_int, addr: *mut c_char, data: c_int) -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
}

/// Anexa-se ao processo (que fica parado) e desanexa-se sempre no Drop.
struct Attached {
    pid: i32,
}

impl Attached {
    fn new(pid: i32) -> Result<Self, String> {
        let r = unsafe { ptrace(PT_ATTACH, pid, std::ptr::null_mut(), 0) };
        if r == -1 {
            return Err(format!(
                "PT_ATTACH falhou para o PID {}: {}. Execute como root (ou ative security.bsd.unprivileged_proc_debug).",
                pid,
                std::io::Error::last_os_error()
            ));
        }
        let mut status: c_int = 0;
        let w = unsafe { waitpid(pid, &mut status, 0) };
        if w == -1 {
            let e = std::io::Error::last_os_error();
            unsafe { ptrace(PT_DETACH, pid, std::ptr::dangling_mut::<c_char>(), 0) };
            return Err(format!("waitpid falhou para o PID {}: {}", pid, e));
        }
        Ok(Self { pid })
    }

    /// Lê até `buf.len()` bytes do endereço `addr` do processo alvo.
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<usize, String> {
        let mut d = PtraceIoDesc {
            piod_op: PIOD_READ_D,
            piod_offs: addr as usize as *mut c_void,
            piod_addr: buf.as_mut_ptr() as *mut c_void,
            piod_len: buf.len(),
        };
        let r = unsafe {
            ptrace(
                PT_IO,
                self.pid,
                &mut d as *mut PtraceIoDesc as *mut c_char,
                0,
            )
        };
        if r == -1 {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(d.piod_len)
        }
    }

    /// Escreve `buf` no endereço `addr` do processo alvo.
    fn write(&self, addr: u64, buf: &[u8]) -> Result<usize, String> {
        let mut d = PtraceIoDesc {
            piod_op: PIOD_WRITE_D,
            piod_offs: addr as usize as *mut c_void,
            piod_addr: buf.as_ptr() as *mut c_void,
            piod_len: buf.len(),
        };
        let r = unsafe {
            ptrace(
                PT_IO,
                self.pid,
                &mut d as *mut PtraceIoDesc as *mut c_char,
                0,
            )
        };
        if r == -1 {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(d.piod_len)
        }
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        unsafe { ptrace(PT_DETACH, self.pid, std::ptr::dangling_mut::<c_char>(), 0) };
    }
}

// ───────────── Tipos públicos ─────────────

#[derive(Clone, Debug)]
pub struct ProcessInfo {
    pub pid: i32,
    pub user: String,
    pub command: String,
}

#[derive(Clone, Debug)]
pub struct MemoryMap {
    pub start: u64,
    pub end: u64,
    pub perm: String,
    pub kind: String,
    #[allow(dead_code)]
    pub path: String,
}

/// Valor numérico: inteiro ou vírgula flutuante (f32/f64).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Num {
    Int(i64),
    F32(f32),
    F64(f64),
}

/// Como comparar o valor atual com o valor guardado numa pesquisa de valor desconhecido.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparison {
    Increased,
    Decreased,
    Changed,
    Unchanged,
    Equal,
}

/// Compara dois valores do mesmo tipo. Devolve `None` para floats NaN/incompatíveis.
fn compare_nums(a: Num, b: Num) -> Option<CmpOrdering> {
    match (a, b) {
        (Num::Int(x), Num::Int(y)) => Some(x.cmp(&y)),
        (Num::F32(x), Num::F32(y)) => x.partial_cmp(&y),
        (Num::F64(x), Num::F64(y)) => x.partial_cmp(&y),
        _ => {
            // Mistura só acontece com valores normalizados pelo chamador; inteiros
            // não podem passar por f64 sem perder precisão.
            if let (Some(x), Some(y)) = (a.as_int(), b.as_int()) {
                Some(x.cmp(&y))
            } else {
                a.as_f64().partial_cmp(&b.as_f64())
            }
        }
    }
}

fn matches_comparison(old: Num, new: Num, comparison: Comparison, target: Option<Num>) -> bool {
    match comparison {
        Comparison::Increased => compare_nums(new, old) == Some(CmpOrdering::Greater),
        Comparison::Decreased => compare_nums(new, old) == Some(CmpOrdering::Less),
        Comparison::Changed => new != old,
        Comparison::Unchanged => new == old,
        Comparison::Equal => target.is_some_and(|t| new == t),
    }
}

impl Num {
    pub fn as_f64(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::F32(f) => f as f64,
            Num::F64(f) => f,
        }
    }

    /// Valor como inteiro, se for exatamente um (p.ex. 100 ou 100.0).
    pub fn as_int(self) -> Option<i64> {
        match self {
            Num::Int(i) => Some(i),
            Num::F32(_) | Num::F64(_) => {
                let f = self.as_f64();
                if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e18 {
                    Some(f as i64)
                } else {
                    None
                }
            }
        }
    }
}

impl std::fmt::Display for Num {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Num::Int(i) => write!(f, "{}", i),
            Num::F32(x) => write!(f, "{}", x), // f32 mostra "0.1", não "0.10000000149…"
            Num::F64(x) => write!(f, "{}", x),
        }
    }
}

pub struct ScanResult {
    pub total: usize,
    pub shown: Vec<(u64, usize, Num)>,
    pub truncated: bool,
    pub kept_previous: bool,
    pub sample: Vec<Num>,
}

/// Larguras: 1, 2, 4, 8 = inteiros (bytes); 5 = float32 (4 bytes); 6 = double (8 bytes).
pub const ALL_WIDTHS: [usize; 6] = [1, 2, 4, 5, 6, 8];
pub const SHOW_LIMIT: usize = 20;
const MAX_CANDIDATES: usize = 16_000_000;
/// Limite do modo de valor desconhecido (guarda endereço + valor de cada posição).
const MAX_UNKNOWN_CANDIDATES: usize = 8_000_000;
const CHUNK: usize = 1 << 20;
/// Quantas listas de candidatos anteriores se guardam para o "Undo".
const HISTORY_DEPTH: usize = 5;
/// Listas maiores do que isto não se guardam (ocupariam demasiada memória).
const HISTORY_MAX_CANDIDATES: usize = 2_000_000;
/// No modo desconhecido cada candidato também ocupa um `Num`; guardar menos histórico.
const HISTORY_MAX_UNKNOWN: usize = 1_000_000;
/// Bytes lidos a mais em cada bloco, para apanhar valores entre dois blocos.
const OVERLAP: usize = 7;

// 3 bits de etiqueta (0..=5) nos bits altos do endereço.
const TAG_SHIFT: u32 = 61;
const ADDR_MASK: u64 = (1u64 << TAG_SHIFT) - 1;

fn tag_of(width: usize) -> usize {
    match width {
        1 => 0,
        2 => 1,
        4 => 2, // inteiro de 4 bytes
        5 => 3, // float32
        6 => 4, // double
        _ => 5, // 8 bytes inteiro
    }
}

fn pack(addr: u64, width: usize) -> u64 {
    addr | ((tag_of(width) as u64) << TAG_SHIFT)
}

fn unpack(p: u64) -> (u64, usize) {
    let w = match p >> TAG_SHIFT {
        0 => 1,
        1 => 2,
        2 => 4,
        3 => 5,
        4 => 6,
        _ => 8,
    };
    (p & ADDR_MASK, w)
}

/// Número de bytes que a largura `width` ocupa na memória (5 = f32 -> 4, 6 = f64 -> 8).
fn byte_len(width: usize) -> usize {
    match width {
        1 => 1,
        2 => 2,
        4 | 5 => 4,
        _ => 8,
    }
}

fn pattern(value: Num, width: usize) -> Result<Vec<u8>, String> {
    match width {
        1 | 2 | 4 | 8 => {
            let v = value
                .as_int()
                .ok_or_else(|| format!("O valor {} não é um inteiro.", value))?;
            match width {
                1 => {
                    if !(-128..=255).contains(&v) {
                        return Err(format!(
                            "Valor {} fora do intervalo de 1 byte (-128 a 255).",
                            v
                        ));
                    }
                    Ok(v.to_ne_bytes()[..1].to_vec())
                }
                2 => {
                    if !(-32_768..=65_535).contains(&v) {
                        return Err(format!(
                            "Valor {} fora do intervalo de 2 bytes (-32768 a 65535).",
                            v
                        ));
                    }
                    Ok(v.to_ne_bytes()[..2].to_vec())
                }
                4 => {
                    if v < i32::MIN as i64 || v > i32::MAX as i64 {
                        return Err(format!(
                            "Valor {} fora do intervalo de 4 bytes (inteiro com sinal).",
                            v
                        ));
                    }
                    Ok((v as i32).to_ne_bytes().to_vec())
                }
                _ => Ok(v.to_ne_bytes().to_vec()),
            }
        }
        5 => {
            let f = value.as_f64() as f32;
            if value.as_f64().is_finite() && !f.is_finite() {
                return Err(format!(
                    "Valor {} fora do intervalo de um float de 4 bytes.",
                    value
                ));
            }
            Ok(f.to_ne_bytes().to_vec())
        }
        6 => Ok(value.as_f64().to_ne_bytes().to_vec()),
        _ => Err(format!("Largura de {} bytes não suportada.", width)),
    }
}

fn decode(b: &[u8], width: usize) -> Num {
    match width {
        1 => Num::Int(b[0] as i8 as i64),
        2 => Num::Int(i16::from_ne_bytes([b[0], b[1]]) as i64),
        4 => Num::Int(i32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as i64),
        5 => Num::F32(f32::from_ne_bytes([b[0], b[1], b[2], b[3]])),
        6 => Num::F64(f64::from_ne_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ])),
        _ => Num::Int(i64::from_ne_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ])),
    }
}

fn build_patterns(value: Num, widths: &[usize]) -> Result<[Option<Vec<u8>>; 6], String> {
    let mut pats: [Option<Vec<u8>>; 6] = [None, None, None, None, None, None];
    let mut first_err: Option<String> = None;
    for &w in widths {
        match pattern(value, w) {
            Ok(p) => pats[tag_of(w)] = Some(p),
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }
    if pats.iter().all(|p| p.is_none()) {
        return Err(first_err.unwrap_or_else(|| "Nenhum tipo selecionado.".to_string()));
    }
    Ok(pats)
}

/// Lê vários valores com um attach já feito.
fn read_with(att: &Attached, items: &[(u64, usize)]) -> Vec<Option<Num>> {
    let mut out = Vec::with_capacity(items.len());
    let mut b = [0u8; 8];
    for &(addr, w) in items {
        let n = byte_len(w);
        match att.read(addr, &mut b[..n]) {
            Ok(got) if got == n => out.push(Some(decode(&b[..n], w))),
            _ => out.push(None),
        }
    }
    out
}

/// Escreve vários valores com um attach já feito.
fn write_with(att: &Attached, items: &[(u64, usize, Num)]) -> Vec<Result<(), String>> {
    items
        .iter()
        .map(|&(addr, w, v)| -> Result<(), String> {
            let pat = pattern(v, w)?;
            let n = att.write(addr, &pat)?;
            if n != pat.len() {
                return Err(format!(
                    "Escrita parcial em {:#x} ({} de {} bytes).",
                    addr,
                    n,
                    pat.len()
                ));
            }
            Ok(())
        })
        .collect()
}

/// Lista de processos (`ps -axo pid,user,comm`).
pub fn list_processes() -> Vec<ProcessInfo> {
    let output = match Command::new("ps").args(["-axo", "pid,user,comm"]).output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut processes = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        let mut parts = line.splitn(2, char::is_whitespace);
        if let (Some(pid_str), Some(rest)) = (parts.next(), parts.next()) {
            if pid_str.eq_ignore_ascii_case("PID") {
                continue;
            }
            let rest = rest.trim_start();
            let mut rest_parts = rest.splitn(2, char::is_whitespace);
            if let (Some(user), Some(comm)) = (rest_parts.next(), rest_parts.next())
                && let Ok(pid) = pid_str.parse::<i32>()
            {
                processes.push(ProcessInfo {
                    pid,
                    user: user.to_string(),
                    command: comm.trim_start().to_string(),
                });
            }
        }
    }
    processes
}

/// Lista de candidatos antes de uma pesquisa, com os valores do modo desconhecido.
#[derive(Default)]
struct Snapshot {
    candidates: Vec<u64>,
    values: Vec<Num>,
}

pub struct MemoryScanner {
    pub target_pid: Option<i32>,
    candidates: Vec<u64>,
    /// Valores paralelos a `candidates`, usados pelo modo de valor desconhecido.
    unknown_values: Vec<Num>,
    /// Listas de candidatos anteriores (a última é a mais recente), para o Undo.
    history: Vec<Snapshot>,
    cancel: Arc<AtomicBool>,
}

impl MemoryScanner {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::with_cancel(Arc::new(AtomicBool::new(false)))
    }

    /// `cancel`: quando passa a `true`, pesquisas longas terminam com erro.
    pub fn with_cancel(cancel: Arc<AtomicBool>) -> Self {
        Self {
            target_pid: None,
            candidates: Vec::new(),
            unknown_values: Vec::new(),
            history: Vec::new(),
            cancel,
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn attach_to_pid(&mut self, pid: i32) -> Result<(), String> {
        if self.target_pid != Some(pid) {
            self.candidates.clear();
            self.unknown_values.clear();
            self.history.clear(); // o histórico pertence ao processo anterior
        }
        self.target_pid = Some(pid);
        Ok(())
    }

    pub fn reset_scan(&mut self) {
        self.candidates.clear();
        self.unknown_values.clear();
        self.history.clear();
    }

    /// Guarda a lista atual (endereços e valores) no histórico, antes de ser substituída.
    fn remember(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        let limit = if self.unknown_values.is_empty() {
            HISTORY_MAX_CANDIDATES
        } else {
            HISTORY_MAX_UNKNOWN
        };
        if self.candidates.len() > limit {
            // Não dá para voltar a esta lista; as mais antigas deixariam de ser consistentes.
            self.history.clear();
            return;
        }
        if self.history.len() >= HISTORY_DEPTH {
            self.history.remove(0);
        }
        self.history.push(Snapshot {
            candidates: self.candidates.clone(),
            values: self.unknown_values.clone(),
        });
    }

    /// Quantas pesquisas se podem desfazer.
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// Desfaz a última pesquisa: repõe a lista de candidatos anterior.
    /// No modo exato os valores devolvidos são 0; quem chama deve lê-los da memória.
    pub fn undo_scan(&mut self) -> Result<ScanResult, String> {
        let prev = self.history.pop().ok_or("Nada para desfazer.")?;
        self.candidates = prev.candidates;
        self.unknown_values = prev.values;
        if self.unknown_values.is_empty() {
            Ok(self.build_result(Num::Int(0), false))
        } else {
            Ok(self.build_unknown_result(false))
        }
    }

    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    /// Há valores guardados de uma pesquisa de valor desconhecido?
    pub fn has_unknown_values(&self) -> bool {
        !self.unknown_values.is_empty()
    }

    #[allow(dead_code)]
    pub fn list_processes(&self) -> Vec<ProcessInfo> {
        list_processes()
    }

    pub fn list_memory_maps(&self, pid: i32) -> Result<Vec<MemoryMap>, String> {
        let output = Command::new("procstat")
            .args(["-v", &pid.to_string()])
            .output()
            .map_err(|e| format!("Falha ao executar procstat: {}", e))?;

        if !output.status.success() {
            return Err(format!("procstat terminou com erro para o PID {}.", pid));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut maps = Vec::new();

        for line in stdout.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 4 || !parts[1].starts_with("0x") || !parts[2].starts_with("0x") {
                continue;
            }
            let (Ok(start), Ok(end)) = (
                u64::from_str_radix(&parts[1][2..], 16),
                u64::from_str_radix(&parts[2][2..], 16),
            ) else {
                continue;
            };
            maps.push(MemoryMap {
                start,
                end,
                perm: parts[3].to_string(),
                kind: parts.get(9).copied().unwrap_or("").to_string(),
                path: parts.get(10..).map(|p| p.join(" ")).unwrap_or_default(),
            });
        }
        Ok(maps)
    }

    #[allow(dead_code)]
    pub fn read_values(&self, items: &[(u64, usize)]) -> Result<Vec<Option<Num>>, String> {
        let pid = self.target_pid.ok_or("Nenhum processo selecionado!")?;
        let att = Attached::new(pid)?;
        Ok(read_with(&att, items))
    }

    #[allow(dead_code)]
    pub fn write_value(&self, addr: u64, width: usize, value: Num) -> Result<(), String> {
        let pid = self.target_pid.ok_or("Nenhum processo selecionado!")?;
        let pat = pattern(value, width)?;
        let att = Attached::new(pid)?;
        att.write(addr, &pat)?;
        Ok(())
    }

    /// Várias escritas com um único attach. O erro externo só ocorre se o
    /// attach falhar; cada escrita tem o seu próprio resultado.
    pub fn write_values(
        &self,
        items: &[(u64, usize, Num)],
    ) -> Result<Vec<Result<(), String>>, String> {
        let pid = self.target_pid.ok_or("Nenhum processo selecionado!")?;
        let att = Attached::new(pid)?;
        Ok(write_with(&att, items))
    }

    /// Escritas (locks) + leituras (valores atuais) com um único attach.
    #[allow(clippy::type_complexity)]
    pub fn sync(
        &self,
        writes: &[(u64, usize, Num)],
        reads: &[(u64, usize)],
    ) -> Result<(Vec<Result<(), String>>, Vec<Option<Num>>), String> {
        let pid = self.target_pid.ok_or("Nenhum processo selecionado!")?;
        let att = Attached::new(pid)?;
        let w = write_with(&att, writes);
        let r = read_with(&att, reads);
        Ok((w, r))
    }

    /// Resultado para mostrar: os primeiros SHOW_LIMIT endereços DISTINTOS.
    /// Se o mesmo endereço aparece com várias larguras (p.ex. 2 e 4 bytes,
    /// porque o valor cabe nas duas), mostra-se só a mais estreita. Os candidatos
    /// guardados não são alterados: a pesquisa seguinte continua a usar todos.
    fn build_result(&self, value: Num, truncated: bool) -> ScanResult {
        let mut order: Vec<u64> = Vec::new();
        let mut min_width: HashMap<u64, usize> = HashMap::new();
        for p in &self.candidates {
            let (a, w) = unpack(*p);
            match min_width.get_mut(&a) {
                Some(m) => {
                    if w < *m {
                        *m = w;
                    }
                }
                None => {
                    if order.len() < SHOW_LIMIT {
                        order.push(a);
                        min_width.insert(a, w);
                    }
                }
            }
        }
        ScanResult {
            total: self.candidates.len(),
            shown: order.iter().map(|a| (*a, min_width[a], value)).collect(),
            truncated,
            kept_previous: false,
            sample: Vec::new(),
        }
    }

    /// Resultado do modo desconhecido: cada candidato tem um tipo e um valor guardado.
    fn build_unknown_result(&self, truncated: bool) -> ScanResult {
        let shown = self
            .candidates
            .iter()
            .zip(self.unknown_values.iter())
            .take(SHOW_LIMIT)
            .map(|(&p, &v)| {
                let (a, w) = unpack(p);
                (a, w, v)
            })
            .collect();
        ScanResult {
            total: self.candidates.len(),
            shown,
            truncated,
            kept_previous: false,
            sample: Vec::new(),
        }
    }

    pub fn perform_first_scan(
        &mut self,
        value: Num,
        widths: &[usize],
    ) -> Result<ScanResult, String> {
        let pid = self.target_pid.ok_or("Nenhum processo selecionado!")?;
        let tags = build_patterns(value, widths)?;
        let pats: Vec<(usize, &Vec<u8>)> = ALL_WIDTHS
            .iter()
            .filter_map(|&w| tags[tag_of(w)].as_ref().map(|p| (w, p)))
            .collect();

        let regions: Vec<MemoryMap> = self
            .list_memory_maps(pid)?
            .into_iter()
            .filter(|m| m.perm.starts_with("rw") && !matches!(m.kind.as_str(), "dv" | "gd" | "ph"))
            .collect();

        if regions.is_empty() {
            return Err(format!(
                "Nenhuma região de memória legível/escritável no PID {}.",
                pid
            ));
        }

        let att = Attached::new(pid)?;
        let mut found: Vec<u64> = Vec::new();
        let mut buf = vec![0u8; CHUNK + OVERLAP];
        let mut truncated = false;

        // Pesquisa em TODOS os endereços (passo de 1 byte). Jogos DOS dentro de
        // emuladores (DOSBox, etc.) guardam valores de 2/4 bytes em endereços
        // ímpares/desalinhados, que uma pesquisa alinhada nunca encontra.
        'regions: for map in &regions {
            let mut addr = map.start;
            while addr < map.end {
                if self.cancelled() {
                    return Err("Pesquisa cancelada.".to_string());
                }
                let remain = (map.end - addr) as usize;
                let want = remain.min(CHUNK + OVERLAP);
                // Posições de início a verificar neste bloco (sem repetir as do seguinte).
                let limit = remain.min(CHUNK);
                let got = match att.read(addr, &mut buf[..want]) {
                    Ok(n) if n > 0 => n,
                    _ => {
                        addr += limit as u64;
                        continue;
                    }
                };
                let hay = &buf[..got];
                for (code, pat) in &pats {
                    let w = pat.len();
                    let code = *code;
                    if got < w {
                        continue;
                    }
                    let upto = limit.min(got - w + 1);
                    let first = pat[0];
                    let mut i = 0usize;
                    while i < upto {
                        match hay[i..upto].iter().position(|&b| b == first) {
                            None => break,
                            Some(p) => {
                                i += p;
                                if &hay[i..i + w] == pat.as_slice() {
                                    found.push(pack(addr + i as u64, code));
                                    if found.len() >= MAX_CANDIDATES {
                                        truncated = true;
                                        break 'regions;
                                    }
                                }
                                i += 1;
                            }
                        }
                    }
                }
                addr += limit as u64;
            }
        }
        drop(att);
        self.remember();
        self.candidates = found;
        self.unknown_values.clear();
        Ok(self.build_result(value, truncated))
    }

    pub fn perform_next_scan(
        &mut self,
        value: Num,
        widths: &[usize],
    ) -> Result<ScanResult, String> {
        let pid = self.target_pid.ok_or("Nenhum processo selecionado!")?;
        if self.candidates.is_empty() {
            return Err("Não há endereços anteriores.".to_string());
        }
        let pats = build_patterns(value, widths)?;
        let att = Attached::new(pid)?;
        let mut keep: Vec<u64> = Vec::with_capacity(self.candidates.len().min(1 << 16));
        let mut sample: Vec<Num> = Vec::new();
        let mut b = [0u8; 8];

        for (n, &p) in self.candidates.iter().enumerate() {
            if n & 0xFFF == 0 && self.cancelled() {
                return Err("Pesquisa cancelada.".to_string());
            }
            let (addr, w) = unpack(p);
            let Some(pat) = &pats[tag_of(w)] else {
                continue;
            };
            let n = byte_len(w);
            match att.read(addr, &mut b[..n]) {
                Ok(got) if got == n => {
                    if sample.len() < 5 {
                        sample.push(decode(&b[..n], w));
                    }
                    if &b[..n] == pat.as_slice() {
                        keep.push(p);
                    }
                }
                _ => {}
            }
        }
        drop(att);

        if keep.is_empty() {
            return Ok(ScanResult {
                total: 0,
                shown: Vec::new(),
                truncated: false,
                kept_previous: true,
                sample,
            });
        }

        self.remember();
        self.candidates = keep;
        self.unknown_values.clear();
        let mut r = self.build_result(value, false);
        r.sample = sample;
        Ok(r)
    }

    /// Primeira pesquisa de valor desconhecido: guarda um instantâneo de cada
    /// posição alinhada ao tamanho do tipo, com o valor atual.
    pub fn perform_unknown_first_scan(&mut self, width: usize) -> Result<ScanResult, String> {
        let pid = self.target_pid.ok_or("Nenhum processo selecionado!")?;
        if !ALL_WIDTHS.contains(&width) {
            return Err(format!("Largura de {} bytes não suportada.", width));
        }
        let size = byte_len(width);
        let regions: Vec<MemoryMap> = self
            .list_memory_maps(pid)?
            .into_iter()
            .filter(|m| m.perm.starts_with("rw") && !matches!(m.kind.as_str(), "dv" | "gd" | "ph"))
            .collect();

        if regions.is_empty() {
            return Err(format!(
                "Nenhuma região de memória legível/escritável no PID {}.",
                pid
            ));
        }

        let att = Attached::new(pid)?;
        let mut found: Vec<u64> = Vec::new();
        let mut values: Vec<Num> = Vec::new();
        let mut buf = vec![0u8; CHUNK + OVERLAP];
        let mut truncated = false;

        // Só posições alinhadas ao tamanho do tipo: o modo desconhecido compara
        // valores (não bytes) e não pode guardar todas as posições de 1 em 1 byte.
        'regions: for map in &regions {
            let mut addr = map.start;
            let misalign = addr % size as u64;
            if misalign != 0 {
                addr += size as u64 - misalign;
            }
            while addr < map.end {
                if self.cancelled() {
                    return Err("Pesquisa cancelada.".to_string());
                }
                let remain = (map.end - addr) as usize;
                let want = remain.min(CHUNK + OVERLAP);
                // CHUNK é divisível por todos os tamanhos suportados (1, 2, 4 e 8).
                let limit = remain.min(CHUNK);
                let got = match att.read(addr, &mut buf[..want]) {
                    Ok(n) if n > 0 => n,
                    _ => {
                        addr += limit as u64;
                        continue;
                    }
                };
                let mut i = 0usize;
                while i < limit && i + size <= got {
                    found.push(pack(addr + i as u64, width));
                    values.push(decode(&buf[i..i + size], width));
                    if found.len() >= MAX_UNKNOWN_CANDIDATES {
                        truncated = true;
                        break 'regions;
                    }
                    i += size;
                }
                addr += limit as u64;
            }
        }
        drop(att);

        self.remember();
        self.candidates = found;
        self.unknown_values = values;
        Ok(self.build_unknown_result(truncated))
    }

    /// Pesquisa seguinte do modo desconhecido: compara o valor atual de cada
    /// candidato com o valor guardado no último instantâneo.
    pub fn perform_unknown_next_scan(
        &mut self,
        width: usize,
        comparison: Comparison,
        value: Option<Num>,
    ) -> Result<ScanResult, String> {
        let pid = self.target_pid.ok_or("Nenhum processo selecionado!")?;
        if self.candidates.is_empty() {
            return Err("Não há endereços anteriores.".to_string());
        }
        if self.unknown_values.len() != self.candidates.len() {
            return Err(
                "Esta pesquisa não tem valores guardados. Faz primeiro um instantâneo.".to_string(),
            );
        }
        if !ALL_WIDTHS.contains(&width) {
            return Err(format!("Largura de {} bytes não suportada.", width));
        }

        // Normalizar o valor de "igual a" para o tipo/largura em causa
        // (100 e 100.0 têm de comparar igual; um inteiro 100 num float vira f32).
        let target = if comparison == Comparison::Equal {
            let v = value.ok_or("Indica um valor para a comparação de igualdade.".to_string())?;
            Some(decode(&pattern(v, width)?, width))
        } else {
            None
        };

        let att = Attached::new(pid)?;
        let mut keep: Vec<u64> = Vec::with_capacity(self.candidates.len().min(1 << 16));
        let mut keep_values: Vec<Num> = Vec::with_capacity(keep.capacity());
        let mut sample: Vec<Num> = Vec::new();
        let mut b = [0u8; 8];

        for (n, &p) in self.candidates.iter().enumerate() {
            if n & 0xFFF == 0 && self.cancelled() {
                return Err("Pesquisa cancelada.".to_string());
            }
            let (addr, w) = unpack(p);
            if w != width {
                continue;
            }
            let size = byte_len(w);
            match att.read(addr, &mut b[..size]) {
                Ok(got) if got == size => {
                    let new = decode(&b[..size], w);
                    if sample.len() < 5 {
                        sample.push(new);
                    }
                    if matches_comparison(self.unknown_values[n], new, comparison, target) {
                        keep.push(p);
                        keep_values.push(new);
                    }
                }
                _ => {}
            }
        }
        drop(att);

        if keep.is_empty() {
            return Ok(ScanResult {
                total: 0,
                shown: Vec::new(),
                truncated: false,
                kept_previous: true,
                sample,
            });
        }

        self.remember();
        self.candidates = keep;
        self.unknown_values = keep_values;
        let mut r = self.build_unknown_result(false);
        r.sample = sample;
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_e_decode_inteiros() {
        for &(v, w) in &[
            (0i64, 1usize),
            (127, 1),
            (-128, 1),
            (0, 2),
            (32767, 2),
            (-32768, 2),
            (0, 4),
            (i32::MAX as i64, 4),
            (i32::MIN as i64, 4),
            (i64::MIN, 8),
            (i64::MAX, 8),
        ] {
            let p = pattern(Num::Int(v), w).unwrap();
            assert_eq!(p.len(), byte_len(w));
            assert_eq!(decode(&p, w), Num::Int(v), "valor {} largura {}", v, w);
        }
    }

    #[test]
    fn pattern_rejeita_inteiros_fora_do_intervalo() {
        assert!(pattern(Num::Int(300), 1).is_err());
        assert!(pattern(Num::Int(-129), 1).is_err());
        assert!(pattern(Num::Int(70_000), 2).is_err());
        assert!(pattern(Num::Int(i64::MAX), 4).is_err());
    }

    #[test]
    fn pattern_e_decode_floats() {
        let p = pattern(Num::F32(1.5), 5).unwrap();
        assert_eq!(p.len(), 4);
        assert_eq!(decode(&p, 5), Num::F32(1.5));

        let p = pattern(Num::F64(0.1), 5).unwrap();
        assert_eq!(decode(&p, 5), Num::F32(0.1));

        let p = pattern(Num::F64(1.25), 6).unwrap();
        assert_eq!(p.len(), 8);
        assert_eq!(decode(&p, 6), Num::F64(1.25));

        let p = pattern(Num::F32(2.5), 6).unwrap();
        assert_eq!(decode(&p, 6), Num::F64(2.5));
    }

    #[test]
    fn pattern_aceita_decimais_inteiros_como_inteiros() {
        assert_eq!(
            pattern(Num::F64(100.0), 4).unwrap(),
            pattern(Num::Int(100), 4).unwrap()
        );
        assert!(pattern(Num::F64(1.5), 4).is_err());
    }

    #[test]
    fn pack_e_unpack_todas_as_larguras() {
        let addr = 0x0000_1234_5678_9abc_u64;
        for &w in &ALL_WIDTHS {
            let p = pack(addr, w);
            assert_eq!(unpack(p), (addr, w), "largura {}", w);
        }
    }

    #[test]
    fn pack_e_unpack_endereco_maximo() {
        let addr = ADDR_MASK;
        for &w in &ALL_WIDTHS {
            assert_eq!(unpack(pack(addr, w)), (addr, w));
        }
    }

    #[test]
    fn byte_len_nao_confunde_codigo_com_tamanho() {
        assert_eq!(byte_len(1), 1);
        assert_eq!(byte_len(2), 2);
        assert_eq!(byte_len(4), 4);
        assert_eq!(byte_len(5), 4); // float32
        assert_eq!(byte_len(6), 8); // float64
        assert_eq!(byte_len(8), 8);
    }

    #[test]
    fn comparacoes_de_inteiros() {
        let a = Num::Int(10);
        let b = Num::Int(20);
        assert!(matches_comparison(a, b, Comparison::Increased, None));
        assert!(matches_comparison(b, a, Comparison::Decreased, None));
        assert!(matches_comparison(a, b, Comparison::Changed, None));
        assert!(matches_comparison(a, a, Comparison::Unchanged, None));
        assert!(!matches_comparison(a, a, Comparison::Changed, None));
        assert!(matches_comparison(
            a,
            Num::Int(10),
            Comparison::Equal,
            Some(Num::Int(10))
        ));
        assert!(!matches_comparison(
            a,
            Num::Int(11),
            Comparison::Equal,
            Some(Num::Int(10))
        ));
    }

    #[test]
    fn comparacoes_de_floats() {
        let a = Num::F32(1.5);
        let b = Num::F32(2.5);
        assert!(matches_comparison(a, b, Comparison::Increased, None));
        assert!(matches_comparison(b, a, Comparison::Decreased, None));
        assert!(matches_comparison(a, a, Comparison::Unchanged, None));
        assert!(matches_comparison(
            a,
            Num::F32(1.5),
            Comparison::Equal,
            Some(Num::F32(1.5))
        ));
    }

    #[test]
    fn nan_nunca_aumenta_diminui_ou_iguala() {
        let nan = Num::F64(f64::NAN);
        let one = Num::F64(1.0);
        assert!(!matches_comparison(one, nan, Comparison::Increased, None));
        assert!(!matches_comparison(one, nan, Comparison::Decreased, None));
        assert!(!matches_comparison(nan, nan, Comparison::Unchanged, None));
        assert!(matches_comparison(one, nan, Comparison::Changed, None));
        assert!(!matches_comparison(nan, nan, Comparison::Equal, Some(nan)));
    }
}
