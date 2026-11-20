//! Traduções com ficheiros .po (sem dependências externas).
//!
//! A ideia é a do gettext:
//!   * o código escreve as frases em INGLÊS, dentro de `tr("...")`;
//!   * o ficheiro `po/pt.po` diz como cada frase se traduz para português;
//!   * `tr()` procura a frase na tabela do idioma atual e, se não a encontrar,
//!     devolve a própria frase em inglês. Uma tradução em falta nunca parte nada.
//!
//! Frases com valores no meio usam `{}` e `trf()`:  trf("Found: {}", &[&n])

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::{OnceLock, RwLock};

/// Idiomas disponíveis: (código, nome mostrado no menu).
pub const LANGUAGES: [(&str, &str); 7] = [
    ("en", "English"),
    ("es", "Español"),
    ("de", "Deutsch"),
    ("fr", "Français"),
    ("it", "Italiano"),
    ("pl", "Polski"),
    ("pt", "Português"),
];

/// Os ficheiros .po são embebidos no executável: não é preciso instalar nada.
/// Para acrescentar um idioma: 1) cria po/xx.po; 2) junta-o a PO_FILES;
/// 3) junta ("xx", "Nome") a LANGUAGES. O inglês não precisa de ficheiro.
const PT_PO: &str = include_str!("../po/pt.po");
const ES_PO: &str = include_str!("../po/es.po");
const DE_PO: &str = include_str!("../po/de.po");
const FR_PO: &str = include_str!("../po/fr.po");
const PL_PO: &str = include_str!("../po/pl.po");
const IT_PO: &str = include_str!("../po/it.po");

const PO_FILES: [(&str, &str); 6] = [
    ("pt", PT_PO),
    ("es", ES_PO),
    ("de", DE_PO),
    ("fr", FR_PO),
    ("pl", PL_PO),
    ("it", IT_PO),
];

struct State {
    lang: String,
    table: HashMap<String, String>,
}

/// O idioma atual é global, para qualquer parte do programa poder chamar `tr()`.
fn state() -> &'static RwLock<State> {
    static S: OnceLock<RwLock<State>> = OnceLock::new();
    S.get_or_init(|| {
        RwLock::new(State {
            lang: "en".to_string(),
            table: HashMap::new(),
        })
    })
}

/// Muda o idioma ("en" ou "pt"). Qualquer outro código fica em inglês.
pub fn set_language(code: &str) {
    let (lang, table) = match PO_FILES.iter().find(|(c, _)| *c == code) {
        Some((c, po)) => (*c, parse_po(po)),
        None => ("en", HashMap::new()), // inglês: as próprias frases do código
    };
    if let Ok(mut s) = state().write() {
        s.lang = lang.to_string();
        s.table = table;
    }
}

/// Código do idioma atual.
pub fn language() -> String {
    state()
        .read()
        .map(|s| s.lang.clone())
        .unwrap_or_else(|_| "en".to_string())
}

/// Idioma do sistema, pelas variáveis LC_ALL, LC_MESSAGES e LANG
/// ("pt_PT.UTF-8" -> "pt"). Se não for um idioma que tenhamos, "en".
pub fn system_language() -> String {
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(var) {
            if v.is_empty() {
                continue;
            }
            let code = v.split(['_', '.', '@']).next().unwrap_or("").to_lowercase();
            if LANGUAGES.iter().any(|(c, _)| *c == code) {
                return code;
            }
            return "en".to_string();
        }
    }
    "en".to_string()
}

/// Marca uma frase para tradução SEM a traduzir agora (útil em listas e constantes).
/// O script extract_po.py também a encontra; a tradução faz-se depois com tr().
pub const fn n_(s: &'static str) -> &'static str {
    s
}

/// Traduz uma frase. Sem tradução, devolve a própria frase.
pub fn tr(msgid: &str) -> String {
    if let Ok(s) = state().read()
        && let Some(t) = s.table.get(msgid)
    {
        return t.clone();
    }
    msgid.to_string()
}

/// Traduz e depois substitui cada `{}` pelos argumentos, por ordem.
pub fn trf(msgid: &str, args: &[&dyn Display]) -> String {
    let text = tr(msgid);
    let mut out = String::with_capacity(text.len() + 16);
    let mut next = args.iter();
    for (i, part) in text.split("{}").enumerate() {
        if i > 0
            && let Some(a) = next.next()
        {
            out.push_str(&a.to_string());
        }
        out.push_str(part);
    }
    out
}

// ───────────── leitura de ficheiros .po ─────────────

/// Tira as aspas e interpreta \n, \t, \" e \\.
fn unquote(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('"').unwrap_or(s);
    let s = s.strip_suffix('"').unwrap_or(s);
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(o) => {
                    out.push('\\');
                    out.push(o);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Guarda a entrada atual (se tiver frase e tradução) e limpa os acumuladores.
fn flush(map: &mut HashMap<String, String>, id: &mut String, st: &mut String) {
    if !id.is_empty() && !st.is_empty() {
        map.insert(std::mem::take(id), std::mem::take(st));
    } else {
        id.clear();
        st.clear();
    }
}

/// Lê o texto de um .po: pares `msgid "..."` / `msgstr "..."`, com frases
/// repartidas por várias linhas, comentários (#) e a entrada de cabeçalho.
fn parse_po(text: &str) -> HashMap<String, String> {
    #[derive(PartialEq)]
    enum Cur {
        None,
        Id,
        Str,
    }
    let mut map = HashMap::new();
    let mut msgid = String::new();
    let mut msgstr = String::new();
    let mut cur = Cur::None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            flush(&mut map, &mut msgid, &mut msgstr);
            cur = Cur::None;
        } else if line.starts_with('#') {
            continue;
        } else if let Some(rest) = line.strip_prefix("msgid ") {
            flush(&mut map, &mut msgid, &mut msgstr);
            msgid = unquote(rest);
            cur = Cur::Id;
        } else if let Some(rest) = line.strip_prefix("msgstr ") {
            msgstr = unquote(rest);
            cur = Cur::Str;
        } else if line.starts_with('"') {
            let piece = unquote(line);
            match cur {
                Cur::Id => msgid.push_str(&piece),
                Cur::Str => msgstr.push_str(&piece),
                Cur::None => {}
            }
        }
    }
    flush(&mut map, &mut msgid, &mut msgstr);
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_entradas_simples_e_de_varias_linhas() {
        let po = "msgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\n\
                  msgid \"Quit\"\nmsgstr \"Sair\"\n\n\
                  msgid \"Hello\\nWorld\"\nmsgstr \"\"\n\"Olá\\n\"\n\"Mundo\"\n";
        let t = parse_po(po);
        assert_eq!(t.get("Quit").map(String::as_str), Some("Sair"));
        assert_eq!(
            t.get("Hello\nWorld").map(String::as_str),
            Some("Olá\nMundo")
        );
        assert!(!t.contains_key("")); // o cabeçalho não é uma frase
    }

    #[test]
    fn frase_sem_traducao_fica_em_ingles() {
        let po = "msgid \"Quit\"\nmsgstr \"\"\n";
        assert!(parse_po(po).is_empty());
    }

    #[test]
    fn todos_os_idiomas_tem_entradas() {
        for (code, po) in PO_FILES {
            assert!(!parse_po(po).is_empty(), "po/{}.po está vazio", code);
        }
    }

    #[test]
    fn traducoes_tem_os_mesmos_marcadores() {
        // Cada {} da frase original tem de existir também na tradução, por ordem.
        for (code, po) in PO_FILES {
            for (id, st) in parse_po(po) {
                assert_eq!(
                    id.matches("{}").count(),
                    st.matches("{}").count(),
                    "[{}] número de {{}} diferente: {:?} -> {:?}",
                    code,
                    id,
                    st
                );
            }
        }
    }

    #[test]
    fn trf_substitui_pela_ordem() {
        // sem idioma carregado, tr() devolve o original
        assert_eq!(trf("{} de {}", &[&1, &2]), "1 de 2");
    }
}
